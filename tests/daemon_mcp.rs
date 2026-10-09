//! Real-process MCP socket ownership, framing, and interrupted-session checks.
use baleyg::daemon::protocol;
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::OpenOptionsExt, io::AsRawFd, net::UnixStream, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

struct Peer {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Value>,
    reaped: bool,
}
impl Peer {
    fn start(home: &Path, root: &Path) -> Self {
        let stderr = home.join(format!("stderr-{}.log", rand::random::<u64>()));
        let mut child = Command::new(env!("CARGO_BIN_EXE_baleyg"))
            .arg("mcp")
            .arg("--workspace")
            .arg(root)
            .env("HOME", home)
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("XDG_DATA_HOME", home.join("data"))
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(fs::File::create(&stderr).unwrap()))
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::sync_channel(8);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            while reader.read_until(b'\n', &mut line).unwrap_or(0) != 0 {
                assert!(line.len() <= 65_536);
                if tx.send(serde_json::from_slice(&line).unwrap()).is_err() {
                    return;
                }
                line.clear();
            }
        });
        Self {
            stdin: child.stdin.take(),
            child,
            lines,
            reaped: false,
        }
    }
    fn ask(&mut self, id: u64, method: &str, params: Value) -> Value {
        writeln!(
            self.stdin.as_mut().unwrap(),
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .unwrap();
        self.lines.recv_timeout(Duration::from_secs(10)).unwrap()
    }
    // Preserve the leader PID until its entire inherited process group is
    // stopped. A daemon started on demand inherits one of the client groups.
    fn finish_unreaped(&mut self) {
        self.stdin.take();
        let pid = self.child.id() as libc::pid_t;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            assert_eq!(rc, 0, "waitid: {}", std::io::Error::last_os_error());
            if unsafe { info.assume_init().si_pid() } == pid {
                break;
            }
            assert!(Instant::now() < deadline, "MCP client did not finish");
            std::thread::yield_now();
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn socket_under(home: &Path) -> Option<PathBuf> {
    let mut dirs = vec![home.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).ok()?;
            if meta.is_file() && path.file_name().is_some_and(|name| name == "daemon.lock") {
                return Some(baleyg::daemon::SocketPaths::new(path.parent()?.parent()?).socket);
            }
            if meta.is_dir() {
                dirs.push(path);
            }
        }
    }
    None
}
fn socket_ready(home: &Path) -> PathBuf {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(path) = socket_under(home)
            && UnixStream::connect(&path).is_ok()
        {
            return path;
        }
        assert!(Instant::now() < until, "daemon socket not ready");
        std::thread::yield_now();
    }
}
fn assert_elected(home: &Path) {
    let mut dirs = vec![home.to_path_buf()];
    let mut lock = None;
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == "daemon.lock") {
                lock = Some(path);
                break;
            }
            if path.is_dir() {
                dirs.push(path);
            }
        }
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(lock.unwrap())
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        -1
    );
}
fn checkout(home: &Path) -> PathBuf {
    let root = home.join("checkout");
    fs::create_dir_all(root.join(".git")).unwrap();
    root
}

