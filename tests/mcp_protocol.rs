//! Executable stdio contract checks. Requests wait for replies; no timing-dependent queue hooks.
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::process::{CommandExt, ExitStatusExt},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/mcp/protocol.json")).unwrap()
}
/// Single-owner waitid(WNOWAIT) observes exit without reaping. The original PID/PGID
/// cannot be recycled until *after* this owner terminates the group and reaps it.
fn bounded_group_wait(child: &mut Child) -> (std::process::ExitStatus, bool) {
    let pid = child.id() as libc::pid_t;
    // Both spawn sites use CommandExt::process_group(0); a failed group setup
    // fails spawn. Do not probe getpgid here: Darwin can transiently report
    // ESRCH before a nonblocking waitid observes a fast child's exit.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let expired = loop {
        if std::time::Instant::now() >= deadline {
            break true;
        }
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        let status = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        assert_eq!(
            status,
            0,
            "non-reaping waitid failed: {}",
            std::io::Error::last_os_error()
        );
        if unsafe { info.assume_init().si_pid() } == pid {
            // A late scheduling resume must not make an over-deadline exit valid.
            break std::time::Instant::now() >= deadline;
        }
        std::thread::yield_now();
    };
    // This owner has never reaped the leader. Even if it exited, its PID and
    // process-group identity remain reserved until child.wait() below.
    unsafe { libc::kill(-pid, libc::SIGKILL) };
    let status = child.wait().unwrap();
    (status, expired)
}
fn capture_file(path: &Path) -> Vec<u8> {
    let file = fs::File::open(path).unwrap();
    let mut bytes = Vec::new();
    file.take(65_537).read_to_end(&mut bytes).unwrap();
    assert!(
        bytes.len() <= 65_536,
        "subprocess output exceeded bounded capture"
    );
    bytes
}
/// Silence is established only after the reader has closed its sender. Timeout is
/// inconclusive and must fail, never count as an empty protocol transcript.
fn drain_stdout(lines: Receiver<Vec<u8>>) -> Vec<Vec<u8>> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut captured = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "stdout reader did not reach EOF within 10 seconds"
        );
        match lines.recv_timeout(remaining) {
            Ok(line) => {
                assert!(line.len() <= 65_536, "stdout frame exceeds response cap");
                captured.push(line);
                assert!(captured.len() <= 16, "too many stdout frames after EOF");
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return captured,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("stdout reader did not reach EOF within 10 seconds")
            }
        }
    }
}
struct Peer {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Vec<u8>>,
    stderr_path: PathBuf,
    reaped: bool,
    _home: tempfile::TempDir,
}
impl Peer {
    fn start(workspace: Option<&Path>) -> Self {
        Self::start_in(workspace, None)
    }
    fn start_in(workspace: Option<&Path>, cwd: Option<&Path>) -> Self {
        Self::start_with_phase(workspace, cwd, None)
    }
    fn start_with_phase(
        workspace: Option<&Path>,
        cwd: Option<&Path>,
        phase: Option<(&str, &Path)>,
    ) -> Self {
        let home = tempfile::tempdir().unwrap();
        let stderr_path = home.path().join("mcp-stderr.log");
        let stderr = fs::File::create(&stderr_path).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_baleyg"));
        cmd.arg("mcp")
            .current_dir(cwd.unwrap_or(home.path()))
            .env("HOME", home.path())
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .env("XDG_DATA_HOME", home.path().join("data"))
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr));
        if let Some(path) = workspace {
            cmd.arg("--workspace").arg(path);
        }
        if let Some((stage, socket)) = phase {
            cmd.env("BALEYG_TEST_MCP_PHASE", stage)
                .env("BALEYG_TEST_MCP_PHASE_SOCKET", socket);
        }
        let mut child = cmd.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::sync_channel(16);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            loop {
                let available = reader.fill_buf().unwrap();
                if available.is_empty() {
                    if !line.is_empty() {
                        let _ = tx.send(line);
                    }
                    break;
                }
                let end = available.iter().position(|&b| b == b'\n');
                let take = end.map_or(available.len(), |at| at + 1);
                if line.len() + take > 65_536 {
                    // Treat an unbounded line as a failed protocol response, never
                    // allocate an attacker-controlled amount of captured stdout.
                    let _ = tx.send(vec![0; 65_537]);
                    break;
                }
                line.extend_from_slice(&available[..take]);
                reader.consume(take);
                if end.is_some() && tx.send(std::mem::take(&mut line)).is_err() {
                    break;
                }
            }
        });
        Self {
            stdin: Some(child.stdin.take().unwrap()),
            child,
            lines: rx,
            stderr_path,
            reaped: false,
            _home: home,
        }
    }
    fn send_raw(&mut self, line: &[u8]) {
        let stdin = self.stdin.as_mut().unwrap();
        stdin.write_all(line).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }
    fn ask(&mut self, value: Value) -> Value {
        self.send_raw(serde_json::to_string(&value).unwrap().as_bytes());
        self.reply()
    }
    fn reply(&self) -> Value {
        serde_json::from_slice(&self.reply_bytes()).unwrap()
    }
    fn reply_bytes(&self) -> Vec<u8> {
        let raw = self
            .lines
            .recv_timeout(Duration::from_secs(10))
            .expect("bounded MCP reply");
        assert!(raw.len() <= 65_536);
        assert_eq!(raw.last(), Some(&b'\n'));
        assert!(
            !raw[..raw.len() - 1].contains(&b'\n'),
            "one JSON message per line"
        );
        raw
    }
    fn finish(mut self) -> (std::process::ExitStatus, String) {
        self.stdin.take();
        let (status, expired) = bounded_group_wait(&mut self.child);
        self.reaped = true;
        assert!(!expired, "MCP subprocess exceeded 10-second deadline");
        let stderr = String::from_utf8_lossy(&capture_file(&self.stderr_path)).into_owned();
        (status, stderr)
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        if !self.reaped {
            // No previous wait/reap occurred: this PID/PGID cannot be recycled.
            unsafe { libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL) };
            let _ = self.child.wait();
        }
    }
}
fn checkout(base: &Path) -> PathBuf {
    let path = base.join("checkout");
    fs::create_dir_all(path.join(".git")).unwrap();
    path
}
fn request(id: Value, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}
fn modern(id: Value, method: &str, fields: Value) -> Value {
    let mut p = fields.as_object().unwrap().clone();
    p.insert("_meta".into(), fixture()["modernMeta"].clone());
    request(id, method, Value::Object(p))
}
fn error(v: &Value, code: i64, id: Value) {
    assert_eq!(v["jsonrpc"], "2.0");
    assert_eq!(v["id"], id);
    assert_eq!(v["error"]["code"], code, "{v}");
}
fn tool(v: &Value, code: Option<&str>, modern: bool) {
    assert_eq!(v["jsonrpc"], "2.0");
    assert_eq!(v["result"]["resultType"].as_str().is_some(), modern);
    assert_eq!(v["result"]["isError"], code.is_some(), "{v}");
    let envelope = &v["result"]["structuredContent"];
    assert_eq!(
        serde_json::from_str::<Value>(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap(),
        *envelope
    );
    assert_eq!(envelope["requestId"], v["id"]);
    if let Some(code) = code {
        assert_eq!(envelope["error"]["code"], code);
    }
}
fn expected_envelope(id: &Value, code: Option<&str>, label: &str, root: &Value) -> Value {
    let fixture = fixture();
    if let Some(code) = code {
        let spec = &fixture["expected"]["failures"][code];
        assert!(spec.is_array(), "missing golden error {code}");
        let mut envelope = json!({"schemaVersion":1,"requestId":id,"error":{"code":code,
            "message":spec[0],"retryable":spec[1],"currentBasis":null,"currentContentHash":null}});
        if code != "invalid_request" && code != "range_too_large" {
            envelope["workspace"] = root.clone();
            envelope["catchingUp"] = json!(true);
        }
        envelope
    } else {
        let mut envelope = fixture["expected"]["describe"].clone();
        envelope["requestId"] = id.clone();
        envelope["data"]["workspaceLabel"] = json!(label);
        envelope["workspace"] = root.clone();
        envelope["catchingUp"] = json!(true);
        envelope
    }
}
fn exact_tool(v: &Value, code: Option<&str>, modern: bool, label: &str) {
    let root = &v["result"]["structuredContent"]["workspace"];
    let envelope = expected_envelope(&v["id"], code, label, root);
    let text = serde_json::to_string(&envelope).unwrap();
    let mut result = json!({"isError":code.is_some(),"structuredContent":envelope,
        "content":[{"type":"text","text":text}]});
    if modern {
        result["resultType"] = json!("complete");
    }
    assert_eq!(*v, json!({"jsonrpc":"2.0","id":v["id"],"result":result}));
}
fn exact_selection_failure(v: &Value, id: i64, reason: &str, attempted: &Path) {
    let envelope = json!({"schemaVersion":1,"requestId":id,"error":{
        "code":"workspace_selection_failed","message":"Workspace selection failed",
        "retryable":reason == "unavailable","reason":reason,
        "attemptedWorkspace":{"kind":"path","value":attempted,"utf8Bytes":attempted.to_str().unwrap().len(),"truncated":false},
        "currentBasis":null,"currentContentHash":null}});
    let result = json!({"resultType":"complete","isError":true,
        "structuredContent":envelope,
        "content":[{"type":"text","text":serde_json::to_string(&envelope).unwrap()}]});
    assert_eq!(*v, json!({"jsonrpc":"2.0","id":id,"result":result}));
}
fn exact_tool_for_id(
    v: &Value,
    expected_id: &Value,
    code: Option<&str>,
    modern: bool,
    label: &str,
) {
    assert_eq!(&v["id"], expected_id, "outer JSON-RPC ID must echo sent ID");
    assert_eq!(
        &v["result"]["structuredContent"]["requestId"], expected_id,
        "typed requestId must echo sent ID"
    );
    exact_tool(v, code, modern, label);
}
fn ready_legacy(peer: &mut Peer) {
    let init = peer.ask(request(
        json!(1),
        "initialize",
        fixture()["legacyInitialize"].clone(),
    ));
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    peer.send_raw(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
}
fn call(legacy: bool, id: Value, name: &str, arguments: Value) -> Value {
    let fields = json!({"name":name,"arguments":arguments});
    if legacy {
        request(id, "tools/call", fields)
    } else {
        modern(id, "tools/call", fields)
    }
}
fn phase_barrier(listener: &std::os::unix::net::UnixListener, stage: &str, mutate: impl FnOnce()) {
    listener.set_nonblocking(true).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::yield_now()
            }
            Err(error) => panic!("MCP phase {stage} was not reached: {error}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut line = String::new();
    BufReader::new(stream.try_clone().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line, format!("{stage}\n"));
    mutate();
    stream.write_all(b"x").unwrap();
}

fn no_db_under(root: &Path) {
    no_db_under_except(root, &[]);
}
fn no_db_under_except(root: &Path, allowed: &[PathBuf]) {
    fn walk(dir: &Path, allowed: &[PathBuf]) {
        assert!(
            dir.is_dir(),
            "isolated HOME must exist during DB inspection: {}",
            dir.display()
        );
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                walk(&path, allowed)
            } else {
                assert!(
                    !matches!(
                        entry.file_name().to_str(),
                        Some("index.db" | "workspace.db" | "requests.db")
                    ) || allowed.contains(&path),
                    "unexpected DB: {}",
                    path.display()
                );
            }
        }
    }
    walk(root, allowed);
}

#[test]
fn both_modes_full_catalog_tools_and_lifecycle() {
    for legacy in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let root = checkout(tmp.path());
        let mut peer = Peer::start(Some(&root));
        if legacy {
            let init = peer.ask(request(
                json!(1),
                "initialize",
                fixture()["legacyInitialize"].clone(),
            ));
            assert_eq!(
                init["result"],
                json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"baleyg","version":env!("CARGO_PKG_VERSION")}})
            );
            error(
                &peer.ask(request(json!(2), "tools/list", json!({}))),
                -32600,
                json!(2),
            );
            peer.send_raw(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        } else {
            let discover = peer.ask(modern(json!(1), "server/discover", json!({})));
            assert_eq!(
                discover["result"],
                json!({"resultType":"complete","ttlMs":0,"cacheScope":"private","supportedVersions":["2026-07-28","2025-11-25"],"capabilities":{"tools":{}},"_meta":{"io.modelcontextprotocol/serverInfo":{"name":"baleyg","version":env!("CARGO_PKG_VERSION")}}})
            );
        }
        let list = if legacy {
            request(json!("catalog"), "tools/list", json!({}))
        } else {
            modern(json!("catalog"), "tools/list", json!({}))
        };
        let response = peer.ask(list);
        let catalog: Value =
            serde_json::from_str(include_str!("fixtures/mcp/catalog.json")).unwrap();
        assert_eq!(catalog.as_array().unwrap().len(), 4);
        let expected_list = |id: Value| {
            let result = if legacy {
                json!({"tools":catalog})
            } else {
                json!({"resultType":"complete","ttlMs":0,"cacheScope":"private","tools":catalog})
            };
            json!({"jsonrpc":"2.0","id":id,"result":result})
        };
        assert_eq!(
            response,
            expected_list(json!("catalog")),
            "entire catalog wrapper must be exact"
        );
        let max_control_id = format!(
            "{}{}{}{}",
            "\"".repeat(80),
            "\\".repeat(20),
            "\u{0000}".repeat(7),
            "a".repeat(12)
        );
        assert_eq!(2 + 80 * 2 + 20 * 2 + 7 * 6 + 12, 256);
        for max_id in ["a".repeat(254), max_control_id] {
            let max_list = if legacy {
                request(json!(max_id), "tools/list", json!({}))
            } else {
                modern(json!(max_id), "tools/list", json!({}))
            };
            let max_reply = peer.ask(max_list);
            assert_eq!(
                max_reply,
                expected_list(json!(max_id)),
                "max-ID entire catalog wrapper must be exact"
            );
        }
        let names = &fixture()["names"];
        for (i, name) in names.as_array().unwrap().iter().enumerate() {
            let fields = json!({"name":name,"arguments":fixture()["validArguments"][i]});
            let call = if legacy {
                request(json!(i + 20), "tools/call", fields)
            } else {
                modern(json!(i + 20), "tools/call", fields)
            };
            let reply = peer.ask(call);
            tool(
                &reply,
                if i == 0 {
                    None
                } else {
                    Some("index_not_ready")
                },
                !legacy,
            );
            if i == 0 {
                assert_eq!(
                    reply["result"]["structuredContent"]["data"]["indexState"],
                    "unavailable"
                );
                assert_eq!(
                    reply["result"]["structuredContent"]["data"]["workspaceLabel"],
                    "checkout"
                );
            }
        }
        let control_id = format!("{}aa", "\u{0000}".repeat(42));
        let fields = json!({"name":"baleyg_workspace_describe","arguments":{"schemaVersion":1}});
        let max_call = if legacy {
            request(json!(control_id), "tools/call", fields)
        } else {
            modern(json!(control_id), "tools/call", fields)
        };
        let max_response = peer.ask(max_call);
        tool(&max_response, None, !legacy);
        assert_eq!(max_response["id"], control_id);
        let fields = json!({"name":"baleyg_find_symbols","arguments":{"schemaVersion":1,"query":"a","limit":51}});
        let reply = peer.ask(if legacy {
            request(json!(40), "tools/call", fields)
        } else {
            modern(json!(40), "tools/call", fields)
        });
        tool(&reply, Some("range_too_large"), !legacy);
        let wrong = json!({"name":"unknown","arguments":{}});
        error(
            &peer.ask(if legacy {
                request(json!(41), "tools/call", wrong)
            } else {
                modern(json!(41), "tools/call", wrong)
            }),
            -32602,
            json!(41),
        );
        let opposite = if legacy {
            modern(json!(42), "server/discover", json!({}))
        } else {
            request(
                json!(42),
                "initialize",
                fixture()["legacyInitialize"].clone(),
            )
        };
        error(&peer.ask(opposite), -32600, json!(42));
        assert!(peer.finish().0.success());
    }
}
#[test]
fn framing_ids_negotiation_and_application_validation() {
    let tmp = tempfile::tempdir().unwrap();
    let root = checkout(tmp.path());
    let mut peer = Peer::start(Some(&root));
    peer.send_raw(b"[1]");
    error(&peer.reply(), -32600, Value::Null);
    peer.send_raw(b"{not json}");
    error(&peer.reply(), -32700, Value::Null);
    peer.send_raw(&vec![b'x'; 16_384]);
    error(&peer.reply(), -32700, Value::Null);
    peer.send_raw(&vec![b'x'; 16_385]);
    error(&peer.reply(), -32600, Value::Null);
    for (unit, repeats, tail) in [
        ("a", 254, 0),
        ("\"", 127, 0),
        ("\\", 127, 0),
        ("\u{0000}", 42, 2),
        ("😀", 63, 2),
        ("é", 127, 0),
    ] {
        let good = format!("{}{}", unit.repeat(repeats), "a".repeat(tail));
        let invalid = format!("{good}a");
        peer.send_raw(
            serde_json::to_string(&modern(json!(invalid), "server/discover", json!({})))
                .unwrap()
                .as_bytes(),
        );
        error(&peer.reply(), -32600, Value::Null);
        let valid = peer.ask(modern(json!(good.clone()), "server/discover", json!({})));
        assert_eq!(valid["id"], good);
    }
    for spelling in [r#""\u00e9""#, r#""é""#, r#""\ud83d\ude00""#, r#""😀""#] {
        peer.send_raw(format!(r#"{{"jsonrpc":"2.0","id":{spelling},"method":"tools/list","params":{{"_meta":{}}}}}"#,fixture()["modernMeta"]).as_bytes());
        let v = peer.reply();
        assert_eq!(v["result"]["tools"].as_array().unwrap().len(), 4);
    }
    for n in ["1e0", "1.0", "-0", "9007199254740991", "-9007199254740991"] {
        peer.send_raw(format!(r#"{{"jsonrpc":"2.0","id":{n},"method":"server/discover","params":{{"_meta":{}}}}}"#,fixture()["modernMeta"]).as_bytes());
        let v = peer.reply();
        assert_eq!(
            v["id"],
            serde_json::from_str::<Value>(n)
                .unwrap()
                .as_i64()
                .unwrap_or(1)
        );
    }
    peer.send_raw(format!(r#"{{"jsonrpc":"2.0","id":9007199254740992,"method":"tools/list","params":{{"_meta":{}}}}}"#,fixture()["modernMeta"]).as_bytes());
    error(&peer.reply(), -32600, Value::Null);
    let mut bad = fixture()["modernMeta"].clone();
    bad["io.modelcontextprotocol/clientInfo"] = json!({"name":"x","version":"1","title":42});
    error(
        &peer.ask(request(json!(88), "tools/list", json!({"_meta":bad}))),
        -32602,
        json!(88),
    );
    let mut extension = fixture()["modernMeta"].clone();
    extension["io.modelcontextprotocol/clientInfo"] =
        json!({"name":"x","version":"1","title":"good","futureField":true});
    assert!(
        peer.ask(request(json!(89), "tools/list", json!({"_meta":extension})))["result"]["tools"]
            .is_array()
    );
    let mut version = fixture()["modernMeta"].clone();
    version["io.modelcontextprotocol/protocolVersion"] = json!("2099-01-01");
    let unsupported = peer.ask(request(json!(90), "tools/list", json!({"_meta":version})));
    error(&unsupported, -32022, json!(90));
    assert_eq!(
        unsupported["error"]["data"],
        json!({"supported":["2026-07-28","2025-11-25"],"requested":"2099-01-01"})
    );
    for (name, wrong, code) in [
        (
            "baleyg_workspace_describe",
            json!({"schemaVersion":1,"extra":0}),
            "invalid_request",
        ),
        (
            "baleyg_find_symbols",
            json!({"schemaVersion":1,"query":""}),
            "invalid_request",
        ),
        (
            "baleyg_inspect",
            json!({"schemaVersion":1,"symbolId":"sid:v1:x","view":"declaration"}),
            "invalid_request",
        ),
        (
            "baleyg_read_source",
            json!({"schemaVersion":1,"path":"../outside","startLine":1,"endLine":1}),
            "invalid_request",
        ),
        (
            "baleyg_read_source",
            json!({"schemaVersion":1,"path":"a","startLine":1,"endLine":201}),
            "range_too_large",
        ),
    ] {
        tool(
            &peer.ask(modern(
                json!(91),
                "tools/call",
                json!({"name":name,"arguments":wrong}),
            )),
            Some(code),
            true,
        );
    }
    assert!(peer.finish().0.success());
}
#[test]
fn legacy_title_recovery_max_id_and_root_selection() {
    let tmp = tempfile::tempdir().unwrap();
    let root = checkout(tmp.path());
    let mut peer = Peer::start(Some(&root));
    let mut init = fixture()["legacyInitialize"].clone();
    init["clientInfo"]["title"] = json!(42);
    error(
        &peer.ask(request(json!(7), "initialize", init)),
        -32602,
        json!(7),
    );
    let mut valid = fixture()["legacyInitialize"].clone();
    valid["_meta"] = json!({"progressToken":9007199254740992_u64});
    valid["clientInfo"]["futureField"] = json!(true);
    assert_eq!(
        peer.ask(request(json!(8), "initialize", valid))["result"]["protocolVersion"],
        "2025-11-25"
    );
    peer.send_raw(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    assert_eq!(
        peer.ask(request(
            json!(9007199254740991_i64),
            "tools/list",
            json!({})
        ))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert!(peer.finish().0.success());
    // Topology overlap is refused before marker attachment.
    let overlap = tmp.path().join("managed-home");
    fs::create_dir_all(overlap.join(".git")).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_baleyg"));
    command
        .arg("mcp")
        .arg("--workspace")
        .arg(&overlap)
        .env("HOME", &overlap)
        .env("XDG_CACHE_HOME", overlap.join("cache"))
        .env("XDG_DATA_HOME", overlap.join("data"));
    let outcome = bounded_output(command);
    assert!(!outcome.status.success());
    assert!(
        outcome.status.signal().is_none(),
        "overlap refusal must exit naturally, not from a signal"
    );
    assert!(outcome.stdout.is_empty());
    assert!(!overlap.join(".git/baleyg/workspace-id").exists());
    assert!(root.join(".git/baleyg/workspace-id").is_file());
    // A separate explicit non-Git workspace cannot attach to the daemon.
    let no_git = tmp.path().join("not-checkout");
    fs::create_dir(&no_git).unwrap();
    silent_failure(Peer::start(Some(&no_git)));
    assert!(!no_git.join(".git").exists());
}
#[test]
fn eof_and_cancellation_are_silent_for_no_pending_request() {
    let tmp = tempfile::tempdir().unwrap();
    let root = checkout(tmp.path());
    let mut peer = Peer::start(Some(&root));
    peer.send_raw(br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"nonexistent","reason":"done"}}"#);
    assert_eq!(
        peer.ask(modern(json!(1), "server/discover", json!({})))["result"]["resultType"],
        "complete"
    );
    assert!(peer.finish().0.success());
}

#[test]
fn modern_legacy_client_info_title() {
    let tmp = tempfile::tempdir().unwrap();
    let root = checkout(tmp.path());
    let mut modern_peer = Peer::start(Some(&root));
    let mut modern_bad = fixture()["modernMeta"].clone();
    modern_bad["io.modelcontextprotocol/clientInfo"] = json!({"name":"x","version":"1","title":42});
    error(
        &modern_peer.ask(request(
            json!(7),
            "server/discover",
            json!({"_meta":modern_bad.clone()}),
        )),
        -32602,
        json!(7),
    );
    modern_bad["io.modelcontextprotocol/clientInfo"] =
        json!({"name":"x","version":"1","title":"Valid","futureField":42});
    assert_eq!(
        modern_peer.ask(request(
            json!(8),
            "server/discover",
            json!({"_meta":modern_bad})
        ))["result"]["resultType"],
        "complete"
    );
    error(
        &modern_peer.ask(request(
            json!(9),
            "initialize",
            fixture()["legacyInitialize"].clone(),
        )),
        -32600,
        json!(9),
    );
    assert!(modern_peer.finish().0.success());
    let mut legacy_peer = Peer::start(Some(&root));
    let mut legacy_bad = fixture()["legacyInitialize"].clone();
    legacy_bad["clientInfo"]["title"] = json!(42);
    error(
        &legacy_peer.ask(request(json!(7), "initialize", legacy_bad.clone())),
        -32602,
        json!(7),
    );
    legacy_bad["clientInfo"]["title"] = json!("Valid");
    legacy_bad["clientInfo"]["futureField"] = json!(true);
    assert_eq!(
        legacy_peer.ask(request(json!(8), "initialize", legacy_bad))["result"]["protocolVersion"],
        "2025-11-25"
    );
    legacy_peer.send_raw(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    error(
        &legacy_peer.ask(modern(json!(9), "server/discover", json!({}))),
        -32600,
        json!(9),
    );
    assert!(legacy_peer.finish().0.success());
    for bad_modern in [true, false] {
        let mut peer = Peer::start(Some(&root));
        if bad_modern {
            let mut meta = fixture()["modernMeta"].clone();
            meta["io.modelcontextprotocol/clientInfo"] =
                json!({"name":"x","version":"1","title":42});
            error(
                &peer.ask(request(json!(1), "server/discover", json!({"_meta":meta}))),
                -32602,
                json!(1),
            );
            assert_eq!(
                peer.ask(request(
                    json!(2),
                    "initialize",
                    fixture()["legacyInitialize"].clone()
                ))["result"]["protocolVersion"],
                "2025-11-25"
            );
        } else {
            let mut args = fixture()["legacyInitialize"].clone();
            args["clientInfo"]["title"] = json!(42);
            error(
                &peer.ask(request(json!(1), "initialize", args)),
                -32602,
                json!(1),
            );
            assert_eq!(
                peer.ask(modern(json!(2), "server/discover", json!({})))["result"]["resultType"],
                "complete"
            );
        }
        assert!(peer.finish().0.success());
    }
    let implicit = Peer::start(None);
    let diagnostic = silent_failure(implicit);
    assert!(
        matches!(
            diagnostic.trim(),
            "Error: workspace root overlaps fixed topology"
                | "Error: implicit home or filesystem root refused"
        ),
        "{diagnostic}"
    );
}

fn alternate_token(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\u0022"),
            '\\' => out.push_str("\\u005c"),
            'a' => out.push('a'),
            c if (c as u32) <= 0xffff && ((c as u32) < 32 || !c.is_ascii()) => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            c if (c as u32) > 0xffff => {
                let scalar = (c as u32) - 0x10000;
                out.push_str(&format!(
                    "\\u{:04x}\\u{:04x}",
                    0xd800 + (scalar >> 10),
                    0xdc00 + (scalar & 0x3ff)
                ));
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}
fn boundary_request(id_token: &str, legacy: bool) -> Vec<u8> {
    let (method, params) = if legacy {
        ("initialize", fixture()["legacyInitialize"].clone())
    } else {
        ("server/discover", json!({"_meta":fixture()["modernMeta"]}))
    };
    let bytes =
        format!(r#"{{"jsonrpc":"2.0","id":{id_token},"method":"{method}","params":{params}}}"#)
            .into_bytes();
    assert!(bytes.len() < 16_384, "input line must stay bounded");
    bytes
}
#[test]
fn id_boundaries_binary_both_modes_and_unselected() {
    let temp = tempfile::tempdir().unwrap();
    let root = checkout(temp.path());
    let fixture = fixture();
    for row in fixture["idBoundaries"].as_array().unwrap() {
        let unit = row["unit"].as_str().unwrap();
        let repeat = row["repeats"].as_u64().unwrap() as usize;
        let tail = row["tail"].as_u64().unwrap() as usize;
        let cost = row["cost"].as_u64().unwrap() as usize;
        let admitted = format!("{}{}", unit.repeat(repeat), "a".repeat(tail));
        let rejected = format!("{admitted}a");
        assert_eq!(2 + repeat * cost + tail, 256, "{}", row["label"]);
        assert_eq!(2 + repeat * cost + tail + 1, 257, "{}", row["label"]);
        for legacy in [false, true] {
            let mut peer = Peer::start(Some(&root));
            // Both spellings must have exactly the same decoded admission and rejection.
            for invalid in [
                serde_json::to_string(&rejected).unwrap(),
                alternate_token(&rejected),
            ] {
                peer.send_raw(&boundary_request(&invalid, legacy));
                error(&peer.reply(), -32600, Value::Null);
            }
            let valid = alternate_token(&admitted);
            peer.send_raw(&boundary_request(&valid, legacy));
            let first = peer.reply();
            assert_eq!(first["id"], admitted, "{}", row["label"]);
            if legacy {
                peer.send_raw(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
            }
            for (token, expected) in [
                (
                    serde_json::to_string(&admitted).unwrap(),
                    Some(admitted.as_str()),
                ),
                (alternate_token(&admitted), Some(admitted.as_str())),
                (serde_json::to_string(&rejected).unwrap(), None),
                (alternate_token(&rejected), None),
            ] {
                let method = if legacy {
                    "tools/list"
                } else {
                    "server/discover"
                };
                let params = if legacy {
                    json!({})
                } else {
                    json!({"_meta":fixture["modernMeta"]})
                };
                let input = format!(
                    r#"{{"jsonrpc":"2.0","id":{token},"method":"{method}","params":{params}}}"#
                );
                assert!(input.len() < 16_384);
                peer.send_raw(input.as_bytes());
                let reply = peer.reply();
                if let Some(id) = expected {
                    assert_eq!(reply["id"], id, "{}", row["label"]);
                    assert!(reply["result"].is_object());
                } else {
                    error(&reply, -32600, Value::Null);
                    assert!(!reply.to_string().contains(&rejected));
                }
            }
            assert!(peer.finish().0.success());
        }
    }
    for (good, bad) in [
        ("\"".repeat(127), "\"".repeat(128)),
        ("😀".repeat(63), "😀".repeat(64)),
    ] {
        for legacy in [false, true] {
            let mut peer = Peer::start(Some(&root));
            peer.send_raw(&boundary_request(
                &serde_json::to_string(&bad).unwrap(),
                legacy,
            ));
            error(&peer.reply(), -32600, Value::Null);
            peer.send_raw(&boundary_request(&alternate_token(&good), legacy));
            assert_eq!(peer.reply()["id"], good);
            assert!(peer.finish().0.success());
        }
    }
    for legacy in [false, true] {
        let mut peer = Peer::start(Some(&root));
        if legacy {
            ready_legacy(&mut peer);
        }
        for (spelling, integer) in [
            ("1", 1),
            ("1e0", 1),
            ("1.0", 1),
            ("-0", 0),
            ("-9007199254740991", -9007199254740991_i64),
        ] {
            let params = if legacy {
                json!({})
            } else {
                json!({"_meta":fixture["modernMeta"]})
            };
            let method = if legacy {
                "tools/list"
            } else {
                "server/discover"
            };
            let line = format!(
                r#"{{"jsonrpc":"2.0","id":{spelling},"method":"{method}","params":{params}}}"#
            );
            peer.send_raw(line.as_bytes());
            assert_eq!(peer.reply()["id"], integer);
        }
        assert!(peer.finish().0.success());
    }
    // Distinct scalar spelling, decomposed Unicode, escaped literal and unescaped slash/U+2028.
    let mut peer = Peer::start(Some(&root));
    for (token, decoded) in [
        (r#""\u0061""#, "a"),
        (r#""a""#, "a"),
        (r#""é""#, "é"),
        (r#""\u00e9""#, "é"),
        (r#""e\u0301""#, "e\u{0301}"),
        (r#""/ ""#, "/ "),
        (r#""\/\u2028""#, "/ "),
    ] {
        peer.send_raw(&boundary_request(token, false));
        assert_eq!(peer.reply()["id"], decoded);
    }
    for bad in [r#""\ud83d""#, r#""\ude00""#, r#""\ud83d\u0061""#] {
        peer.send_raw(&boundary_request(bad, false));
        error(&peer.reply(), -32700, Value::Null);
    }
    assert!(peer.finish().0.success());
    // Neither rejected first request can select a mode or echo the unsafe ID.
    for initial_legacy in [false, true] {
        let mut peer = Peer::start(Some(&root));
        peer.send_raw(&boundary_request(
            &serde_json::to_string(&"a".repeat(255)).unwrap(),
            initial_legacy,
        ));
        error(&peer.reply(), -32600, Value::Null);
        let response = if initial_legacy {
            peer.ask(modern(json!(2), "server/discover", json!({})))
        } else {
            peer.ask(request(
                json!(2),
                "initialize",
                fixture["legacyInitialize"].clone(),
            ))
        };
        assert!(response["result"].is_object());
        assert!(peer.finish().0.success());
    }
}

#[test]
fn golden_tool_envelopes_and_maximum_ids_both_modes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = checkout(tmp.path());
    fs::write(root.join("src-sentinel.txt"), b"original source bytes").unwrap();
    let heavy_id = format!(
        "{}{}{}{}",
        "\"".repeat(80),
        "\\".repeat(20),
        "\u{0000}".repeat(7),
        "a".repeat(12)
    );
    assert_eq!(2 + 80 * 2 + 20 * 2 + 7 * 6 + 12, 256);
    let names = fixture()["names"].as_array().unwrap().clone();
    for legacy in [false, true] {
        let mut peer = Peer::start(Some(&root));
        if legacy {
            ready_legacy(&mut peer)
        } else {
            assert_eq!(
                peer.ask(modern(json!(1), "server/discover", json!({})))["result"]["resultType"],
                "complete"
            );
        }
        for id in [
            json!(heavy_id),
            json!("😀".repeat(63) + "aa"),
            json!(9007199254740991_i64),
            json!(-9007199254740991_i64),
        ] {
            for (index, name) in names.iter().enumerate() {
                let name = name.as_str().unwrap();
                let bad = json!({"schemaVersion":1,"unknown":"rejected"});
                let invalid = peer.ask(call(legacy, id.clone(), name, bad));
                exact_tool_for_id(&invalid, &id, Some("invalid_request"), !legacy, "checkout");
                let args = fixture()["validArguments"][index].clone();
                let accepted = peer.ask(call(legacy, id.clone(), name, args));
                exact_tool_for_id(
                    &accepted,
                    &id,
                    if index == 0 {
                        None
                    } else {
                        Some("index_not_ready")
                    },
                    !legacy,
                    "checkout",
                );
            }
        }
        for name in &names {
            let name = name.as_str().unwrap();
            for args in [json!([]), json!(42), Value::Null] {
                exact_tool_for_id(
                    &peer.ask(call(legacy, json!(heavy_id), name, args)),
                    &json!(heavy_id),
                    Some("invalid_request"),
                    !legacy,
                    "checkout",
                );
            }
        }
        let pin = json!({"indexGeneration":"123e4567-e89b-42d3-a456-426614174000","indexRevision":9007199254740991_u64});
        for (name, args) in [
            (
                "baleyg_find_symbols",
                json!({"schemaVersion":1,"query":"x","expectedBasis":pin}),
            ),
            (
                "baleyg_inspect",
                json!({"schemaVersion":1,"symbolId":format!("sid:v1:{}","a".repeat(32)),"view":"outgoing_calls","limit":50,"expectedBasis":pin}),
            ),
            (
                "baleyg_read_source",
                json!({"schemaVersion":1,"path":"src-sentinel.txt","startLine":1,"endLine":200,"expectedBasis":pin,"expectedContentHash":"a".repeat(64)}),
            ),
        ] {
            exact_tool_for_id(
                &peer.ask(call(legacy, json!(heavy_id), name, args)),
                &json!(heavy_id),
                Some("index_not_ready"),
                !legacy,
                "checkout",
            );
        }
        let mut wrong = fixture()["validArguments"][1].clone();
        wrong["limit"] = json!(51);
        exact_tool_for_id(
            &peer.ask(call(legacy, json!(heavy_id), "baleyg_find_symbols", wrong)),
            &json!(heavy_id),
            Some("range_too_large"),
            !legacy,
            "checkout",
        );
        for fields in [
            json!({"name":"baleyg_workspace_describe","arguments":{},"unexpected":0}),
            json!({"name":"not_a_tool","arguments":{}}),
            json!({"arguments":{}}),
        ] {
            let response = peer.ask(if legacy {
                request(json!(heavy_id), "tools/call", fields)
            } else {
                modern(json!(heavy_id), "tools/call", fields)
            });
            error(&response, -32602, json!(heavy_id));
        }
        no_db_under(peer._home.path());
        assert!(peer.finish().0.success());
    }
    assert_eq!(
        fs::read(root.join("src-sentinel.txt")).unwrap(),
        b"original source bytes"
    );
    fs::write(root.join("src-sentinel.txt"), b"changed source contents").unwrap();
    let mut peer = Peer::start(Some(&root));
    let args = json!({"schemaVersion":1,"path":"src-sentinel.txt","startLine":1,"endLine":1,"expectedContentHash":"a".repeat(64)});
    exact_tool_for_id(
        &peer.ask(call(false, json!(heavy_id), "baleyg_read_source", args)),
        &json!(heavy_id),
        Some("index_not_ready"),
        true,
        "checkout",
    );
    assert!(peer.finish().0.success());
}

fn bounded_output(mut command: Command) -> std::process::Output {
    let capture = tempfile::tempdir().unwrap();
    let stdout_path = capture.path().join("stdout.log");
    let stderr_path = capture.path().join("stderr.log");
    let stdout = fs::File::create(&stdout_path).unwrap();
    let stderr = fs::File::create(&stderr_path).unwrap();
    let mut child = command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    let (status, expired) = bounded_group_wait(&mut child);
    assert!(!expired, "subprocess exceeded 10-second deadline");
    std::process::Output {
        status,
        stdout: capture_file(&stdout_path),
        stderr: capture_file(&stderr_path),
    }
}
fn silent_failure(mut peer: Peer) -> String {
    peer.stdin.take();
    let (_tx, substitute) = mpsc::channel();
    let lines = std::mem::replace(&mut peer.lines, substitute);
    let (status, stderr) = peer.finish();
    assert!(!status.success(), "expected rejected workspace");
    assert!(
        status.signal().is_none(),
        "startup refusal must exit naturally, not from a signal"
    );
    assert!(!stderr.is_empty(), "diagnostics belong on stderr");
    assert!(
        drain_stdout(lines).is_empty(),
        "startup failure emitted stdout"
    );
    stderr
}
#[test]
fn workspace_selection_startup_side_effects_and_final_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let ancestor = tmp.path().join("ancestor");
    fs::create_dir_all(ancestor.join(".git")).unwrap();
    let nearer = ancestor.join("nearer");
    fs::create_dir_all(nearer.join(".git")).unwrap();
    let nested = nearer.join("deep");
    fs::create_dir(&nested).unwrap();
    // Explicit root wins even if cwd has a nearer Git marker.
    let mut explicit = Peer::start_in(Some(&ancestor), Some(&nested));
    exact_tool(
        &explicit.ask(modern(
            json!(1),
            "tools/call",
            json!({"name":"baleyg_workspace_describe","arguments":{"schemaVersion":1}}),
        )),
        None,
        true,
        "ancestor",
    );
    let ancestor_marker = ancestor.join(".git/baleyg/workspace-id");
    let first_marker = fs::read(&ancestor_marker).unwrap();
    assert!(!nearer.join(".git/baleyg/workspace-id").exists());
    no_db_under(explicit._home.path());
    assert!(explicit.finish().0.success());
    // Adopt the same marker on restart; never regenerate a valid existing UUID.
    let mut adopted = Peer::start(Some(&ancestor));
    exact_tool(
        &adopted.ask(modern(
            json!(2),
            "tools/call",
            json!({"name":"baleyg_workspace_describe","arguments":{"schemaVersion":1}}),
        )),
        None,
        true,
        "ancestor",
    );
    assert!(adopted.finish().0.success());
    assert_eq!(fs::read(&ancestor_marker).unwrap(), first_marker);
    // Implicit selection walks to the closest Git directory, not a farther ancestor.
    let mut implicit = Peer::start_in(None, Some(&nested));
    exact_tool(
        &implicit.ask(modern(
            json!(3),
            "tools/call",
            json!({"name":"baleyg_workspace_describe","arguments":{"schemaVersion":1}}),
        )),
        None,
        true,
        "nearer",
    );
    let nearer_marker = fs::read(nearer.join(".git/baleyg/workspace-id")).unwrap();
    assert_ne!(first_marker, nearer_marker);
    assert!(implicit.finish().0.success());
    // A non-Git cwd cannot acquire a daemon checkout attachment or marker.
    let plain = tmp.path().join("plain");
    fs::create_dir(&plain).unwrap();
    silent_failure(Peer::start_in(None, Some(&plain)));
    assert!(!plain.join(".git").exists());
    // A nearer malformed .git is not silently skipped for ancestor fallback.
    let bad = ancestor.join("bad-nearer");
    fs::create_dir(&bad).unwrap();
    fs::write(bad.join(".git"), "not a valid gitdir pointer").unwrap();
    silent_failure(Peer::start_in(None, Some(&bad)));
    // Final symlinks are refused rather than canonicalized into a different root.
    let linked = tmp.path().join("symlink-root");
    std::os::unix::fs::symlink(&ancestor, &linked).unwrap();
    silent_failure(Peer::start(Some(&linked)));
    silent_failure(Peer::start_in(None, Some(Path::new("/"))));
    // The selected root identity is rechecked for every valid known-tool outcome.
    let mut marker_lost = Peer::start(Some(&ancestor));
    assert!(marker_lost.ask(modern(json!(40), "server/discover", json!({})))["result"].is_object());
    fs::remove_file(&ancestor_marker).unwrap();
    exact_selection_failure(
        &marker_lost.ask(call(
            false,
            json!(5),
            "baleyg_workspace_describe",
            json!({"schemaVersion":1}),
        )),
        5,
        "identity_changed",
        &ancestor.canonicalize().unwrap(),
    );
    exact_selection_failure(
        &marker_lost.ask(call(
            false,
            json!(6),
            "baleyg_workspace_describe",
            json!({"schemaVersion":1,"workspace":"escape"}),
        )),
        6,
        "identity_changed",
        Path::new("escape"),
    );
    no_db_under(marker_lost._home.path());
    assert!(marker_lost.finish().0.success());
    let mut root_changed = Peer::start(Some(&nearer));
    assert!(
        root_changed.ask(modern(json!(41), "server/discover", json!({})))["result"].is_object()
    );
    let moved = ancestor.join("moved-nearer");
    fs::rename(&nearer, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &nearer).unwrap();
    exact_selection_failure(
        &root_changed.ask(call(
            false,
            json!(7),
            "baleyg_find_symbols",
            json!({"schemaVersion":1,"query":"a"}),
        )),
        7,
        "identity_changed",
        &ancestor.canonicalize().unwrap().join("nearer"),
    );
    assert!(root_changed.finish().0.success());
}

#[test]
fn exact_frames_split_utf8_crlf_and_eof() {
    let tmp = tempfile::tempdir().unwrap();
    let root = checkout(tmp.path());
    let mut peer = Peer::start(Some(&root));
    let mut req = modern(json!(50), "server/discover", json!({}));
    req["params"]["_meta"]["padding"] = json!("");
    let empty = serde_json::to_vec(&req).unwrap();
    let padding = "a".repeat(16_384 - empty.len());
    req["params"]["_meta"]["padding"] = json!(padding);
    let raw = serde_json::to_vec(&req).unwrap();
    assert_eq!(raw.len(), 16_384);
    peer.send_raw(&raw);
    assert_eq!(peer.reply()["id"], 50);
    let mut oversized = raw.clone();
    oversized.push(b'\r');
    peer.send_raw(&oversized);
    error(&peer.reply(), -32600, Value::Null);
    // Exactly 16,383 JSON bytes plus CR are 16,384 bytes before LF: admitted.
    req["params"]["_meta"]["padding"] = json!("a".repeat(16_383 - empty.len()));
    let mut exact_crlf = serde_json::to_vec(&req).unwrap();
    assert_eq!(exact_crlf.len(), 16_383);
    exact_crlf.push(b'\r');
    peer.send_raw(&exact_crlf);
    assert_eq!(peer.reply()["id"], 50);
    let mut invalid_utf8 = br#"{"jsonrpc":"2.0","id":"x","method":"server/discover"}"#.to_vec();
    let marker = invalid_utf8.windows(3).position(|p| p == b"\"x\"").unwrap();
    invalid_utf8[marker + 1] = 0xff;
    peer.send_raw(&invalid_utf8);
    error(&peer.reply(), -32700, Value::Null);
    assert_eq!(
        peer.ask(modern(json!(53), "server/discover", json!({})))["id"],
        53,
        "bad UTF-8 must not poison the next frame"
    );
    let mut crlf = serde_json::to_vec(&modern(json!(51), "server/discover", json!({}))).unwrap();
    crlf.push(b'\r');
    peer.send_raw(&crlf);
    assert_eq!(peer.reply()["id"], 51);
    let mut unicode =
        serde_json::to_vec(&modern(json!("é😀"), "server/discover", json!({}))).unwrap();
    let at = unicode
        .windows(4)
        .position(|bytes| bytes == "😀".as_bytes())
        .unwrap();
    let tail = unicode.split_off(at + 1);
    peer.stdin.as_mut().unwrap().write_all(&unicode).unwrap();
    peer.stdin.as_mut().unwrap().flush().unwrap();
    peer.send_raw(&tail);
    assert_eq!(peer.reply()["id"], "é😀");
    assert!(peer.finish().0.success());
    let mut incomplete = Peer::start(Some(&root));
    incomplete
        .stdin
        .as_mut()
        .unwrap()
        .write_all(
            fixture()["transcripts"]["incompleteEof"]["input"]
                .as_str()
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
    let (_tx, substitute) = mpsc::channel();
    let lines = std::mem::replace(&mut incomplete.lines, substitute);
    assert!(incomplete.finish().0.success());
    assert!(
        drain_stdout(lines).is_empty(),
        "unterminated input cannot be dispatched"
    );
    // A completed request remains silent after its cancellation and at EOF.
    let mut completed = Peer::start(Some(&root));
    assert_eq!(
        completed.ask(modern(json!(52), "server/discover", json!({})))["id"],
        52
    );
    let mut cancelled = fixture()["transcripts"]["completedCancellation"]["notification"].clone();
    cancelled["params"]["requestId"] = json!(52);
    completed.send_raw(serde_json::to_string(&cancelled).unwrap().as_bytes());
    let (_tx, substitute) = mpsc::channel();
    let lines = std::mem::replace(&mut completed.lines, substitute);
    assert!(completed.finish().0.success());
    assert!(
        drain_stdout(lines).is_empty(),
        "cancelled completed request/EOF emitted a response"
    );
}

#[test]
fn unusable_index_sentinels_do_not_change_unavailable_projection_or_get_repaired() {
    use sha2::{Digest, Sha256};
    let tmp = tempfile::tempdir().unwrap();
    let root = checkout(tmp.path());
    let root_key = hex::encode(Sha256::digest(
        fs::canonicalize(&root)
            .unwrap()
            .to_str()
            .unwrap()
            .as_bytes(),
    ));
    let mut peer = Peer::start(Some(&root));
    assert_eq!(
        peer.ask(modern(json!(1), "server/discover", json!({})))["result"]["resultType"],
        "complete"
    );
    let home = peer._home.path();
    let caches = [
        home.join("Library/Caches/dev.odin.baleyg"),
        home.join("cache/baleyg"),
        home.join("cache/dev/odin/baleyg"),
    ];
    let mut sentinels = Vec::new();
    for cache in caches {
        let index = cache.join("indexes").join(&root_key).join("index.db");
        fs::create_dir_all(index.parent().unwrap()).unwrap();
        fs::write(&index, b"unusable-index-DO-NOT-OPEN").unwrap();
        sentinels.push(index);
    }
    for name in [
        "baleyg_find_symbols",
        "baleyg_inspect",
        "baleyg_read_source",
    ] {
        let i = fixture()["names"]
            .as_array()
            .unwrap()
            .iter()
            .position(|v| v == name)
            .unwrap();
        let response = peer.ask(call(
            false,
            json!(2),
            name,
            fixture()["validArguments"][i].clone(),
        ));
        exact_tool(&response, Some("index_not_ready"), true, "checkout");
    }
    no_db_under_except(peer._home.path(), &sentinels);
    for index in sentinels {
        assert_eq!(fs::read(&index).unwrap(), b"unusable-index-DO-NOT-OPEN");
        assert!(!index.with_extension("db-wal").exists());
        assert!(!index.with_extension("db-shm").exists());
    }
    assert!(!root.join(".git/baleyg/requests.db").exists());
    assert!(peer.finish().0.success());
}

#[test]
fn golden_modern_legacy_transcripts() {
    let tmp = tempfile::tempdir().unwrap();
    let root = checkout(tmp.path());
    for mode in ["modern", "legacy"] {
        let fixture = fixture();
        let transcript = &fixture["transcripts"][mode];
        let mut peer = Peer::start(Some(&root));
        let mut expected = transcript["response"].clone();
        if mode == "modern" {
            expected["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["version"] =
                json!(env!("CARGO_PKG_VERSION"));
        } else {
            expected["result"]["serverInfo"]["version"] = json!(env!("CARGO_PKG_VERSION"));
        }
        assert_eq!(peer.ask(transcript["request"].clone()), expected);
        if mode == "legacy" {
            peer.send_raw(
                serde_json::to_string(&transcript["initialized"])
                    .unwrap()
                    .as_bytes(),
            );
        }
        let response = peer.ask(call(
            mode == "legacy",
            json!("golden-describe"),
            "baleyg_workspace_describe",
            json!({"schemaVersion":1}),
        ));
        exact_tool(&response, None, mode == "modern", "checkout");
        let response = peer.ask(call(
            mode == "legacy",
            json!("golden-evidence"),
            "baleyg_find_symbols",
            json!({"schemaVersion":1,"query":"x"}),
        ));
        exact_tool(
            &response,
            Some("index_not_ready"),
            mode == "modern",
            "checkout",
        );
        assert!(peer.finish().0.success());
    }
}

#[test]
fn ordinary_cancellation_and_eof_without_private_race_hook() {
    let tmp = tempfile::tempdir().unwrap();
    let root = checkout(tmp.path());
    for legacy in [false, true] {
        let mut peer = Peer::start(Some(&root));
        if legacy {
            ready_legacy(&mut peer)
        }
        let active = call(
            legacy,
            json!("cancel-me"),
            "baleyg_find_symbols",
            json!({"schemaVersion":1,"query":"a"}),
        );
        let cancelled = json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"cancel-me","reason":"client no longer needs response"}});
        let sentinel = if legacy {
            request(json!("after-cancel"), "tools/list", json!({}))
        } else {
            modern(json!("after-cancel"), "server/discover", json!({}))
        };
        // One write gives the reader an ordinary input burst, not a barrier between
        // prepare and commit. The old reply may already be committed before cancellation.
        let burst = format!("{}\n{}\n{}\n", active, cancelled, sentinel);
        peer.stdin
            .as_mut()
            .unwrap()
            .write_all(burst.as_bytes())
            .unwrap();
        peer.stdin.as_mut().unwrap().flush().unwrap();
        let mut reply = peer.reply();
        if reply["id"] == "cancel-me" {
            exact_tool(&reply, Some("index_not_ready"), !legacy, "checkout");
            reply = peer.reply();
        }
        assert_eq!(reply["id"], "after-cancel");
        assert!(reply["result"].is_object());
        let (_tx, substitute) = mpsc::channel();
        let lines = std::mem::replace(&mut peer.lines, substitute);
        assert!(peer.finish().0.success());
        assert!(
            drain_stdout(lines).is_empty(),
            "cancellation/EOF emitted an extra response"
        );
    }
    // EOF during an ordinary request can race a completed response. It must
    // terminate without emitting a partial frame or any response after shutdown.
    let mut peer = Peer::start(Some(&root));
    let active = modern(json!("eof-request"), "tools/list", json!({}));
    peer.send_raw(serde_json::to_string(&active).unwrap().as_bytes());
    let (_tx, substitute) = mpsc::channel();
    let lines = std::mem::replace(&mut peer.lines, substitute);
    assert!(peer.finish().0.success());
    let all = drain_stdout(lines);
    assert!(all.len() <= 1, "EOF cannot cause duplicate outcomes");
    for line in all {
        assert!(line.len() <= 65_536);
        assert_eq!(
            line.last(),
            Some(&b'\n'),
            "EOF response must be a complete line"
        );
        let value: Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(value["id"], "eof-request");
    }
}

#[test]
fn linked_git_worktrees_get_distinct_selected_markers() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("main-checkout");
    fn git(cwd: &Path, home: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        command
            .current_dir(cwd)
            .args(args)
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", Path::new("/dev/null"))
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid");
        let result = bounded_output(command);
        assert!(
            result.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    git(
        tmp.path(),
        tmp.path(),
        &["init", "--quiet", repo.to_str().unwrap()],
    );
    git(
        &repo,
        tmp.path(),
        &["commit", "--allow-empty", "--quiet", "-m", "fixture root"],
    );
    let one = tmp.path().join("linked-one");
    let two = tmp.path().join("linked-two");
    for path in [&one, &two] {
        git(
            &repo,
            tmp.path(),
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                path.to_str().unwrap(),
                "HEAD",
            ],
        );
    }
    fn gitdir(root: &Path) -> PathBuf {
        let pointer = fs::read_to_string(root.join(".git")).unwrap();
        assert!(pointer.starts_with("gitdir: "));
        let relative = pointer.trim_end().strip_prefix("gitdir: ").unwrap();
        fs::canonicalize(root.join(relative)).unwrap()
    }
    let git_one = gitdir(&one);
    let git_two = gitdir(&two);
    assert_ne!(
        git_one, git_two,
        "different linked worktrees need distinct Git dirs"
    );
    let mut ids = Vec::new();
    for (root, label, git) in [
        (&one, "linked-one", &git_one),
        (&two, "linked-two", &git_two),
    ] {
        let mut peer = Peer::start(Some(root));
        let result = peer.ask(call(
            false,
            json!(1),
            "baleyg_workspace_describe",
            json!({"schemaVersion":1}),
        ));
        exact_tool_for_id(&result, &json!(1), None, true, label);
        let marker = git.join("baleyg/workspace-id");
        ids.push(fs::read(&marker).unwrap());
        assert!(
            !root.join(".git/baleyg/workspace-id").exists(),
            "pointer file must not be treated as directory"
        );
        no_db_under(peer._home.path());
        assert!(peer.finish().0.success());
        let mut reopened = Peer::start_in(None, Some(root));
        let result = reopened.ask(call(
            false,
            json!(2),
            "baleyg_workspace_describe",
            json!({"schemaVersion":1}),
        ));
        exact_tool_for_id(&result, &json!(2), None, true, label);
        assert!(reopened.finish().0.success());
        assert_eq!(
            fs::read(&marker).unwrap(),
            *ids.last().unwrap(),
            "restart must adopt selected pointer marker"
        );
    }
    assert_ne!(
        ids[0], ids[1],
        "two real linked worktrees must not share a workspace UUID marker"
    );
}

#[test]
fn per_call_linked_worktree_selection_and_closed_failures() {
    assert_eq!(
        fixture()["workspaceSelection"],
        json!({
        "inputField":"workspace","maxUtf8Bytes":4096,"attemptedValueBytes":256,
        "resolvedFields":["workspace","catchingUp"],
        "reasons":["not_absolute","not_checkout","different_repository","identity_changed","unavailable"]})
    );
    fn git(cwd: &Path, args: &[&str]) {
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
    }
    let temp = tempfile::tempdir().unwrap();
    let main = temp.path().join("main");
    git(temp.path(), &["init", "--quiet", main.to_str().unwrap()]);
    git(
        &main,
        &["commit", "--allow-empty", "--quiet", "-m", "fixture"],
    );
    let linked = temp.path().join("linked");
    git(
        &main,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    let foreign = temp.path().join("foreign");
    git(temp.path(), &["init", "--quiet", foreign.to_str().unwrap()]);
    let main = main.canonicalize().unwrap();
    let linked = linked.canonicalize().unwrap();
    let foreign = foreign.canonicalize().unwrap();
    let foreign_marker = foreign.join(".git/baleyg/workspace-id");
    let git_pointer = fs::read_to_string(linked.join(".git")).unwrap();
    let linked_git =
        fs::canonicalize(linked.join(git_pointer.trim().strip_prefix("gitdir: ").unwrap()))
            .unwrap();
    let linked_marker = linked_git.join("baleyg/workspace-id");
    for legacy in [false, true] {
        let mut peer = Peer::start(Some(&main));
        if legacy {
            ready_legacy(&mut peer);
        }
        let describe = |peer: &mut Peer, id: i64, args: Value| {
            peer.ask(call(legacy, json!(id), "baleyg_workspace_describe", args))
        };
        let selected = describe(&mut peer, 10, json!({"schemaVersion":1,"workspace":linked}));
        tool(&selected, None, !legacy);
        assert_eq!(
            selected["result"]["structuredContent"]["workspace"],
            json!(linked)
        );
        assert_eq!(selected["result"]["structuredContent"]["catchingUp"], true);
        assert_eq!(
            selected["result"]["structuredContent"]["data"]["workspaceLabel"],
            "linked"
        );
        assert!(linked_marker.exists());
        for (offset, name) in fixture()["names"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .skip(1)
        {
            let mut arguments = fixture()["validArguments"][offset].clone();
            arguments["workspace"] = json!(linked);
            let answer = peer.ask(call(
                legacy,
                json!(30 + offset),
                name.as_str().unwrap(),
                arguments,
            ));
            tool(&answer, Some("index_not_ready"), !legacy);
            assert_eq!(
                answer["result"]["structuredContent"]["workspace"],
                json!(linked)
            );
            assert_eq!(answer["result"]["structuredContent"]["catchingUp"], true);
        }
        let launch = describe(&mut peer, 11, json!({"schemaVersion":1}));
        tool(&launch, None, !legacy);
        assert_eq!(
            launch["result"]["structuredContent"]["workspace"],
            json!(main)
        );
        assert_eq!(
            launch["result"]["structuredContent"]["data"]["workspaceLabel"],
            "main"
        );
        for (id, value, reason) in [
            (12, json!("relative"), "not_absolute"),
            (13, json!(foreign), "different_repository"),
        ] {
            let answer = describe(&mut peer, id, json!({"schemaVersion":1,"workspace":value}));
            tool(&answer, Some("workspace_selection_failed"), !legacy);
            let envelope = &answer["result"]["structuredContent"];
            assert!(envelope.get("workspace").is_none());
            assert!(envelope.get("catchingUp").is_none());
            assert_eq!(envelope["error"]["reason"], reason);
            assert_eq!(envelope["error"]["attemptedWorkspace"]["value"], value);
            assert_eq!(envelope["error"]["currentBasis"], Value::Null);
        }
        assert!(
            !foreign_marker.exists(),
            "foreign selection cannot create a marker"
        );
        let symlink = temp.path().join("link-to-linked");
        if !symlink.exists() {
            std::os::unix::fs::symlink(&linked, &symlink).unwrap();
        }
        let answer = describe(
            &mut peer,
            14,
            json!({"schemaVersion":1,"workspace":symlink}),
        );
        tool(&answer, Some("workspace_selection_failed"), !legacy);
        assert_eq!(
            answer["result"]["structuredContent"]["error"]["reason"],
            "not_checkout"
        );
        let invalid = describe(&mut peer, 15, json!({"schemaVersion":1,"workspace":[]}));
        tool(&invalid, Some("invalid_request"), !legacy);
        assert_eq!(
            invalid["result"]["structuredContent"]["error"]["attemptedWorkspace"],
            json!({"kind":"invalid_type","jsonType":"array"})
        );
        assert!(
            invalid["result"]["structuredContent"]
                .get("workspace")
                .is_none()
        );
        let long = format!("{}😀", "é".repeat(2047));
        let invalid = describe(&mut peer, 16, json!({"schemaVersion":1,"workspace":long}));
        tool(&invalid, Some("invalid_request"), !legacy);
        let attempt = &invalid["result"]["structuredContent"]["error"]["attemptedWorkspace"];
        assert_eq!(attempt["utf8Bytes"], 4098);
        assert_eq!(attempt["value"].as_str().unwrap().len(), 256);
        assert_eq!(attempt["truncated"], true);
        assert_eq!(
            describe(&mut peer, 17, json!({"schemaVersion":1}))["result"]["structuredContent"]["workspace"],
            json!(main)
        );
        no_db_under(peer._home.path());
        assert!(peer.finish().0.success());
    }
}

#[test]
fn catalog_rejects_contradictory_unresolved_and_resolved_failures() {
    let catalog: Value = serde_json::from_str(include_str!("fixtures/mcp/catalog.json")).unwrap();
    assert_eq!(catalog.as_array().unwrap().len(), 4);
    for entry in catalog.as_array().unwrap() {
        let defs = &entry["outputSchema"]["$defs"];
        assert_eq!(
            defs["Failure"]["properties"]["error"]["properties"]["code"],
            json!({"$ref":"#/$defs/ResolvedFailureCode"})
        );
        assert!(
            !defs["ResolvedFailureCode"]["enum"]
                .as_array()
                .unwrap()
                .contains(&json!("workspace_selection_failed"))
        );
        for kind in ["SelectionFailure", "InvalidWorkspaceFailure"] {
            let error = &defs[kind]["properties"]["error"]["properties"];
            assert_eq!(error["currentBasis"], json!({"const":null}));
            assert_eq!(error["currentContentHash"], json!({"const":null}));
        }
        assert_eq!(
            defs["InvalidWorkspaceFailure"]["properties"]["error"]["properties"]["retryable"],
            json!({"const":false})
        );
        assert_eq!(
            defs["SelectionFailure"]["properties"]["error"]["allOf"][0]["then"]["properties"]["retryable"],
            json!({"const":true})
        );
        assert_eq!(
            defs["SelectionFailure"]["properties"]["error"]["allOf"][0]["else"]["properties"]["retryable"],
            json!({"const":false})
        );
    }
}

fn git_selection(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(dir)
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
}
fn git_linked_fixture(temp: &Path) -> (PathBuf, PathBuf) {
    let main = temp.join("main");
    git_selection(temp, &["init", "--quiet", main.to_str().unwrap()]);
    git_selection(
        &main,
        &["commit", "--allow-empty", "--quiet", "-m", "fixture"],
    );
    let linked = temp.join("linked");
    git_selection(
        &main,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    (main.canonicalize().unwrap(), linked.canonicalize().unwrap())
}
fn linked_marker(linked: &Path) -> PathBuf {
    let pointer = fs::read_to_string(linked.join(".git")).unwrap();
    fs::canonicalize(linked.join(pointer.trim().strip_prefix("gitdir: ").unwrap()))
        .unwrap()
        .join("baleyg/workspace-id")
}

#[test]
fn selected_clone_failure_is_resolved_and_does_not_kill_the_session() {
    for legacy in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (main, linked) = git_linked_fixture(temp.path());
        let socket = temp.path().join("phase.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let mut peer = Peer::start_with_phase(Some(&main), None, Some(("selected_clone", &socket)));
        if legacy {
            ready_legacy(&mut peer);
        }
        peer.send_raw(
            serde_json::to_string(&call(
                legacy,
                json!(8),
                "baleyg_workspace_describe",
                json!({"schemaVersion":1,"workspace":linked}),
            ))
            .unwrap()
            .as_bytes(),
        );
        let marker = linked_marker(&linked);
        phase_barrier(&listener, "selected_clone", || {
            fs::remove_file(&marker).unwrap()
        });
        let reply = peer.reply();
        tool(&reply, Some("root_changed"), !legacy);
        assert_eq!(
            reply["result"]["structuredContent"]["workspace"],
            json!(linked)
        );
        assert_eq!(reply["result"]["structuredContent"]["catchingUp"], true);
        assert_eq!(
            reply["result"]["structuredContent"]["error"]["currentBasis"],
            Value::Null
        );
        assert!(
            peer.finish().0.success(),
            "daemon panic cannot interrupt thin client"
        );
    }
}

#[test]
fn final_resolved_drift_is_attributed_for_four_tools_and_both_modes() {
    for legacy in [false, true] {
        for (index, name) in fixture()["names"].as_array().unwrap().iter().enumerate() {
            for explicit in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let (main, linked) = git_linked_fixture(temp.path());
                let root = if explicit { &linked } else { &main };
                let stage = if explicit {
                    "selected_final"
                } else {
                    "launch_final"
                };
                let socket = temp.path().join("phase.sock");
                let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
                let mut peer = Peer::start_with_phase(Some(&main), None, Some((stage, &socket)));
                if legacy {
                    ready_legacy(&mut peer);
                }
                let mut args = fixture()["validArguments"][index].clone();
                if explicit {
                    args["workspace"] = json!(linked);
                }
                peer.send_raw(
                    serde_json::to_string(&call(
                        legacy,
                        json!(index),
                        name.as_str().unwrap(),
                        args,
                    ))
                    .unwrap()
                    .as_bytes(),
                );
                if index % 2 == 0 {
                    let marker = if explicit {
                        linked_marker(root)
                    } else {
                        root.join(".git/baleyg/workspace-id")
                    };
                    let mut saved = Vec::new();
                    phase_barrier(&listener, stage, || {
                        saved = fs::read(&marker).unwrap();
                        fs::remove_file(&marker).unwrap();
                    });
                    let reply = peer.reply();
                    tool(&reply, Some("root_changed"), !legacy);
                    let envelope = &reply["result"]["structuredContent"];
                    assert_eq!(envelope["workspace"], json!(root));
                    assert_eq!(envelope["catchingUp"], true);
                    assert_eq!(envelope["error"]["currentBasis"], Value::Null);
                    assert!(envelope["error"].get("reason").is_none());
                    fs::write(&marker, saved).unwrap();
                } else {
                    let moved = temp.path().join(if explicit {
                        "moved-linked"
                    } else {
                        "moved-main"
                    });
                    phase_barrier(&listener, stage, || fs::rename(root, &moved).unwrap());
                    let reply = peer.reply();
                    tool(&reply, Some("root_changed"), !legacy);
                    assert_eq!(
                        reply["result"]["structuredContent"]["workspace"],
                        json!(root)
                    );
                    assert_eq!(
                        reply["result"]["structuredContent"]["error"]["currentBasis"],
                        Value::Null
                    );
                    fs::rename(moved, root).unwrap();
                }
                assert!(peer.finish().0.success());
            }
        }
    }
}

#[test]
fn capacity_witness_final_drift_refuses_stale_capacity_attribution() {
    let temp = tempfile::tempdir().unwrap();
    let (main, first) = git_linked_fixture(temp.path());
    let mut linked = vec![first];
    for index in 1..=63 {
        let path = temp.path().join(format!("linked-{index}"));
        git_selection(
            &main,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                path.to_str().unwrap(),
                "HEAD",
            ],
        );
        linked.push(path.canonicalize().unwrap());
    }
    let socket = temp.path().join("phase.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let mut peer = Peer::start_with_phase(Some(&main), None, Some(("capacity_final", &socket)));
    for (index, path) in linked.iter().take(63).enumerate() {
        let reply = peer.ask(call(
            false,
            json!(index),
            "baleyg_workspace_describe",
            json!({"schemaVersion":1,"workspace":path}),
        ));
        tool(&reply, None, true);
        assert_eq!(
            reply["result"]["structuredContent"]["workspace"],
            json!(path)
        );
    }
    let denied = &linked[63];
    peer.send_raw(
        serde_json::to_string(&call(
            false,
            json!(64),
            "baleyg_workspace_describe",
            json!({"schemaVersion":1,"workspace":denied}),
        ))
        .unwrap()
        .as_bytes(),
    );
    let marker = linked_marker(denied);
    phase_barrier(&listener, "capacity_final", || {
        fs::remove_file(&marker).unwrap()
    });
    let reply = peer.reply();
    tool(&reply, Some("root_changed"), true);
    assert_eq!(
        reply["result"]["structuredContent"]["workspace"],
        json!(denied)
    );
    assert_eq!(reply["result"]["structuredContent"]["catchingUp"], true);
    assert_eq!(
        reply["result"]["structuredContent"]["error"]["currentBasis"],
        Value::Null
    );
    assert!(peer.finish().0.success());
}

#[test]
fn catching_up_reports_pending_h_then_committed_h_on_the_same_checkout() {
    let temp = tempfile::tempdir().unwrap();
    let root = checkout(temp.path());
    let mut peer = Peer::start(Some(&root));
    let first = peer.ask(call(
        false,
        json!(1),
        "baleyg_workspace_describe",
        json!({"schemaVersion":1}),
    ));
    tool(&first, None, true);
    assert_eq!(first["result"]["structuredContent"]["catchingUp"], true);
    let home = peer._home.path();
    let mut command = Command::new(env!("CARGO_BIN_EXE_baleyg"));
    command
        .arg("index")
        .arg("--workspace")
        .arg(&root)
        .env("HOME", home)
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_DATA_HOME", home.join("data"));
    let indexed = bounded_output(command);
    assert!(
        indexed.status.success(),
        "index: {}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    let second = peer.ask(call(
        false,
        json!(2),
        "baleyg_workspace_describe",
        json!({"schemaVersion":1}),
    ));
    tool(&second, None, true);
    assert_eq!(
        second["result"]["structuredContent"]["workspace"],
        first["result"]["structuredContent"]["workspace"]
    );
    assert_eq!(second["result"]["structuredContent"]["catchingUp"], false);
    assert!(peer.finish().0.success());
}