#[test]
fn concurrent_clients_share_single_socket_and_survive_bad_private_frame() {
    let home = tempfile::tempdir().unwrap();
    let root = checkout(home.path());
    let mut first = Peer::start(home.path(), &root);
    let mut second = Peer::start(home.path(), &root);
    let socket = socket_ready(home.path());
    assert_elected(home.path());
    for (id, peer) in [(1, &mut first), (2, &mut second)] {
        let reply = peer.ask(
            id,
            "server/discover",
            json!({"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}),
        );
        assert_eq!(reply["id"], id);
        assert!(reply["result"].is_object(), "{reply}");
    }
    // Cancellation and a following request cross the same live daemon socket.
    // One write lets the daemon's bounded input queue see cancellation before
    // committing an unsent tool result.
    let metadata = json!({"_meta":{
        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
        "io.modelcontextprotocol/clientCapabilities":{}
    }});
    let sequence = [
        json!({"jsonrpc":"2.0","id":"cancelled","method":"tools/call","params":{
            "name":"baleyg_find_symbols","arguments":{"schemaVersion":1,"query":"x"},
            "_meta":metadata["_meta"]
        }}),
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"cancelled"}}),
        json!({"jsonrpc":"2.0","id":"after-cancel","method":"tools/list","params":metadata}),
    ];
    let burst = sequence.map(|request| request.to_string()).join("\n") + "\n";
    first
        .stdin
        .as_mut()
        .unwrap()
        .write_all(burst.as_bytes())
        .unwrap();
    let mut reply = first.lines.recv_timeout(Duration::from_secs(10)).unwrap();
    if reply["id"] == "cancelled" {
        assert_eq!(reply["result"]["isError"], true);
        reply = first.lines.recv_timeout(Duration::from_secs(10)).unwrap();
    }
    assert_eq!(reply["id"], "after-cancel");
    let mut invalid = UnixStream::connect(&socket).unwrap();
    invalid
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    invalid
        .write_all(&((protocol::MAX_FRAME as u32) + 1).to_be_bytes())
        .unwrap();
    let mut one = [0u8; 1];
    use std::io::Read;
    assert_eq!(invalid.read(&mut one).unwrap(), 0);
    assert_eq!(
        first.ask(
            3,
            "tools/list",
            json!({"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}})
        )["id"],
        3
    );
    first.finish_unreaped();
    assert_eq!(
        second.ask(
            4,
            "tools/list",
            json!({"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}})
        )["id"],
        4
    );
    second.finish_unreaped();
    assert_elected(home.path());
    // Both leaders remain unreaped, so their group IDs cannot be recycled.
    for peer in [&mut first, &mut second] {
        unsafe { libc::kill(-(peer.child.id() as libc::pid_t), libc::SIGKILL) };
        assert!(peer.child.wait().unwrap().success());
        peer.reaped = true;
    }
    assert!(
        !home.path().join("token").exists(),
        "MCP cannot create a browser token"
    );
}

#[test]
fn daemon_death_interrupts_session_and_next_launch_recovers() {
    let home = tempfile::tempdir().unwrap();
    let root = checkout(home.path());
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_baleyg"))
        .arg("daemon")
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let socket = socket_ready(home.path());
    let mut stale_launch = UnixStream::connect(&socket).unwrap();
    protocol::write_frame(
        &mut stale_launch,
        &protocol::Request {
            id: 33,
            operation: "mcp".into(),
            payload: json!({"workspace":root,"device":0,"inode":0}),
        },
    )
    .unwrap();
    let refused: protocol::Reply = protocol::read_frame(&mut stale_launch).unwrap();
    assert_eq!(refused.id, 33);
    assert!(
        refused.payload["error"]
            .as_str()
            .unwrap()
            .contains("root_changed")
    );
    assert!(!root.join(".git/baleyg/workspace-id").exists());
    let mut client = Peer::start(home.path(), &root);
    assert_eq!(
        client.ask(
            1,
            "server/discover",
            json!({"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}})
        )["id"],
        1
    );
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    assert!(
        client.child.try_wait().unwrap().is_none(),
        "original MCP client exited"
    );
    let recovered = client.ask(
        2,
        "server/discover",
        json!({"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}),
    );
    assert_eq!(recovered["id"], 2);
    assert!(recovered["result"].is_object(), "{recovered}");
    client.finish_unreaped();
    assert_elected(home.path());
    unsafe { libc::kill(-(client.child.id() as libc::pid_t), libc::SIGKILL) };
    assert!(client.child.wait().unwrap().success());
    client.reaped = true;
}

#[test]
fn legacy_mcp_reconnect_restores_handshake_without_new_client() {
    let home = tempfile::tempdir().unwrap();
    let root = checkout(home.path());
    let git = |cwd: &Path, args: &[&str]| {
        let output = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&root, &["init", "--quiet"]);
    git(
        &root,
        &["commit", "--allow-empty", "--quiet", "-m", "fixture"],
    );
    let linked = home.path().join("linked");
    git(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    let linked = linked.canonicalize().unwrap();
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_baleyg"))
        .arg("daemon")
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _socket = socket_ready(home.path());
    let mut peer = Peer::start(home.path(), &root);
    let init = peer.ask(
        1,
        "initialize",
        json!({
            "protocolVersion":"2025-11-25", "capabilities":{},
            "clientInfo":{"name":"regression","version":"1"}
        }),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    writeln!(
        peer.stdin.as_mut().unwrap(),
        "{}",
        json!({
            "jsonrpc":"2.0", "method":"notifications/initialized"
        })
    )
    .unwrap();
    assert_eq!(peer.ask(2, "tools/list", json!({}))["id"], 2);
    let selected = peer.ask(
        20,
        "tools/call",
        json!({
            "name":"baleyg_workspace_describe",
            "arguments":{"schemaVersion":1,"workspace":linked}
        }),
    );
    assert_eq!(
        selected["result"]["structuredContent"]["workspace"],
        json!(linked)
    );
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    assert!(peer.child.try_wait().unwrap().is_none());
    let result = peer.ask(
        3,
        "tools/call",
        json!({
            "name":"baleyg_workspace_describe",
            "arguments":{"schemaVersion":1,"workspace":linked}
        }),
    );
    assert_eq!(result["id"], 3, "{result}");
    assert_eq!(
        result["result"]["structuredContent"]["workspace"],
        json!(linked)
    );
    assert_eq!(result["result"]["structuredContent"]["requestId"], 3);
    peer.finish_unreaped();
    assert_elected(home.path());
    unsafe { libc::kill(-(peer.child.id() as libc::pid_t), libc::SIGKILL) };
    assert!(peer.child.wait().unwrap().success());
    peer.reaped = true;
}

#[test]
fn cancellation_precedes_invalid_queue_and_sixteen_follow_ons() {
    use std::os::unix::net::UnixListener;
    for count in [1usize, 16usize] {
        let home = tempfile::tempdir().unwrap();
        let root = checkout(home.path());
        let phase_path = PathBuf::from(format!(
            "/tmp/baleyg-107-mcp-{}.sock",
            rand::random::<u64>()
        ));
        let listener = UnixListener::bind(&phase_path).unwrap();
        let mut daemon = Command::new(env!("CARGO_BIN_EXE_baleyg"))
            .arg("daemon")
            .env("HOME", home.path())
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .env("XDG_DATA_HOME", home.path().join("data"))
            .env("BALEYG_TEST_MCP_PHASE", "launch_final")
            .env("BALEYG_TEST_MCP_PHASE_SOCKET", &phase_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let _ = socket_ready(home.path());
        let mut peer = Peer::start(home.path(), &root);
        let metadata = json!({"_meta":{
            "io.modelcontextprotocol/protocolVersion":"2026-07-28",
            "io.modelcontextprotocol/clientCapabilities":{}
        }});
        writeln!(
            peer.stdin.as_mut().unwrap(),
            "{}",
            json!({
                "jsonrpc":"2.0","id":"cancel-A","method":"tools/call",
                "params":{"name":"baleyg_workspace_describe",
                    "arguments":{"schemaVersion":1},"_meta":metadata["_meta"]}
            })
        )
        .unwrap();
        let (mut phase, _) = listener.accept().unwrap();
        phase
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut stage = String::new();
        BufReader::new(phase.try_clone().unwrap())
            .read_line(&mut stage)
            .unwrap();
        assert_eq!(stage, "launch_final\n");
        let cancel = "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":\"cancel-A\"}}\n";
        let mut burst = if count == 1 {
            format!("{cancel}{{bad json}}\n{}\n", "x".repeat(16_385))
        } else {
            String::new()
        };
        for n in 0..count {
            burst.push_str(
                &json!({"jsonrpc":"2.0","id":100+n,"method":"tools/list","params":metadata})
                    .to_string(),
            );
            burst.push('\n');
        }
        if count == 16 {
            burst.push_str(cancel);
        }
        peer.stdin
            .as_mut()
            .unwrap()
            .write_all(burst.as_bytes())
            .unwrap();
        phase.write_all(b"x").unwrap();
        if count == 1 {
            let invalid = peer.lines.recv_timeout(Duration::from_secs(10)).unwrap();
            assert_eq!(invalid["id"], Value::Null, "{invalid}");
            assert_eq!(invalid["error"]["code"], -32700, "{invalid}");
            let oversized = peer.lines.recv_timeout(Duration::from_secs(10)).unwrap();
            assert_eq!(oversized["id"], Value::Null, "{oversized}");
            assert_eq!(oversized["error"]["code"], -32600, "{oversized}");
        }
        for n in 0..count {
            let reply = peer.lines.recv_timeout(Duration::from_secs(10)).unwrap();
            assert_eq!(reply["id"], 100 + n, "cancelled/queued response: {reply}");
            assert!(reply["result"].is_object(), "{reply}");
        }
        assert!(
            peer.lines.recv_timeout(Duration::from_millis(200)).is_err(),
            "canceled A produced stdout"
        );
        peer.finish_unreaped();
        unsafe { libc::kill(-(peer.child.id() as libc::pid_t), libc::SIGKILL) };
        assert!(peer.child.wait().unwrap().success());
        peer.reaped = true;
        daemon.kill().unwrap();
        daemon.wait().unwrap();
        fs::remove_file(&phase_path).unwrap();
    }
}

#[test]
fn stdin_eof_cancels_in_flight_call_without_stdout() {
    use std::os::unix::net::UnixListener;
    let home = tempfile::tempdir().unwrap();
    let root = checkout(home.path());
    let phase_path = PathBuf::from(format!(
        "/tmp/baleyg-107-eof-{}.sock",
        rand::random::<u64>()
    ));
    let listener = UnixListener::bind(&phase_path).unwrap();
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_baleyg"))
        .arg("daemon")
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("BALEYG_TEST_MCP_PHASE", "launch_final")
        .env("BALEYG_TEST_MCP_PHASE_SOCKET", &phase_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _ = socket_ready(home.path());
    let mut peer = Peer::start(home.path(), &root);
    writeln!(
        peer.stdin.as_mut().unwrap(),
        "{}",
        json!({
            "jsonrpc":"2.0","id":91,"method":"tools/call",
            "params":{"name":"baleyg_workspace_describe","arguments":{"schemaVersion":1},
            "_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28",
                "io.modelcontextprotocol/clientCapabilities":{}}}
        })
    )
    .unwrap();
    let (mut phase, _) = listener.accept().unwrap();
    phase
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut stage = String::new();
    BufReader::new(phase.try_clone().unwrap())
        .read_line(&mut stage)
        .unwrap();
    assert_eq!(stage, "launch_final\n");
    peer.stdin.take();
    phase.write_all(b"x").unwrap();
    peer.finish_unreaped();
    assert!(
        peer.lines.recv_timeout(Duration::from_secs(1)).is_err(),
        "pending result after stdin EOF"
    );
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    unsafe { libc::kill(-(peer.child.id() as libc::pid_t), libc::SIGKILL) };
    assert!(peer.child.wait().unwrap().success());
    peer.reaped = true;
    fs::remove_file(&phase_path).unwrap();
}

#[test]
fn canceled_read_is_not_replayed_after_daemon_death() {
    use std::os::unix::net::UnixListener;
    let home = tempfile::tempdir().unwrap();
    let root = checkout(home.path());
    let phase_path = PathBuf::from(format!(
        "/tmp/baleyg-107-kill-{}.sock",
        rand::random::<u64>()
    ));
    let listener = UnixListener::bind(&phase_path).unwrap();
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_baleyg"))
        .arg("daemon")
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("BALEYG_TEST_MCP_PHASE", "launch_final")
        .env("BALEYG_TEST_MCP_PHASE_SOCKET", &phase_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _ = socket_ready(home.path());
    let mut peer = Peer::start(home.path(), &root);
    let metadata = json!({"_meta":{
        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
        "io.modelcontextprotocol/clientCapabilities":{}
    }});
    writeln!(
        peer.stdin.as_mut().unwrap(),
        "{}",
        json!({
            "jsonrpc":"2.0","id":"cancel-A","method":"tools/call",
            "params":{"name":"baleyg_workspace_describe",
                "arguments":{"schemaVersion":1},"_meta":metadata["_meta"]}
        })
    )
    .unwrap();
    let (phase, _) = listener.accept().unwrap();
    phase
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut stage = String::new();
    BufReader::new(phase.try_clone().unwrap())
        .read_line(&mut stage)
        .unwrap();
    assert_eq!(stage, "launch_final\n");
    writeln!(
        peer.stdin.as_mut().unwrap(),
        "{}",
        json!({
            "jsonrpc":"2.0","method":"notifications/cancelled",
            "params":{"requestId":"cancel-A"}
        })
    )
    .unwrap();
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    drop(phase);
    assert!(peer.child.try_wait().unwrap().is_none());
    let reply = peer.ask(5, "tools/list", metadata);
    assert_eq!(reply["id"], 5, "{reply}");
    assert!(reply["result"].is_object(), "{reply}");
    assert!(
        peer.lines.recv_timeout(Duration::from_millis(200)).is_err(),
        "canceled read was replayed"
    );
    peer.finish_unreaped();
    unsafe { libc::kill(-(peer.child.id() as libc::pid_t), libc::SIGKILL) };
    assert!(peer.child.wait().unwrap().success());
    peer.reaped = true;
    fs::remove_file(&phase_path).unwrap();
}

#[test]
fn idle_mcp_connection_outlives_cli_request_deadline() {
    let home = tempfile::tempdir().unwrap();
    let root = checkout(home.path());
    let mut client = Peer::start(home.path(), &root);
    let metadata = json!({"_meta":{
        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
        "io.modelcontextprotocol/clientCapabilities":{}
    }});
    assert_eq!(client.ask(1, "server/discover", metadata.clone())["id"], 1);
    // The daemon's 15-second CLI framing deadline must not close an MCP session.
    std::thread::sleep(Duration::from_secs(16));
    assert_eq!(client.ask(2, "tools/list", metadata)["id"], 2);
    client.finish_unreaped();
    unsafe { libc::kill(-(client.child.id() as libc::pid_t), libc::SIGKILL) };
    assert!(client.child.wait().unwrap().success());
    client.reaped = true;
}

#[test]
fn partial_daemon_reply_is_typed_and_never_reaches_stdout() {
    use std::io::Read;
    let home = tempfile::tempdir().unwrap();
    let root = checkout(home.path());
    // Start once to create the production private directory, then replace its
    // stale socket under the same singleton lock with a controlled socket peer.
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_baleyg"))
        .arg("daemon")
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _ = socket_ready(home.path());
    daemon.kill().unwrap();
    daemon.wait().unwrap();
    let mut dirs = vec![home.path().to_path_buf()];
    let mut lock = None;
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == "daemon.lock") {
                lock = Some(path);
                break;
            }
            if path.is_dir() {
                dirs.push(path);
            }
        }
    }
    let lock = lock.unwrap();
    let data = lock.parent().unwrap().parent().unwrap();
    let owner = baleyg::daemon::SocketOwner::acquire(&baleyg::daemon::SocketPaths::new(data))
        .unwrap()
        .expect("the killed daemon released its lock");
    let listener = owner.listener().try_clone().unwrap();
    let fake = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let attach: protocol::Request = protocol::read_frame(&mut stream).unwrap();
        assert_eq!(attach.operation, "mcp");
        protocol::write_frame(
            &mut stream,
            &protocol::Reply {
                id: attach.id,
                payload: json!({"result":true}),
            },
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap()["method"],
            "server/discover"
        );
        stream
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":")
            .unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_baleyg"))
        .arg("mcp")
        .arg("--workspace")
        .arg(&root)
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        child.stdin.as_mut().unwrap(),
        "{}",
        json!({
            "jsonrpc":"2.0", "id":1,"method":"server/discover",
            "params":{"_meta":{
                "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                "io.modelcontextprotocol/clientCapabilities":{}
            }}
        })
    )
    .unwrap();
    // Keep stdin open until the interrupted call returns a typed failure.
    // Closing it earlier must suppress that pending response entirely.
    let (tx, lines) = mpsc::sync_channel(2);
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = Vec::new();
        while reader.read_until(b'\n', &mut line).unwrap_or(0) != 0 {
            assert!(line.len() <= 65_536);
            if tx.send(std::mem::take(&mut line)).is_err() {
                return;
            }
        }
    });
    let first = lines
        .recv_timeout(Duration::from_secs(10))
        .expect("typed interrupted result while stdin open");
    let response: Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(response["id"], 1);
    assert_eq!(response["error"]["data"]["code"], "daemon_unavailable");
    let listener = owner.listener().try_clone().unwrap();
    let failed_attach = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let attach: protocol::Request = protocol::read_frame(&mut stream).unwrap();
        assert_eq!(attach.operation, "mcp");
        protocol::write_frame(
            &mut stream,
            &protocol::Reply {
                id: attach.id,
                payload: json!({"error":"root_changed"}),
            },
        )
        .unwrap();
    });
    writeln!(
        child.stdin.as_mut().unwrap(),
        "{}",
        json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"baleyg_workspace_describe",
                "arguments":{"schemaVersion":1,"workspace":"../../foreign"},
                "_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities":{}}}
        })
    )
    .unwrap();
    let selected: Value =
        serde_json::from_slice(&lines.recv_timeout(Duration::from_secs(10)).unwrap()).unwrap();
    assert_eq!(selected["id"], 2);
    assert_eq!(selected["error"]["data"]["code"], "daemon_unavailable");
    assert_eq!(
        selected["error"]["data"]["attemptedWorkspace"]["value"],
        "../../foreign"
    );
    assert!(
        selected.get("result").is_none(),
        "unverified checkout attributed: {selected}"
    );
    assert!(selected.get("workspace").is_none(), "{selected}");
    failed_attach.join().unwrap();
    child.stdin.take();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "client did not finish after stdin EOF"
        );
        std::thread::yield_now();
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .take(65_537)
        .read_to_string(&mut stderr)
        .unwrap();
    fake.join().unwrap();
    owner.listener().set_nonblocking(true).unwrap();
    assert!(
        owner
            .listener()
            .accept()
            .is_err_and(|error| error.kind() == std::io::ErrorKind::WouldBlock),
        "client reattached more than once after the controlled failure"
    );
    assert!(status.success(), "{stderr}");
    assert!(
        lines.recv_timeout(Duration::from_secs(1)).is_err(),
        "partial or duplicate stdout"
    );
}
