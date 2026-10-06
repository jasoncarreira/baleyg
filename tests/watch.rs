//! Leader-owned watcher and durable FIFO share one native publication stream.
use baleyg::{
    index_coordinator::{self, LeaderWork},
    indexer::IndexOptions,
    store::Store,
};
use std::{
    fs,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

#[test]
fn leader_reconciles_edit_without_explicit_request() {
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let source = workspace.path().join("a.js");
    fs::write(&source, "function before() { return 1; }\n").unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let options = IndexOptions::new(workspace.path().to_owned());
    let cancel = Arc::new(AtomicBool::new(false));
    let session =
        index_coordinator::establish_serving_session(&store, Some(&options), &cancel).unwrap();
    let before = store.status().unwrap().revision;
    let mut work = LeaderWork::new(&store, &session, &options).unwrap();
    fs::write(&source, "function after() { return 2; }\n").unwrap();
    let until = Instant::now() + Duration::from_secs(8);
    while Instant::now() < until && store.status().unwrap().revision == before {
        work.reconcile_due(&store, &session, &options, &cancel, false)
            .unwrap();
        std::thread::sleep(Duration::from_millis(30));
    }
    assert_ne!(
        store.status().unwrap().revision,
        before,
        "watch signal must publish a fresh selected revision"
    );
    let pinned = store.evidence_response().unwrap();
    let (_, historical) = pinned.source_at("a.js", Some(before)).unwrap().unwrap();
    assert_eq!(historical.text, "function before() { return 1; }\n");
    pinned.finish(()).unwrap();
}

fn cli(root: &std::path::Path, home: &std::path::Path, command: &str) -> std::process::Command {
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_baleyg"));
    child
        .env("HOME", home)
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_DATA_HOME")
        .arg(command)
        .arg("--workspace")
        .arg(root);
    child
}

#[test]
fn finite_cli_owner_child() {
    use std::io::{Read, Write};
    use std::os::{fd::FromRawFd, unix::net::UnixStream};
    let Ok(root) = std::env::var("BALEYG_TEST_FINITE_CLI_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let roots = baleyg::store::topology::TopologyRoots::production().unwrap();
    let identity = baleyg::store::topology::WorkspaceIdentity::discover(
        Some(&root),
        &std::env::current_dir().unwrap(),
    )
    .unwrap();
    roots.reject_root_overlap(&identity).unwrap();
    let store = Store::open(roots, identity).unwrap();
    let options = IndexOptions::new(root);
    // The parent transferred a private socket through FD3 before process start.
    let channel = std::sync::Mutex::new(unsafe { UnixStream::from_raw_fd(3) });
    let paused = AtomicBool::new(false);
    let cancel = Arc::new(AtomicBool::new(false));
    let (_, held_session) =
        index_coordinator::enqueue_and_wait_observed(&store, &options, &cancel, |phase| {
            if phase.phase == "timing:publish"
                && !paused.swap(true, std::sync::atomic::Ordering::AcqRel)
            {
                let mut channel = channel.lock().unwrap();
                channel.write_all(b"P").unwrap();
                let mut release = [0];
                channel.read_exact(&mut release).unwrap();
                assert_eq!(release, *b"G");
            }
        })
        .unwrap();
    assert!(held_session.is_leader());
    let mut channel = channel.lock().unwrap();
    channel.write_all(b"R").unwrap();
    let mut release = [0];
    channel.read_exact(&mut release).unwrap();
    assert_eq!(release, *b"D");
    drop(held_session);
}

struct Server(std::process::Child);
impl Drop for Server {
    fn drop(&mut self) {
        // A failed assertion must not leave a SIGSTOPed CLI child behind.
        match self.0.try_wait() {
            Ok(None) => {
                // The still-live child retains this PID. Never signal after a
                // successful reap or if liveness itself could not be proved.
                unsafe {
                    libc::kill(self.0.id() as i32, libc::SIGCONT);
                }
                let _ = self.0.kill();
            }
            Ok(Some(_)) => {}
            Err(_) => {
                let _ = self.0.kill();
            }
        }
        let _ = self.0.wait();
    }
}

#[test]
fn killed_leader_reconciles_lost_edits_before_serving() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&home).unwrap();
    fs::write(root.join("a.js"), "function before() {}\n").unwrap();
    let token = home.join("token");
    fs::write(
        &token,
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    )
    .unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let server = cli(&root, &home, "serve")
        .arg("--bind")
        .arg(address.to_string())
        .arg("--token-file")
        .arg(&token)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut server = Server(server);
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut ready = false;
    while Instant::now() < deadline {
        if std::net::TcpStream::connect_timeout(&address, Duration::from_millis(50)).is_ok() {
            ready = true;
            break;
        }
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "daemon exited before readiness"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(ready, "daemon did not bind");
    let first = cli(&root, &home, "status").output().unwrap();
    assert!(first.status.success(), "first selected status unavailable");
    let old: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let marker = fs::read(leader_lock_under(&home).unwrap()).unwrap();
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    fs::write(root.join("a.js"), "function after() {}\n").unwrap();
    // A second REAL daemon, without a FIFO request, must reconcile the lost
    // edit before it binds its serving port. An explicit CLI index follows only
    // after the selected source is proved to be the successor's capture.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let successor_addr = listener.local_addr().unwrap();
    drop(listener);
    let successor_process = cli(&root, &home, "serve")
        .arg("--bind")
        .arg(successor_addr.to_string())
        .arg("--token-file")
        .arg(&token)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut successor = Server(successor_process);
    let deadline = Instant::now() + Duration::from_secs(12);
    while std::net::TcpStream::connect_timeout(&successor_addr, Duration::from_millis(50)).is_err()
    {
        assert!(
            Instant::now() < deadline,
            "successor daemon did not serve after takeover"
        );
        assert!(
            successor.0.try_wait().unwrap().is_none(),
            "successor daemon exited"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let selected = cli(&root, &home, "status").output().unwrap();
    assert!(
        selected.status.success(),
        "successor selected status unavailable"
    );
    let selected: serde_json::Value = serde_json::from_slice(&selected.stdout).unwrap();
    assert_eq!(
        selected["revision"]["indexGeneration"],
        old["revision"]["indexGeneration"]
    );
    assert!(
        selected["revision"]["indexRevision"].as_u64().unwrap()
            > old["revision"]["indexRevision"].as_u64().unwrap(),
        "mandatory takeover must publish before any explicit request"
    );
    assert_ne!(fs::read(leader_lock_under(&home).unwrap()).unwrap(), marker);
    let exported = cli(&root, &home, "export").output().unwrap();
    assert!(
        exported.status.success(),
        "successor selected export unavailable"
    );
    let exported: serde_json::Value = serde_json::from_slice(&exported.stdout).unwrap();
    assert_eq!(exported["files"][0]["text"], "function after() {}\n");
    let explicit = cli(&root, &home, "index").output().unwrap();
    assert!(
        explicit.status.success(),
        "explicit request after mandatory takeover failed"
    );
    let explicit: serde_json::Value = serde_json::from_slice(&explicit.stdout).unwrap();
    assert!(
        explicit["publishedRevision"]["indexRevision"]
            .as_u64()
            .unwrap()
            > selected["revision"]["indexRevision"].as_u64().unwrap()
    );
}

fn leader_lock_under(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == "leader.lock") {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = leader_lock_under(&path)
        {
            return Some(found);
        }
    }
    None
}

fn request_db_under(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == "requests.db") {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = request_db_under(&path)
        {
            return Some(found);
        }
    }
    None
}

fn index_db_under(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == "index.db") {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = index_db_under(&path)
        {
            return Some(found);
        }
    }
    None
}

/// AC1 is a separate literal-binary witness. The finite helper test below
/// proves AC4's stronger competing-CLI-before-owner-release property.
#[test]
fn actual_cli_owner_edit_then_daemon_takeover_keeps_selected_b_options() {
    use sha2::Digest;
    use std::{
        os::fd::AsRawFd,
        os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    };
    const B_MAX_BYTES: u64 = 4096;
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&home).unwrap();
    let root_metadata = fs::symlink_metadata(&root).unwrap();
    assert!(root_metadata.file_type().is_dir() && !root_metadata.file_type().is_symlink());
    let root_identity = (root_metadata.dev(), root_metadata.ino());
    for n in 0..400 {
        fs::write(
            root.join(format!("source{n}.js")),
            format!("function f{n}() {{ return {n}; }}\n"),
        )
        .unwrap();
    }
    let cli_log = temp.path().join("actual-cli-stderr.log");
    let cli_output = temp.path().join("actual-cli-stdout.json");
    let executable = std::path::Path::new(env!("CARGO_BIN_EXE_baleyg"));
    let executable_before = sha2::Sha256::digest(fs::read(executable).unwrap());
    let actual = cli(&root, &home, "index")
        .arg("--max-file-bytes")
        .arg(B_MAX_BYTES.to_string())
        .env("BALEYG_INDEX_DIAGNOSTICS", "1")
        .stdout(std::process::Stdio::from(
            fs::File::create(&cli_output).unwrap(),
        ))
        .stderr(std::process::Stdio::from(
            fs::File::create(&cli_log).unwrap(),
        ))
        .spawn()
        .unwrap();
    let mut owner = Server(actual);
    let deadline = Instant::now() + Duration::from_secs(20);
    let leader_lock = loop {
        if let Some(path) = leader_lock_under(&home) {
            let probe = fs::OpenOptions::new().read(true).open(&path).unwrap();
            let busy =
                unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0;
            if !busy {
                unsafe {
                    libc::flock(probe.as_raw_fd(), libc::LOCK_UN);
                }
            }
            // Only a real published-capture diagnostic counts as a stop barrier.
            // This is a bounded observation, never a production timing hook.
            if busy
                && fs::read_to_string(&cli_log)
                    .unwrap()
                    .contains("index-phase publish_ms=")
            {
                assert_eq!(unsafe { libc::kill(owner.0.id() as i32, libc::SIGSTOP) }, 0);
                assert!(
                    owner.0.try_wait().unwrap().is_none(),
                    "real CLI exited before stop"
                );
                let check = fs::OpenOptions::new().read(true).open(&path).unwrap();
                assert_ne!(
                    unsafe { libc::flock(check.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
                    0,
                    "stopped actual CLI must still own EX"
                );
                break path;
            }
        }
        assert!(
            Instant::now() < deadline,
            "real CLI EX/publication checkpoint not observed"
        );
        assert!(
            owner.0.try_wait().unwrap().is_none(),
            "real CLI exited before checkpoint: {}",
            fs::read_to_string(&cli_log).unwrap()
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let first_marker = fs::read(&leader_lock).unwrap();
    assert_eq!(first_marker.len(), 36);
    uuid::Uuid::parse_str(std::str::from_utf8(&first_marker).unwrap()).unwrap();
    assert_eq!(
        executable_before,
        sha2::Sha256::digest(fs::read(executable).unwrap()),
        "actual executable drifted while live"
    );
    fs::write(
        root.join("source0.js"),
        "function during_real_cli() { return 999; }\n",
    )
    .unwrap();
    let token = home.join("token");
    fs::write(&token, TOKEN).unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let daemon = cli(&root, &home, "serve")
        .arg("--bind")
        .arg(address.to_string())
        .arg("--token-file")
        .arg(&token)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut daemon = Server(daemon);
    let deadline = Instant::now() + Duration::from_secs(12);
    while std::net::TcpStream::connect_timeout(&address, Duration::from_millis(50)).is_err() {
        assert!(Instant::now() < deadline, "follower daemon not ready");
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "follower daemon exited"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(fs::read(&leader_lock).unwrap(), first_marker);
    assert_eq!(unsafe { libc::kill(owner.0.id() as i32, libc::SIGCONT) }, 0);
    let deadline = Instant::now() + Duration::from_secs(25);
    let cli_exit = loop {
        if let Some(status) = owner.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "real CLI failed finite release: {}",
            fs::read_to_string(&cli_log).unwrap()
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        cli_exit.success(),
        "actual CLI failed: {}",
        fs::read_to_string(&cli_log).unwrap()
    );
    let cli_result: serde_json::Value =
        serde_json::from_slice(&fs::read(&cli_output).unwrap()).unwrap();
    assert!(
        cli_result["publishedRevision"]["indexRevision"]
            .as_u64()
            .is_some()
    );
    assert_eq!(
        executable_before,
        sha2::Sha256::digest(fs::read(executable).unwrap())
    );
    let requests = rusqlite::Connection::open_with_flags(
        request_db_under(&home).unwrap(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let pending: i64 = requests
        .query_row(
            "SELECT count(*) FROM requests WHERE state IN ('queued','running')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        pending, 0,
        "daemon takeover must start with an empty explicit FIFO"
    );
    let selected_max_bytes = || -> u64 {
        let index = rusqlite::Connection::open_with_flags(
            index_db_under(&home).unwrap(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let payload: String = index
            .query_row(
                "SELECT reconcile_options FROM index_metadata WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str::<baleyg::indexer::ReconcileOptions>(&payload)
            .unwrap()
            .max_file_bytes
    };
    let deadline = Instant::now() + Duration::from_secs(12);
    while fs::read(&leader_lock).unwrap() == first_marker {
        assert!(
            Instant::now() < deadline,
            "follower daemon did not elect new synced incarnation"
        );
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "daemon exited during takeover"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let successor_marker = fs::read(&leader_lock).unwrap();
    assert_ne!(successor_marker, first_marker);
    uuid::Uuid::parse_str(std::str::from_utf8(&successor_marker).unwrap()).unwrap();
    let lock_metadata = fs::symlink_metadata(&leader_lock).unwrap();
    assert!(lock_metadata.file_type().is_file() && !lock_metadata.file_type().is_symlink());
    let lock_identity = (lock_metadata.dev(), lock_metadata.ino());
    let lock_mode = lock_metadata.mode() & 0o777;
    let deadline = Instant::now() + Duration::from_secs(12);
    let first_export = loop {
        let export = cli(&root, &home, "export").output().unwrap();
        if export.status.success() {
            break serde_json::from_slice::<serde_json::Value>(&export.stdout).unwrap();
        }
        // A failed export is meaningful only while the same successor still
        // owns EX for the original root. Check on every failed read before
        // accepting even a structured transient error.
        let root_now =
            fs::symlink_metadata(&root).unwrap_or_else(|_| panic!("successor root missing"));
        assert!(
            root_now.file_type().is_dir() && !root_now.file_type().is_symlink(),
            "successor root changed type"
        );
        assert_eq!(
            (root_now.dev(), root_now.ino()),
            root_identity,
            "successor root identity changed"
        );
        let lock_now = fs::symlink_metadata(&leader_lock)
            .unwrap_or_else(|_| panic!("successor marker missing"));
        assert!(
            lock_now.file_type().is_file() && !lock_now.file_type().is_symlink(),
            "successor marker path changed type"
        );
        assert_eq!(
            (lock_now.dev(), lock_now.ino(), lock_now.mode() & 0o777),
            (lock_identity.0, lock_identity.1, lock_mode),
            "successor marker inode/mode changed"
        );
        assert_eq!(
            fs::read(&leader_lock).unwrap(),
            successor_marker,
            "successor marker changed"
        );
        let mut probe_options = fs::OpenOptions::new();
        probe_options.read(true).custom_flags(libc::O_NOFOLLOW);
        let probe = probe_options
            .open(&leader_lock)
            .unwrap_or_else(|_| panic!("successor EX probe refused marker"));
        let held_lock = probe
            .metadata()
            .unwrap_or_else(|_| panic!("successor EX probe lost inode"));
        assert!(
            held_lock.file_type().is_file(),
            "successor EX probe opened wrong type"
        );
        assert_eq!(
            (held_lock.dev(), held_lock.ino(), held_lock.mode() & 0o777),
            (lock_identity.0, lock_identity.1, lock_mode),
            "successor EX probe opened a different marker inode/mode"
        );
        let lock_result = unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        let lock_error = std::io::Error::last_os_error().raw_os_error();
        if lock_result == 0 {
            assert_eq!(
                unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_UN) },
                0,
                "failed to release unexpected EX probe"
            );
        }
        let named_again = fs::symlink_metadata(&leader_lock)
            .unwrap_or_else(|_| panic!("successor marker disappeared"));
        assert!(
            named_again.file_type().is_file() && !named_again.file_type().is_symlink(),
            "successor marker replaced with wrong type"
        );
        assert_eq!(
            (
                named_again.dev(),
                named_again.ino(),
                named_again.mode() & 0o777
            ),
            (lock_identity.0, lock_identity.1, lock_mode),
            "successor marker replaced during EX probe"
        );
        assert_eq!(
            fs::read(&leader_lock).unwrap(),
            successor_marker,
            "successor marker changed during EX probe"
        );
        assert_eq!(lock_result, -1, "successor EX not held");
        assert_eq!(
            lock_error,
            Some(libc::EWOULDBLOCK),
            "successor EX probe failed for non-contention reason"
        );
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "successor daemon exited"
        );
        let error = String::from_utf8_lossy(&export.stderr);
        assert!(
            error.starts_with("Error: index_not_ready: ")
                || error == "Error: storage_busy: SQLite lock contention\n",
            "unexpected selected read failure"
        );
        assert!(
            Instant::now() < deadline,
            "daemon never finished mandatory takeover reconcile"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        first_export["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "source0.js"
                && f["text"] == "function during_real_cli() { return 999; }\n")
    );
    // Only a successful selected read can prove the new incarnation has
    // committed. Reading option bytes at marker rotation would still see B
    // from the predecessor and could false-green a default-option takeover.
    assert_eq!(
        selected_max_bytes(),
        B_MAX_BYTES,
        "daemon defaults reverted committed B options"
    );
    let first_status = cli(&root, &home, "status").output().unwrap();
    assert!(first_status.status.success());
    let first_status: serde_json::Value = serde_json::from_slice(&first_status.stdout).unwrap();
    fs::write(
        root.join("source1.js"),
        "function daemon_under_b() { return 1234; }\n",
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    let live = loop {
        let status = cli(&root, &home, "status").output().unwrap();
        if status.status.success() {
            let current: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
            if current["revision"] != first_status["revision"] {
                let export = cli(&root, &home, "export").output().unwrap();
                if export.status.success() {
                    let graph: serde_json::Value = serde_json::from_slice(&export.stdout).unwrap();
                    if graph["files"].as_array().is_some_and(|files| {
                        files.iter().any(|f| {
                            f["path"] == "source1.js"
                                && f["text"] == "function daemon_under_b() { return 1234; }\n"
                        })
                    }) {
                        break graph;
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not refresh edit under selected B options"
        );
        std::thread::sleep(Duration::from_millis(30));
    };
    assert_eq!(selected_max_bytes(), B_MAX_BYTES);
    let cold_root = temp.path().join("cold-root");
    let cold_home = temp.path().join("cold-home");
    fs::create_dir(&cold_root).unwrap();
    fs::create_dir(&cold_home).unwrap();
    for entry in fs::read_dir(&root).unwrap().flatten() {
        if entry.path().is_file() {
            fs::copy(entry.path(), cold_root.join(entry.file_name())).unwrap();
        }
    }
    let cold_index = cli(&cold_root, &cold_home, "index")
        .arg("--max-file-bytes")
        .arg(B_MAX_BYTES.to_string())
        .output()
        .unwrap();
    assert!(cold_index.status.success(), "cold B full index failed");
    let cold = cli(&cold_root, &cold_home, "export").output().unwrap();
    assert!(cold.status.success());
    let mut cold: serde_json::Value = serde_json::from_slice(&cold.stdout).unwrap();
    let mut live = live;
    // Native SyntaxIds hash the sourceSet (`source-set:v1:{root_id}`), so the
    // separate cold checkout MUST have different IDs for identical symbols.
    // This fixture has no calls/regions or parent references. Assert that
    // shape, require unique valid IDs in both graphs, then remove ONLY these
    // checkout-qualified IDs. Every other field and complete element remains
    // exact; sorting only removes incremental versus cold vector order.
    for graph in [&mut live, &mut cold] {
        assert!(graph["calls"].as_array().unwrap().is_empty());
        assert!(graph["regions"].as_array().unwrap().is_empty());
        let mut ids = std::collections::HashSet::new();
        for node in graph["nodes"].as_array_mut().unwrap() {
            assert!(
                node["parent"].is_null(),
                "fixture gained root-dependent parent references"
            );
            let id = node.as_object_mut().unwrap().remove("id").unwrap();
            let id = id.as_str().unwrap();
            assert!(id.len() == 39 && id.starts_with("sid:v1:"));
            assert!(ids.insert(id.to_owned()), "duplicate native SyntaxId");
        }
        for field in ["files", "nodes", "calls", "regions", "diagnostics"] {
            graph[field].as_array_mut().unwrap().sort_by(|a, b| {
                serde_json::to_string(a)
                    .unwrap()
                    .cmp(&serde_json::to_string(b).unwrap())
            });
        }
    }
    for field in [
        "schemaVersion",
        "files",
        "nodes",
        "calls",
        "regions",
        "diagnostics",
        "stats",
    ] {
        if let (Some(a), Some(b)) = (live[field].as_array(), cold[field].as_array()) {
            assert_eq!(a.len(), b.len(), "cold B graph {field} count differs");
            for (n, (a, b)) in a.iter().zip(b).enumerate() {
                assert_eq!(
                    a, b,
                    "cold B graph {field}[{n}] differs (only checkout-qualified SyntaxIds normalized)"
                );
            }
        } else {
            assert_eq!(live[field], cold[field], "cold B graph {field} differs");
        }
    }
}

// Fail-closed real-binary barrier proof. Until the opt-in FD3 hook exists this
// MUST fail with "missing P/owner advanced", never pass by a completed queue row.
#[test]
fn real_cli_preclaim_barrier_withheld_g_keeps_own_request_queued() {
    use std::{
        io::Read,
        os::{fd::AsRawFd, unix::process::CommandExt},
        time::{Duration, Instant},
    };
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&home).unwrap();
    fs::write(root.join("source.js"), "function before() {}\n").unwrap();
    let seed = cli(&root, &home, "index").output().unwrap();
    assert!(
        seed.status.success(),
        "healthy initial real CLI index required"
    );
    let status = cli(&root, &home, "status").output().unwrap();
    assert!(status.status.success(), "healthy selected head required");
    let head: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert!(
        head["revision"]["indexRevision"]
            .as_u64()
            .is_some_and(|n| n > 0)
    );
    let seed_marker = fs::read(leader_lock_under(&home).unwrap()).unwrap();
    assert_eq!(seed_marker.len(), 36, "healthy seed incarnation missing");
    uuid::Uuid::parse_str(std::str::from_utf8(&seed_marker).unwrap()).unwrap();
    let request_db = request_db_under(&home).unwrap();
    let seed_db = rusqlite::Connection::open_with_flags(
        &request_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let seed_seq: i64 = seed_db
        .query_row("SELECT coalesce(max(seq),0) FROM requests", [], |row| {
            row.get(0)
        })
        .unwrap();
    drop(seed_db);
    fs::write(root.join("source.js"), "function after() {}\n").unwrap();
    // No daemon or other CLI is spawned before the first owner's P.
    let (mut channel, child_socket) = std::os::unix::net::UnixStream::pair().unwrap();
    channel
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    channel
        .set_write_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let owner_log = temp.path().join("red-owner-stderr.log");
    let owner_stdout = temp.path().join("red-owner-stdout.log");
    let fd = child_socket.as_raw_fd();
    let mut command = cli(&root, &home, "index");
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command
        .env("BALEYG_TEST_FINITE_CLI_FD", "3")
        .stdout(std::process::Stdio::from(
            fs::File::create(&owner_stdout).unwrap(),
        ))
        .stderr(std::process::Stdio::from(
            fs::File::create(&owner_log).unwrap(),
        ))
        .spawn()
        .unwrap();
    drop(child_socket);
    let mut owner = Server(child);
    let rows = || -> Vec<(String, String)> {
        let db = rusqlite::Connection::open_with_flags(
            &request_db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut query = db
            .prepare("SELECT id,state FROM requests ORDER BY seq")
            .unwrap();
        query
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let mut p = [0u8];
    let signal = channel.read_exact(&mut p);
    if let Err(read_error) = signal {
        // Classify the hook-absent baseline only AFTER the actual CLI has
        // independently completed and its OWN post-seed durable row matches
        // the successful CLI result. A stalled, failed, or partial owner is a
        // different RED and cannot masquerade as missing P/owner advanced.
        let deadline = Instant::now() + Duration::from_secs(12);
        let owner_exit = loop {
            if let Some(status) = owner.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "missing P but owner stalled: {read_error}"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            owner_exit.success(),
            "missing P but owner exited nonzero: {owner_exit}"
        );
        let output = fs::read(&owner_stdout).unwrap();
        let result: serde_json::Value = serde_json::from_slice(&output)
            .expect("missing P but successful owner stdout has no valid result");
        let published = &result["publishedRevision"];
        assert!(
            published["indexRevision"].as_u64().is_some(),
            "missing P but successful owner has no published pin"
        );
        assert_eq!(
            result["status"]["workspaceRoot"], head["workspaceRoot"],
            "missing P but owner result has wrong root"
        );
        let db = rusqlite::Connection::open_with_flags(
            &request_db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut query = db
            .prepare(
                "SELECT seq,id,state,result_generation,result_revision FROM requests WHERE seq>?1 ORDER BY seq",
            )
            .unwrap();
        let newer: Vec<_> = query
            .query_map([seed_seq], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            newer.len(),
            1,
            "missing P but owner did not create exactly one own row"
        );
        let own = &newer[0];
        assert_eq!(
            own.2.as_str(),
            "done",
            "missing P but own row did not finish"
        );
        assert_eq!(
            published["indexGeneration"],
            own.3.as_ref().unwrap().as_str()
        );
        assert_eq!(published["indexRevision"], own.4.unwrap());
        let advanced_marker = fs::read(leader_lock_under(&home).unwrap()).unwrap();
        assert_ne!(
            advanced_marker, seed_marker,
            "missing P but no new owner incarnation"
        );
        uuid::Uuid::parse_str(std::str::from_utf8(&advanced_marker).unwrap()).unwrap();
        panic!("missing P/owner advanced: completed own row matched successful real CLI result");
    }
    assert_eq!(p, *b"P", "invalid P frame from owner");
    let at_p = rows();
    let queued: Vec<_> = at_p.iter().filter(|row| row.1 == "queued").collect();
    assert_eq!(
        queued.len(),
        1,
        "P must precede exactly one own queued claim"
    );
    assert!(at_p.iter().all(|row| row.1 != "running"), "P after claim");
    let own_id = queued[0].0.clone();
    assert!(owner.0.try_wait().unwrap().is_none(), "owner exited at P");
    let lock_path = leader_lock_under(&home).unwrap();
    let marker = fs::read(&lock_path).unwrap();
    assert_eq!(marker.len(), 36, "P before synced incarnation");
    uuid::Uuid::parse_str(std::str::from_utf8(&marker).unwrap()).unwrap();
    assert_ne!(
        marker, seed_marker,
        "P must expose a fresh owner incarnation"
    );
    let lock_probe = fs::OpenOptions::new().read(true).open(&lock_path).unwrap();
    let result = unsafe { libc::flock(lock_probe.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        assert_eq!(
            unsafe { libc::flock(lock_probe.as_raw_fd(), libc::LOCK_UN) },
            0
        );
        panic!("P did not hold first owner's EX");
    }
    let error = std::io::Error::last_os_error();
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::WouldBlock,
        "P EX probe failed for a reason other than live contention: {error}"
    );
    // Intentionally withhold G. EOF is the explicit failure stimulus.
    drop(channel);
    let deadline = Instant::now() + Duration::from_secs(12);
    let exit = loop {
        if let Some(status) = owner.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "owner did not fail after withheld G"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(!exit.success(), "withheld G must never produce CLI success");
    assert_eq!(
        rows()
            .iter()
            .find(|row| row.0.as_str() == own_id.as_str())
            .unwrap()
            .1
            .as_str(),
        "queued",
        "preclaim barrier failure must not claim or complete the accepted row"
    );
    assert!(fs::metadata(&owner_log).unwrap().len() <= 64 * 1024);
}

#[tokio::test]
async fn completed_cli_result_read_busy_keeps_done_and_requires_successor_reconcile() {
    use std::{
        io::{Read, Write},
        os::{
            fd::AsRawFd,
            unix::{fs::PermissionsExt, process::CommandExt},
        },
    };
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&home).unwrap();
    let source = root.join("source.js");
    fs::write(&source, "function before() {}\n").unwrap();
    let seed = cli(&root, &home, "index").output().unwrap();
    assert!(seed.status.success());
    let seed_result: serde_json::Value = serde_json::from_slice(&seed.stdout).unwrap();
    let expected_root = seed_result["status"]["workspaceRoot"]
        .as_str()
        .unwrap()
        .to_owned();
    let predecessor = fs::read(leader_lock_under(&home).unwrap()).unwrap();
    let request_db = request_db_under(&home).unwrap();
    let seed_seq: i64 = rusqlite::Connection::open(&request_db)
        .unwrap()
        .query_row("SELECT coalesce(max(seq),0) FROM requests", [], |r| {
            r.get(0)
        })
        .unwrap();
    fs::write(&source, "function own_result() {}\n").unwrap();

    // This opt-in real binary emits P before claiming its own row and R only
    // after its durable DONE pin has passed coordinator evidence proof.
    let (mut channel, child_socket) = std::os::unix::net::UnixStream::pair().unwrap();
    channel
        .set_read_timeout(Some(Duration::from_secs(45)))
        .unwrap();
    channel
        .set_write_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let owner_log = temp.path().join("owner-stderr.log");
    let owner_stdout = temp.path().join("owner-stdout.json");
    let child_fd = child_socket.as_raw_fd();
    let mut command = cli(&root, &home, "index");
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(child_fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command
        .env("BALEYG_TEST_FINITE_CLI_FD", "3")
        .env("BALEYG_INDEX_DIAGNOSTICS", "1")
        .stdout(std::process::Stdio::from(
            fs::File::create(&owner_stdout).unwrap(),
        ))
        .stderr(std::process::Stdio::from(
            fs::File::create(&owner_log).unwrap(),
        ))
        .spawn()
        .unwrap();
    drop(child_socket);
    let mut owner = Server(child);
    let mut signal = [0u8];
    channel.read_exact(&mut signal).unwrap();
    assert_eq!(
        signal, *b"P",
        "mandatory reconcile must precede own FIFO claim"
    );
    let own_id: String = {
        let db = rusqlite::Connection::open(&request_db).unwrap();
        let mut query = db
            .prepare("SELECT seq,id,state FROM requests WHERE seq>?1 ORDER BY seq")
            .unwrap();
        let newer: Vec<(i64, String, String)> = query
            .query_map([seed_seq], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(newer.len(), 1, "P must have exactly one post-seed own row");
        assert_eq!(newer[0].2, "queued", "P must precede own claim");
        let running: i64 = db
            .query_row(
                "SELECT count(*) FROM requests WHERE state='running'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(running, 0, "P must precede all FIFO claims");
        newer[0].1.clone()
    };
    let leader_lock = leader_lock_under(&home).unwrap();
    let marker = fs::read(&leader_lock).unwrap();
    assert_ne!(marker, predecessor);
    uuid::Uuid::parse_str(std::str::from_utf8(&marker).unwrap()).unwrap();
    let assert_live_ex = |path: &std::path::Path| {
        let probe = fs::OpenOptions::new().read(true).open(path).unwrap();
        let result = unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            assert_eq!(unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_UN) }, 0);
            panic!("independent probe acquired supposed live owner EX");
        }
        let error = std::io::Error::last_os_error();
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::WouldBlock,
            "EX probe failed for a non-contention reason: {error}"
        );
    };
    assert_live_ex(&leader_lock);
    channel.write_all(b"G").unwrap();
    channel.read_exact(&mut signal).unwrap();
    assert_eq!(
        signal, *b"R",
        "coordinator must return before terminal status"
    );
    assert!(owner.0.try_wait().unwrap().is_none());
    assert_eq!(fs::read(&leader_lock).unwrap(), marker);

    let row = || -> (String, String, i64) {
        let db = rusqlite::Connection::open(&request_db).unwrap();
        db.query_row(
            "SELECT state,result_generation,result_revision FROM requests WHERE id=?1",
            [&own_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    let own = row();
    assert_eq!(
        own.0, "done",
        "own accepted row must be DONE before any writer lock"
    );
    assert!(own.2 > 0, "DONE pin must be positive");
    let index_path = index_db_under(&home).unwrap();
    let writer = rusqlite::Connection::open(&index_path).unwrap();
    writer.busy_timeout(Duration::ZERO).unwrap();
    let journal: String = writer
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .unwrap();
    assert_eq!(journal, "delete");
    let selected: (String, i64) = writer
        .query_row(
            "SELECT index_generation,index_revision FROM index_metadata WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(selected, (own.1.clone(), own.2));
    writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let contended_since = Instant::now();
    channel.write_all(b"D").unwrap();
    // Harness deadlock safety only. This does NOT select the product retry bound;
    // the owner must choose that from an observed selected-DB commit envelope.
    let deadline = Instant::now() + Duration::from_secs(30);
    let exit = loop {
        if let Some(status) = owner.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "result-read wait did not end finitely"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(
        !exit.success(),
        "SQLite contention must not fabricate CLI success"
    );
    assert_eq!(
        row(),
        own,
        "terminal DONE row and pin must not move backwards"
    );
    assert_eq!(fs::metadata(&owner_stdout).unwrap().len(), 0);
    assert!(
        fs::metadata(&owner_log).unwrap().len() <= 64 * 1024,
        "private owner log exceeded fixed size cap"
    );
    let output = fs::read(&owner_log).unwrap();
    let stderr = String::from_utf8_lossy(&output);
    let attempts = stderr
        .matches("index-phase terminal_status_attempt")
        .count();
    // Precise baseline RED: current main has one status read after R. Raw
    // SQLite code 5 is NOT a truthful typed expiry of a durable DONE request.
    if attempts == 1
        && stderr.contains("Error: database is locked")
        && stderr.contains("Error code 5: database is locked")
    {
        assert!(
            !stderr.contains(&own_id),
            "raw SQLite baseline unexpectedly named own ID"
        );
        panic!(
            "post-DONE RED: terminal status returned raw SQLite code 5, not typed busy with own request ID"
        );
    }
    if attempts == 1 && stderr.contains("storage_busy:") && !stderr.contains(&own_id) {
        panic!(
            "post-DONE RED: terminal status returned unstructured busy without durable own request ID"
        );
    }
    assert!(
        attempts >= 2,
        "terminal status did not make multiple bounded BUSY attempts; other failure is not expected RED"
    );
    assert!(
        contended_since.elapsed() >= Duration::from_millis(100),
        "retry attempts must not spin"
    );
    for expected in [
        "storage_busy:",
        own_id.as_str(),
        "durable",
        "may already be complete",
        "check job status",
    ] {
        assert!(
            stderr.contains(expected),
            "typed result-read expiry missing {expected}"
        );
    }
    assert!(!stderr.contains("index_failed") && !stderr.contains("queued indexing failed"));
    writer.execute_batch("ROLLBACK").unwrap();
    drop(writer);
    assert_eq!(row(), own);

    // A separate successor must reconcile a NEW edit before its Status is ready.
    fs::write(&source, "function after_timeout() {}\n").unwrap();
    let token = home.join("token");
    fs::write(&token, TOKEN).unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let server = cli(&root, &home, "serve")
        .arg("--bind")
        .arg(address.to_string())
        .arg("--token-file")
        .arg(&token)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(
            fs::File::create(temp.path().join("successor.log")).unwrap(),
        ))
        .spawn()
        .unwrap();
    let mut successor = Server(server);
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(12);
    let ready = loop {
        assert!(
            successor.0.try_wait().unwrap().is_none(),
            "successor exited"
        );
        if let Ok(response) = client
            .get(format!("http://{address}/api/status"))
            .bearer_auth(TOKEN)
            .send()
            .await
        {
            let code = response.status();
            let body = response.bytes().await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if code == reqwest::StatusCode::OK {
                assert_eq!(value["workspaceRoot"], expected_root);
                let pin = &value["revision"];
                assert_eq!(pin["indexGeneration"].as_str(), Some(own.1.as_str()));
                assert!(
                    pin["indexRevision"]
                        .as_u64()
                        .is_some_and(|r| r > own.2 as u64),
                    "HTTP 200 cannot serve the pre-successor DONE head"
                );
                break value;
            }
            let error_code = value.pointer("/error/code").and_then(|v| v.as_str());
            assert!(
                (code == reqwest::StatusCode::SERVICE_UNAVAILABLE
                    && error_code == Some("index_not_ready"))
                    || (code == reqwest::StatusCode::CONFLICT
                        && error_code == Some("storage_busy")),
                "unexpected successor status before reconciliation: {code} {error_code:?}"
            );
        }
        assert!(
            Instant::now() < deadline,
            "successor never reconciled before status"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    let new_marker = fs::read(&leader_lock).unwrap();
    assert_ne!(new_marker, marker);
    uuid::Uuid::parse_str(std::str::from_utf8(&new_marker).unwrap()).unwrap();
    assert_live_ex(&leader_lock);
    // Bind source bytes to the daemon's authenticated selected HTTP pin BEFORE
    // a separate export can run; an export may never establish this witness.
    let pin = &ready["revision"];
    let rev_text = pin["indexRevision"].as_u64().unwrap().to_string();
    let source_response = client
        .get(format!("http://{address}/api/source"))
        .bearer_auth(TOKEN)
        .query(&[
            ("path", "source.js"),
            ("indexGeneration", pin["indexGeneration"].as_str().unwrap()),
            ("indexRevision", rev_text.as_str()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(source_response.status(), reqwest::StatusCode::OK);
    let pinned_source: serde_json::Value = source_response.json().await.unwrap();
    assert_eq!(pinned_source["revision"], *pin);
    assert_eq!(pinned_source["file"]["path"], "source.js");
    assert_eq!(
        pinned_source["file"]["text"],
        "function after_timeout() {}\n"
    );
    let export = cli(&root, &home, "export").output().unwrap();
    assert!(
        export.status.success(),
        "selected export under successor must succeed"
    );
    let graph: serde_json::Value = serde_json::from_slice(&export.stdout).unwrap();
    assert!(
        graph["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "source.js" && f["text"] == "function after_timeout() {}\n")
    );
    let follow = client
        .get(format!("http://{address}/api/status"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(follow.status(), reqwest::StatusCode::OK);
    let after: serde_json::Value = follow.json().await.unwrap();
    assert_eq!(after["revision"], ready["revision"]);
}

#[tokio::test]
async fn cli_daemon_edit_during_cli_leadership_then_handoff_matches_cold_full() {
    cli_daemon_handoff_fixture(false).await;
}

#[tokio::test]
async fn direct_child_daemon_handoff_preserves_queued_cli_and_browser_ack() {
    cli_daemon_handoff_fixture(true).await;
}

async fn cli_daemon_handoff_fixture(direct_child: bool) {
    use std::{
        os::fd::AsRawFd,
        os::unix::fs::{MetadataExt, PermissionsExt},
    };
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&home).unwrap();
    let original_root = fs::metadata(&root).unwrap();
    let root_identity = (original_root.dev(), original_root.ino());
    // Enough work to observe the real CLI owner's nonblocking flock.
    for n in 0..180 {
        fs::write(
            root.join(format!("source{n}.js")),
            format!("function f{n}() {{ return {n}; }}\n"),
        )
        .unwrap();
    }
    // An initial CLI builds the predecessor head. The child below drives the
    // SAME production finite CLI coordinator, but IPC freezes it after its
    // mandatory takeover publication and before claiming its first FIFO row.
    assert!(
        cli(&root, &home, "index")
            .output()
            .unwrap()
            .status
            .success()
    );
    // Scope this timing:publish hook to a healthy selected head; no daemon exists yet.
    let seed_status = cli(&root, &home, "status").output().unwrap();
    assert!(seed_status.status.success(), "healthy seed status required");
    let seed: serde_json::Value = serde_json::from_slice(&seed_status.stdout).unwrap();
    let expected_root = seed["workspaceRoot"].as_str().unwrap().to_owned();
    assert!(
        seed["revision"]["indexRevision"]
            .as_u64()
            .is_some_and(|n| n > 0)
    );
    fs::write(
        root.join("source0.js"),
        "function edited() { return 999; }\n",
    )
    .unwrap();
    // An inherited socket pair removes the unbounded UnixStream::connect step.
    use std::os::unix::process::CommandExt;
    let (mut channel, child_socket) = std::os::unix::net::UnixStream::pair().unwrap();
    channel
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    channel
        .set_write_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    // File-backed output keeps fatal child diagnostics bounded and private.
    let owner_log = temp.path().join("finite-cli-owner.log");
    let owner_stdout_path = temp.path().join("finite-cli-owner-stdout.log");
    let child_fd = child_socket.as_raw_fd();
    let mut owner_command = if direct_child {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("finite_cli_owner_child")
            .env("HOME", &home)
            .env_remove("XDG_CACHE_HOME")
            .env_remove("XDG_DATA_HOME")
            .env("BALEYG_TEST_FINITE_CLI_ROOT", &root);
        command
    } else {
        cli(&root, &home, "index")
    };
    unsafe {
        owner_command.pre_exec(move || {
            if libc::dup2(child_fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    if direct_child {
        // The historical direct child did not write progress diagnostics
        // inside the EX/SQLite handoff. Keep its contention timing intact.
        owner_command.env_remove("BALEYG_INDEX_DIAGNOSTICS");
    } else {
        owner_command.env("BALEYG_INDEX_DIAGNOSTICS", "1");
    }
    let child = owner_command
        .env("BALEYG_TEST_FINITE_CLI_FD", "3")
        .env("RUST_LIB_BACKTRACE", "1")
        .env("RUST_BACKTRACE", "1")
        .stdout(std::process::Stdio::from(
            fs::File::create(&owner_stdout_path).unwrap(),
        ))
        .stderr(std::process::Stdio::from(
            fs::File::create(&owner_log).unwrap(),
        ))
        .spawn()
        .unwrap();
    drop(child_socket);
    let mut cli_owner = Server(child);
    let mut milestone = [0];
    use std::io::{Read, Write};
    channel.read_exact(&mut milestone).unwrap();
    assert_eq!(
        milestone, *b"P",
        "first finite CLI publication must precede FIFO claim"
    );
    assert!(cli_owner.0.try_wait().unwrap().is_none());
    let leader_lock = leader_lock_under(&home).unwrap();
    let cli_incarnation = fs::read(&leader_lock).unwrap();
    assert_eq!(cli_incarnation.len(), 36, "synced leader marker missing");
    uuid::Uuid::parse_str(std::str::from_utf8(&cli_incarnation).unwrap()).unwrap();
    let probe = fs::OpenOptions::new()
        .read(true)
        .open(&leader_lock)
        .unwrap();
    assert_ne!(
        unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "finite CLI child must hold EX before competing ingress"
    );
    let request_db = request_db_under(&home).unwrap();
    type DurableQueuedRow = (i64, String, String, Option<String>, Option<i64>);
    let queued_rows = || -> Vec<DurableQueuedRow> {
        let db = rusqlite::Connection::open_with_flags(
            &request_db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut query = db
            .prepare(
                "SELECT seq,id,state,result_generation,result_revision FROM requests ORDER BY seq",
            )
            .unwrap();
        query
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let at_p = queued_rows();
    assert_eq!(
        at_p.iter().filter(|row| row.2 == "queued").count(),
        1,
        "P must precede all FIFO claims except the single own queued row"
    );
    assert!(
        at_p.iter().all(|row| row.2 != "running"),
        "P must occur before any claim"
    );
    let first_row = at_p.into_iter().find(|row| row.2 == "queued").unwrap();
    let token = home.join("token");
    fs::write(&token, TOKEN).unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let daemon_log = temp.path().join("follower-daemon-stderr.log");
    let server = cli(&root, &home, "serve")
        .arg("--bind")
        .arg(address.to_string())
        .arg("--token-file")
        .arg(&token)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(
            fs::File::create(&daemon_log).unwrap(),
        ))
        .spawn()
        .unwrap();
    let mut daemon = Server(server);
    let client = reqwest::Client::new();
    let url = format!("http://{address}");
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut ready = false;
    while Instant::now() < deadline {
        if client
            .get(format!("{url}/healthz"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            ready = true;
            break;
        }
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "follower daemon exited before readiness"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert!(
        ready,
        "follower daemon did not bind while CLI held leadership"
    );
    let accepted = client
        .post(format!("{url}/api/index"))
        .header("Origin", &url)
        .bearer_auth(TOKEN)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        accepted.status(),
        202,
        "browser FIFO must accept while CLI owns lock"
    );
    let job: serde_json::Value = accepted.json().await.unwrap();
    assert_eq!(job["state"], "queued");
    // The second process is the actual `baleyg index` CLI. Both foreign rows
    // must exist DURABLY before the first owner reaches its finite cutoff.
    let contender_log = temp.path().join("contender-stderr.log");
    let contender_stdout = temp.path().join("contender-stdout.log");
    let contender_process = cli(&root, &home, "index")
        .env_remove("BALEYG_INDEX_DIAGNOSTICS")
        .env("RUST_LIB_BACKTRACE", "1")
        .stdout(std::process::Stdio::from(
            fs::File::create(&contender_stdout).unwrap(),
        ))
        .stderr(std::process::Stdio::from(
            fs::File::create(&contender_log).unwrap(),
        ))
        .spawn()
        .unwrap();
    let mut contender = Server(contender_process);
    let browser_id = job["id"].as_str().unwrap().to_owned();
    let deadline = Instant::now() + Duration::from_secs(12);
    let contender_row = loop {
        let rows = queued_rows();
        if let Some(browser) = rows.iter().find(|row| row.1 == browser_id)
            && let Some(contender_row) = rows
                .iter()
                .find(|row| row.0 > browser.0 && row.1 != first_row.1 && row.2 == "queued")
        {
            assert!(first_row.0 < browser.0 && browser.0 < contender_row.0);
            assert_eq!(
                first_row.2, "queued",
                "first owner must still be before FIFO claim"
            );
            break contender_row.clone();
        }
        assert!(
            Instant::now() < deadline,
            "second CLI row was not durably admitted"
        );
        assert!(
            contender.0.try_wait().unwrap().is_none(),
            "contender exited before FIFO admission"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(fs::read(&leader_lock).unwrap(), cli_incarnation);
    channel.write_all(b"G").unwrap();
    channel
        .set_read_timeout(Some(Duration::from_secs(45)))
        .unwrap();
    if let Err(read_error) = channel.read_exact(&mut milestone) {
        let child_status = cli_owner.0.try_wait();
        let mut child_log = Vec::new();
        fs::File::open(&owner_log)
            .unwrap()
            .take(64 * 1024)
            .read_to_end(&mut child_log)
            .unwrap();
        let rows = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&queued_rows));
        let marker = fs::read(&leader_lock);
        let probe = fs::OpenOptions::new().read(true).open(&leader_lock);
        let ex_busy = probe.as_ref().ok().map(|file| {
            let busy = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0;
            if !busy {
                unsafe {
                    libc::flock(file.as_raw_fd(), libc::LOCK_UN);
                }
            }
            busy
        });
        panic!(
            "finite owner MUST return R before browser/CLI ACK: read={read_error}; pid={}; child_exit={child_status:?}; child_log={}; durable_rows={rows:?}; marker={marker:?}; expected_marker={cli_incarnation:?}; ex_busy={ex_busy:?}",
            cli_owner.0.id(),
            String::from_utf8_lossy(&child_log)
        );
    }
    assert_eq!(
        milestone, *b"R",
        "first CLI did not finish its bounded FIFO drain"
    );
    // The finite owner has returned but its RAII session remains held via IPC.
    // Neither the daemon nor the second CLI can take over to fake these ACKs.
    let deadline = Instant::now() + Duration::from_secs(20);
    let browser_pin = loop {
        let state: serde_json::Value = client
            .get(format!("{url}/api/jobs/{browser_id}"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if state["state"] == "done" {
            break state["revision"].clone();
        }
        assert_ne!(state["state"], "failed", "queued browser row failed");
        assert!(
            Instant::now() < deadline,
            "browser row was not drained by finite owner"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    let second_status = loop {
        if let Some(status) = contender.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "second CLI did not finish under finite leader"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    if !second_status.success() {
        let read_bounded = |path: &std::path::Path| -> String {
            let read = || -> std::io::Result<String> {
                let length = fs::metadata(path)?.len();
                let mut out = Vec::new();
                fs::File::open(path)?
                    .take(64 * 1024)
                    .read_to_end(&mut out)?;
                Ok(format!(
                    "bytes={length} truncated={} text={}",
                    length > 64 * 1024,
                    String::from_utf8_lossy(&out)
                ))
            };
            read().unwrap_or_else(|error| format!("unavailable: {error}"))
        };
        let durable_rows = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&queued_rows));
        let leader_ex = (|| -> std::io::Result<bool> {
            let file = fs::OpenOptions::new().read(true).open(&leader_lock)?;
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result == 0 {
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(false)
            } else {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    Ok(true)
                } else {
                    Err(error)
                }
            }
        })();
        panic!(
            "independent CLI MUST exit success with own FIFO ACK: exit={second_status}; contender_pid={}; stderr={}; stdout={}; durable_rows={:?}; browser_id={browser_id}; contender_row={contender_row:?}; first_owner_pid={}; first_owner_exit={:?}; first_owner_log={}; first_owner_ex_busy={leader_ex:?}; expected_marker={cli_incarnation:?}; current_marker={:?}",
            contender.0.id(),
            read_bounded(&contender_log),
            read_bounded(&contender_stdout),
            durable_rows,
            cli_owner.0.id(),
            cli_owner.0.try_wait(),
            read_bounded(&owner_log),
            fs::read(&leader_lock),
        );
    }
    let second_stdout = fs::read(&contender_stdout).unwrap();
    let second: serde_json::Value = serde_json::from_slice(&second_stdout).unwrap();
    let rows = queued_rows();
    let browser_row = rows.iter().find(|row| row.1 == browser_id).unwrap();
    let second_row = rows.iter().find(|row| row.1 == contender_row.1).unwrap();
    assert_eq!(browser_row.2, "done");
    assert_eq!(second_row.2, "done");
    assert_eq!(
        browser_pin["indexGeneration"],
        browser_row.3.as_ref().unwrap().as_str()
    );
    assert_eq!(browser_pin["indexRevision"], browser_row.4.unwrap());
    assert_eq!(
        second["publishedRevision"]["indexGeneration"],
        second_row.3.as_ref().unwrap().as_str()
    );
    assert_eq!(
        second["publishedRevision"]["indexRevision"],
        second_row.4.unwrap()
    );
    assert!(browser_row.4.unwrap() < second_row.4.unwrap());
    assert!(
        cli_owner.0.try_wait().unwrap().is_none(),
        "first owner exited before competitor ACK"
    );
    let file = fs::OpenOptions::new()
        .read(true)
        .open(&leader_lock)
        .unwrap();
    let still_ex = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0;
    if !still_ex {
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
    }
    assert!(
        still_ex && fs::read(&leader_lock).unwrap() == cli_incarnation,
        "browser and CLI ACKs must precede first owner EX/incarnation release"
    );
    // The first owner's bounded cutoff has passed at R. A fresh edit now
    // MUST be published by the daemon's new EX/incarnation, not the CLI.
    const POST_CUTOFF_SOURCE: &str = "function posthandoff() { return 1; }\n";
    fs::write(root.join("source1.js"), POST_CUTOFF_SOURCE).unwrap();
    channel.write_all(b"D").unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    let first_exit = loop {
        if let Some(status) = cli_owner.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "first CLI owner did not exit after release"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(
        first_exit.success(),
        "first real CLI owner did not exit successfully"
    );
    let own_row = queued_rows()
        .into_iter()
        .find(|row| row.1 == first_row.1)
        .unwrap();
    assert_eq!(own_row.2, "done");
    if !direct_child {
        let first: serde_json::Value =
            serde_json::from_slice(&fs::read(&owner_stdout_path).unwrap()).unwrap();
        assert_eq!(
            first["publishedRevision"]["indexGeneration"],
            own_row.3.as_ref().unwrap().as_str()
        );
        assert_eq!(
            first["publishedRevision"]["indexRevision"],
            own_row.4.unwrap()
        );
    }
    // Transport/body failures are fatal too; they must retain the same bounded
    // takeover diagnostics as a timed-out 200+revision admission.
    macro_rules! unavailable_status {
        ($reason:expr, $code:expr, $raw_body:expr) => {{
            let raw_body: &[u8] = $raw_body;
            let daemon_stderr = (|| -> std::io::Result<String> {
                let len = fs::metadata(&daemon_log)?.len();
                let mut bytes = Vec::new();
                fs::File::open(&daemon_log)?
                    .take(64 * 1024)
                    .read_to_end(&mut bytes)?;
                Ok(format!(
                    "bytes={len} truncated={} text={}",
                    len > 64 * 1024,
                    String::from_utf8_lossy(&bytes)
                ))
            })();
            let rows = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&queued_rows));
            let lock_probe = (|| -> std::io::Result<bool> {
                let file = fs::OpenOptions::new().read(true).open(&leader_lock)?;
                let result =
                    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
                if result == 0 {
                    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(false)
                } else {
                    let error = std::io::Error::last_os_error();
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        Ok(true)
                    } else {
                        Err(error)
                    }
                }
            })();
            let selected = (|| -> anyhow::Result<(String, i64, Option<String>)> {
                let path =
                    index_db_under(&home).ok_or_else(|| anyhow::anyhow!("index.db missing"))?;
                let index = rusqlite::Connection::open_with_flags(
                    path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                )?;
                index.busy_timeout(Duration::ZERO)?;
                Ok(index.query_row(
                    "SELECT index_generation,index_revision,reconcile_options FROM index_metadata WHERE singleton=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?)
            })();
            let current_root = fs::metadata(&root).map(|m| (m.dev(), m.ino()));
            let error_code = serde_json::from_slice::<serde_json::Value>(raw_body)
                .ok().and_then(|value| value.pointer("/error/code").cloned());
            panic!(
                "successor did not open reconciled status: reason={}; http_status={:?}; body_bytes={} body_truncated={} body={}; error_code={error_code:?}; daemon_pid={}; daemon_exit={:?}; daemon_stderr={daemon_stderr:?}; durable_rows={rows:?}; selected_pin_options={selected:?}; own_done={own_row:?}; browser_done={browser_row:?}; contender_done={second_row:?}; expected_root_identity={root_identity:?}; current_root_identity={current_root:?}; expected_old_marker={cli_incarnation:?}; current_marker={:?}; independent_ex_probe={lock_probe:?}",
                $reason, $code, raw_body.len(), raw_body.len() > 64 * 1024,
                String::from_utf8_lossy(&raw_body[..raw_body.len().min(64 * 1024)]),
                daemon.0.id(), daemon.0.try_wait(), fs::read(&leader_lock)
            );
        }};
    }
    // No successor HTTP/status/source read has occurred after D. The daemon
    // must first own a NEW incarnation under the original root identity.
    let takeover_deadline = Instant::now() + Duration::from_secs(30);
    let successor_marker = loop {
        match daemon.0.try_wait() {
            Ok(None) => {}
            Ok(Some(_)) => unavailable_status!(
                "daemon_exited_before_new_EX",
                None::<reqwest::StatusCode>,
                &[]
            ),
            Err(_) => {
                unavailable_status!("daemon_pid_probe_error", None::<reqwest::StatusCode>, &[])
            }
        }
        let current_root = match fs::metadata(&root) {
            Ok(metadata) => metadata,
            Err(_) => unavailable_status!(
                "root_missing_before_new_EX",
                None::<reqwest::StatusCode>,
                &[]
            ),
        };
        if (current_root.dev(), current_root.ino()) != root_identity {
            unavailable_status!(
                "root_changed_before_new_EX",
                None::<reqwest::StatusCode>,
                &[]
            );
        }
        let marker = match fs::read(&leader_lock) {
            Ok(marker) => marker,
            Err(_) => {
                unavailable_status!("leader_marker_unreadable", None::<reqwest::StatusCode>, &[])
            }
        };
        if marker != cli_incarnation {
            // EX is acquired before the owner truncates, writes, and fsyncs its
            // incarnation. Partial/empty marker bytes are intermediate, never
            // an accepted successor; retry until the finite failure deadline.
            let valid = marker.len() == 36
                && std::str::from_utf8(&marker)
                    .ok()
                    .and_then(|text| uuid::Uuid::parse_str(text).ok())
                    .is_some();
            if valid {
                let probe = match fs::OpenOptions::new().read(true).open(&leader_lock) {
                    Ok(probe) => probe,
                    Err(_) => unavailable_status!(
                        "new_EX_probe_open_error",
                        None::<reqwest::StatusCode>,
                        &[]
                    ),
                };
                let result =
                    unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
                if result == 0 {
                    if unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_UN) } != 0 {
                        unavailable_status!(
                            "new_EX_probe_unlock_error",
                            None::<reqwest::StatusCode>,
                            &[]
                        );
                    }
                    unavailable_status!(
                        "new_marker_without_live_EX",
                        None::<reqwest::StatusCode>,
                        &[]
                    );
                }
                let errno = std::io::Error::last_os_error().raw_os_error();
                if errno != Some(libc::EWOULDBLOCK) && errno != Some(libc::EAGAIN) {
                    unavailable_status!("new_EX_probe_error", None::<reqwest::StatusCode>, &[]);
                }
                break marker;
            }
        }
        if Instant::now() >= takeover_deadline {
            unavailable_status!("new_EX_deadline", None::<reqwest::StatusCode>, &[]);
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    assert_ne!(successor_marker, cli_incarnation);
    // Browser ingress uses `client`; successor reads get their own client and
    // explicit per-request bounds through response-body completion.
    let successor_client = reqwest::Client::new();
    macro_rules! require_live_successor {
        ($phase:expr, $code:expr, $body:expr) => {{
            match daemon.0.try_wait() {
                Ok(None) => {}
                Ok(Some(_)) => {
                    unavailable_status!(format!("{}_daemon_exited", $phase), $code, $body)
                }
                Err(_) => {
                    unavailable_status!(format!("{}_daemon_pid_probe_error", $phase), $code, $body)
                }
            }
            let current_root = match fs::metadata(&root) {
                Ok(metadata) => metadata,
                Err(_) => unavailable_status!(format!("{}_root_missing", $phase), $code, $body),
            };
            if (current_root.dev(), current_root.ino()) != root_identity {
                unavailable_status!(format!("{}_root_changed", $phase), $code, $body);
            }
            let current_marker = match fs::read(&leader_lock) {
                Ok(marker) => marker,
                Err(_) => {
                    unavailable_status!(format!("{}_marker_unreadable", $phase), $code, $body)
                }
            };
            if current_marker != successor_marker {
                unavailable_status!(format!("{}_incarnation_changed", $phase), $code, $body);
            }
            let probe = match fs::OpenOptions::new().read(true).open(&leader_lock) {
                Ok(probe) => probe,
                Err(_) => {
                    unavailable_status!(format!("{}_EX_probe_open_error", $phase), $code, $body)
                }
            };
            let result = unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result == 0 {
                if unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_UN) } != 0 {
                    unavailable_status!(format!("{}_EX_unlock_error", $phase), $code, $body);
                }
                unavailable_status!(format!("{}_EX_not_held", $phase), $code, $body);
            }
            let errno = std::io::Error::last_os_error().raw_os_error();
            if errno != Some(libc::EWOULDBLOCK) && errno != Some(libc::EAGAIN) {
                unavailable_status!(format!("{}_EX_probe_error", $phase), $code, $body);
            }
        }};
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let after = loop {
        let response = match successor_client
            .get(format!("{url}/api/status"))
            .bearer_auth(TOKEN)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => unavailable_status!(
                format!("transport_error: {error}"),
                None::<reqwest::StatusCode>,
                &[]
            ),
        };
        let http_status = response.status();
        let body = match response.bytes().await {
            Ok(body) => body,
            Err(error) => {
                unavailable_status!(format!("body_error: {error}"), Some(http_status), &[])
            }
        };
        let parsed = serde_json::from_slice::<serde_json::Value>(&body);
        if http_status == reqwest::StatusCode::OK {
            let status = match &parsed {
                Ok(status) => status.clone(),
                Err(_) => unavailable_status!("invalid_status_200", Some(http_status), &body),
            };
            require_live_successor!("status_200", Some(http_status), &body);
            let pin = &status["revision"];
            let observed_revision = pin["indexRevision"].as_u64();
            let expected_generation = own_row.3.as_ref().unwrap();
            if status["workspaceRoot"].as_str() != Some(expected_root.as_str())
                || pin["indexGeneration"].as_str() != Some(expected_generation.as_str())
                || second_row.3.as_ref() != Some(expected_generation)
                || browser_row.3.as_ref() != Some(expected_generation)
                || !observed_revision.is_some_and(|revision| {
                    revision > own_row.4.unwrap() as u64
                        && revision > browser_row.4.unwrap() as u64
                        && revision > second_row.4.unwrap() as u64
                })
            {
                unavailable_status!("stale_or_wrong_root_status_200", Some(http_status), &body);
            }
            break status;
        }
        // Both exact typed BUSY responses can be transient while the verified
        // successor writes selected status. The direct SQLite BUSY mapping says
        // "Storage is busy"; a guarded storage_busy refusal says "Storage is
        // unavailable". Neither admits a raw SQLite stderr or a different 409.
        let storage_busy = http_status == reqwest::StatusCode::CONFLICT
            && parsed.as_ref().is_ok_and(|value| {
                value
                    == &serde_json::json!({
                        "error": {"code": "storage_busy", "message": "Storage is unavailable"}
                    })
                    || value
                        == &serde_json::json!({
                            "error": {"code": "storage_busy", "message": "Storage is busy"}
                        })
            });
        let error_code = parsed.ok().and_then(|value| {
            value
                .pointer("/error/code")
                .and_then(|code| code.as_str())
                .map(str::to_owned)
        });
        let index_not_ready = http_status == reqwest::StatusCode::SERVICE_UNAVAILABLE
            && error_code.as_deref() == Some("index_not_ready");
        if storage_busy {
            require_live_successor!("status_409_storage_busy", Some(http_status), &body);
        } else if !index_not_ready {
            unavailable_status!("unexpected_nonready_status", Some(http_status), &body);
        }
        if Instant::now() >= deadline {
            unavailable_status!(
                if storage_busy {
                    "storage_busy_deadline"
                } else {
                    "index_not_ready_deadline"
                },
                Some(http_status),
                &body
            );
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    // Authenticate source bytes AT THE SAME HTTP PIN, before any CLI export.
    let pin = &after["revision"];
    let revision_text = match pin["indexRevision"].as_u64() {
        Some(revision) => revision.to_string(),
        None => unavailable_status!(
            "status_200_missing_pin_revision",
            Some(reqwest::StatusCode::OK),
            &[]
        ),
    };
    let generation = match pin["indexGeneration"].as_str() {
        Some(generation) => generation,
        None => unavailable_status!(
            "status_200_missing_pin_generation",
            Some(reqwest::StatusCode::OK),
            &[]
        ),
    };
    let source_response = match successor_client
        .get(format!("{url}/api/source"))
        .bearer_auth(TOKEN)
        .timeout(Duration::from_secs(10))
        .query(&[
            ("path", "source1.js"),
            ("indexGeneration", generation),
            ("indexRevision", revision_text.as_str()),
        ])
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => unavailable_status!(
            format!("source_transport_error: {error}"),
            None::<reqwest::StatusCode>,
            &[]
        ),
    };
    let source_status = source_response.status();
    let source_body = match source_response.bytes().await {
        Ok(body) => body,
        Err(error) => unavailable_status!(
            format!("source_body_error: {error}"),
            Some(source_status),
            &[]
        ),
    };
    if source_status != reqwest::StatusCode::OK {
        unavailable_status!("pinned_source_not_ready", Some(source_status), &source_body);
    }
    require_live_successor!("source_200", Some(source_status), &source_body);
    let source: serde_json::Value = match serde_json::from_slice(&source_body) {
        Ok(source) => source,
        Err(_) => unavailable_status!("invalid_source_200", Some(source_status), &source_body),
    };
    if source["revision"] != *pin
        || source["file"]["path"] != "source1.js"
        || source["file"]["text"] != POST_CUTOFF_SOURCE
    {
        unavailable_status!(
            "stale_or_wrong_pinned_source_200",
            Some(source_status),
            &source_body
        );
    }
    let export = cli(&root, &home, "export").output().unwrap();
    assert!(
        export.status.success(),
        "export after pinned successor HTTP source must succeed"
    );
    let live: serde_json::Value = serde_json::from_slice(&export.stdout).unwrap();
    assert!(
        live["files"].as_array().is_some_and(|files| files
            .iter()
            .any(|file| file["path"] == "source1.js" && file["text"] == POST_CUTOFF_SOURCE)),
        "export must retain the HTTP-proven post-cutoff source"
    );
    let cold_root = temp.path().join("cold-root");
    let cold_home = temp.path().join("cold-home");
    fs::create_dir(&cold_root).unwrap();
    fs::create_dir(&cold_home).unwrap();
    for entry in fs::read_dir(&root).unwrap().flatten() {
        if entry.path().is_file() {
            fs::copy(entry.path(), cold_root.join(entry.file_name())).unwrap();
        }
    }
    assert!(
        cli(&cold_root, &cold_home, "index")
            .output()
            .unwrap()
            .status
            .success()
    );
    let cold_export = cli(&cold_root, &cold_home, "export").output().unwrap();
    assert!(cold_export.status.success());
    let cold: serde_json::Value = serde_json::from_slice(&cold_export.stdout).unwrap();
    assert_eq!(
        live["files"], cold["files"],
        "live native graph differs from cold full rebuild"
    );
}

#[tokio::test]
async fn real_serve_pending_takeover_preserves_h_before_distinct_a_b_ack_pins() {
    use baleyg::{
        indexer::ReconcileOptions,
        model::IndexPin,
        store::topology::{TopologyRoots, WorkspaceIdentity},
    };
    use std::os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, PermissionsExt},
    };
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const BEFORE: &str = "function a() {}\n";
    const AFTER: &str = "function b() {}\n";
    assert!(BEFORE.len() < 32 && AFTER.len() < 32);
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&home).unwrap();
    fs::write(root.join("a.js"), BEFORE).unwrap();
    let original_root = fs::metadata(&root).unwrap();
    let original_identity = (original_root.dev(), original_root.ino());
    let seed = cli(&root, &home, "index")
        .arg("--max-file-bytes")
        .arg("128")
        .output()
        .unwrap();
    assert!(seed.status.success(), "real H seed index must succeed");
    let seed_json: serde_json::Value = serde_json::from_slice(&seed.stdout).unwrap();
    let seed_pin: IndexPin =
        serde_json::from_value(seed_json["publishedRevision"].clone()).unwrap();
    let expected_root = seed_json["status"]["workspaceRoot"]
        .as_str()
        .unwrap()
        .to_owned();
    let index_path = index_db_under(&home).unwrap();
    let cache = index_path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let identity = WorkspaceIdentity::discover(Some(&root), &root).unwrap();
    let roots = TopologyRoots::isolated_for_tests(cache, home.join("unused-record-root"));
    assert_eq!(roots.index_db(&identity), index_path);
    let leader_lock = roots.leader_lock(&identity);
    let store = Store::open(roots, identity).unwrap();
    let predecessor = store.leader_session().unwrap();
    let predecessor_marker = fs::read(&leader_lock).unwrap();
    let mut h = IndexOptions::new(root.canonicalize().unwrap());
    h.max_file_bytes = 128;
    let mut a = h.clone();
    a.max_file_bytes = 32;
    let mut b = h.clone();
    b.max_file_bytes = 512;
    let recorded_h = store.recorded_index_options().unwrap().unwrap();
    assert_eq!(recorded_h.workspace_root, h.workspace_root);
    assert_eq!(
        ReconcileOptions::from(&recorded_h),
        ReconcileOptions::from(&h)
    );
    // The CLI seed belongs to an exited incarnation. This held EX owner must
    // first publish its own H head, or a real serve follower cannot verify it.
    let predecessor_pin = index_coordinator::IndexJobCoordinator::prepare_with_session(
        &store,
        None,
        predecessor.clone(),
    )
    .unwrap()
    .run(&h, &Arc::new(AtomicBool::new(false)), |_| {})
    .unwrap();
    assert_eq!(predecessor_pin.index_generation, seed_pin.index_generation);
    assert!(predecessor_pin.index_revision > seed_pin.index_revision);
    predecessor.verify().unwrap();
    assert_eq!(fs::read(&leader_lock).unwrap(), predecessor_marker);
    let predecessor_read = store.evidence_response().unwrap();
    predecessor_read.validate_pin(predecessor_pin).unwrap();
    let predecessor_status = predecessor_read.status().unwrap();
    assert_eq!(predecessor_status.workspace_root, expected_root);
    assert_eq!(predecessor_status.revision, predecessor_pin);
    assert_eq!(
        predecessor_read
            .source_at("a.js", Some(predecessor_pin))
            .unwrap()
            .unwrap()
            .1
            .text,
        BEFORE
    );
    predecessor_read.finish(()).unwrap();
    drop(predecessor_read);
    let persisted_h = store.recorded_index_options().unwrap().unwrap();
    assert_eq!(persisted_h.workspace_root, h.workspace_root);
    assert_eq!(
        ReconcileOptions::from(&persisted_h),
        ReconcileOptions::from(&h)
    );
    let token = home.join("token");
    fs::write(&token, TOKEN).unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let daemon_stderr = temp.path().join("successor-stderr.log");
    let process = cli(&root, &home, "serve")
        .arg("--max-file-bytes")
        .arg("256") // distinct daemon defaults
        .arg("--bind")
        .arg(address.to_string())
        .arg("--token-file")
        .arg(&token)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(
            fs::File::create(&daemon_stderr).unwrap(),
        ))
        .spawn()
        .unwrap();
    let mut successor = Server(process);
    let client = reqwest::Client::new();
    let url = format!("http://{address}");
    let ready_until = Instant::now() + Duration::from_secs(12);
    loop {
        if client
            .get(format!("{url}/healthz"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            break;
        }
        assert!(
            successor.0.try_wait().unwrap().is_none(),
            "real follower exited before binding"
        );
        assert!(Instant::now() < ready_until, "real follower never bound");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let q1 = store.enqueue_request(&a, None).unwrap();
    let q2 = store.enqueue_request(&b, None).unwrap();
    assert!(q1.seq < q2.seq && q1.id != q2.id);
    let expected_h = serde_json::to_string(&ReconcileOptions::from(&h)).unwrap();
    let expected_a = serde_json::to_string(&ReconcileOptions::from(&a)).unwrap();
    let expected_b = serde_json::to_string(&ReconcileOptions::from(&b)).unwrap();
    assert_eq!(q1.options_json, expected_a);
    assert_eq!(q2.options_json, expected_b);
    assert_eq!(
        store.request_by_id(&q1.id).unwrap().unwrap().state,
        "queued"
    );
    assert_eq!(
        store.request_by_id(&q2.id).unwrap().unwrap().state,
        "queued"
    );
    // Keep all failure evidence private and bounded; never print source paths or token.
    macro_rules! fail_observation {
        ($reason:expr, $code:expr, $body:expr) => {{
            let body: &[u8] = $body;
            let log = (|| -> std::io::Result<(u64, String)> {
                use std::io::Read;
                let size = fs::metadata(&daemon_stderr)?.len();
                let mut bytes = Vec::new();
                fs::File::open(&daemon_stderr)?.take(64 * 1024).read_to_end(&mut bytes)?;
                Ok((size, String::from_utf8_lossy(&bytes).into_owned()))
            })();
            let root_now = fs::metadata(&root).map(|m| (m.dev(), m.ino()));
            let marker_now = fs::read(&leader_lock);
            let rows = (store.request_by_id(&q1.id), store.request_by_id(&q2.id));
            panic!("real H/A/B observation failed: reason={}; http={:?}; body_len={} body_prefix={}; daemon_pid={}; daemon_exit={:?}; daemon_log={log:?}; root_expected={original_identity:?}; root_now={root_now:?}; old_marker={predecessor_marker:?}; marker_now={marker_now:?}; rows={rows:?}",
                $reason, $code, body.len(), String::from_utf8_lossy(&body[..body.len().min(64 * 1024)]),
                successor.0.id(), successor.0.try_wait());
        }};
    }
    fs::write(root.join("a.js"), AFTER).unwrap();
    drop(predecessor);
    // Cumulative acceptance policy, checked between operations. A single
    // synchronous SQLite call may straddle this time; the external process
    // group watchdog is the separate hard wall for the test process.
    let observation_until = Instant::now() + Duration::from_secs(30);
    macro_rules! observation_time {
        ($phase:expr) => {{
            if Instant::now() >= observation_until {
                fail_observation!(
                    format!("{}_observation_deadline", $phase),
                    None::<reqwest::StatusCode>,
                    &[]
                );
            }
        }};
    }
    let (q1_done, q2_done, new_marker) = loop {
        observation_time!("queue_attempt");
        match successor.0.try_wait() {
            Ok(None) => {}
            _ => fail_observation!(
                "successor_exited_before_fifo_done",
                None::<reqwest::StatusCode>,
                &[]
            ),
        }
        let first = match store.request_by_id(&q1.id) {
            Ok(Some(row)) => row,
            Err(error)
                if baleyg::store::terminal_status_sqlite_contention(&error)
                    && Instant::now() < observation_until =>
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            }
            _ => fail_observation!("q1_row_read_failed", None::<reqwest::StatusCode>, &[]),
        };
        let second = match store.request_by_id(&q2.id) {
            Ok(Some(row)) => row,
            Err(error)
                if baleyg::store::terminal_status_sqlite_contention(&error)
                    && Instant::now() < observation_until =>
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            }
            _ => fail_observation!("q2_row_read_failed", None::<reqwest::StatusCode>, &[]),
        };
        if first.state == "failed" || second.state == "failed" {
            fail_observation!("accepted_fifo_row_failed", None::<reqwest::StatusCode>, &[]);
        }
        let marker = match fs::read(&leader_lock) {
            Ok(marker) => marker,
            Err(_) => fail_observation!("marker_read_failed", None::<reqwest::StatusCode>, &[]),
        };
        if first.state == "done" && second.state == "done" {
            observation_time!("queue_done_late_accept");
            if marker == predecessor_marker
                || marker.len() != 36
                || std::str::from_utf8(&marker)
                    .ok()
                    .and_then(|text| uuid::Uuid::parse_str(text).ok())
                    .is_none()
            {
                fail_observation!(
                    "done_without_valid_new_marker",
                    None::<reqwest::StatusCode>,
                    &[]
                );
            }
            observation_time!("queue_done_after_marker_late_accept");
            break (first, second, marker);
        }
        if Instant::now() >= observation_until {
            fail_observation!(
                "accepted_rows_not_done_deadline",
                None::<reqwest::StatusCode>,
                &[]
            );
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    macro_rules! verify_successor {
        ($reason:expr, $code:expr, $body:expr) => {{
            if !matches!(successor.0.try_wait(), Ok(None)) {
                fail_observation!(format!("{}_daemon_exit", $reason), $code, $body);
            }
            let root_now = match fs::metadata(&root) {
                Ok(meta) => meta,
                Err(_) => fail_observation!(format!("{}_root_missing", $reason), $code, $body),
            };
            if (root_now.dev(), root_now.ino()) != original_identity {
                fail_observation!(format!("{}_root_changed", $reason), $code, $body);
            }
            if fs::read(&leader_lock).ok().as_deref() != Some(new_marker.as_slice()) {
                fail_observation!(format!("{}_marker_changed", $reason), $code, $body);
            }
            let probe = match fs::OpenOptions::new().read(true).open(&leader_lock) {
                Ok(probe) => probe,
                Err(_) => fail_observation!(format!("{}_EX_open_failed", $reason), $code, $body),
            };
            let locked = unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if locked == 0 {
                if unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_UN) } != 0 {
                    fail_observation!(format!("{}_EX_unlock_failed", $reason), $code, $body);
                }
                fail_observation!(format!("{}_EX_not_held", $reason), $code, $body);
            }
            let errno = std::io::Error::last_os_error().raw_os_error();
            if errno != Some(libc::EWOULDBLOCK) && errno != Some(libc::EAGAIN) {
                fail_observation!(format!("{}_EX_error", $reason), $code, $body);
            }
        }};
    }
    verify_successor!("before_http", None::<reqwest::StatusCode>, &[]);
    let a_pin = q1_done.revision.unwrap();
    let b_pin = q2_done.revision.unwrap();
    assert_eq!(q1_done.id, q1.id);
    assert_eq!(q2_done.id, q2.id);
    assert_eq!(q1_done.seq, q1.seq);
    assert_eq!(q2_done.seq, q2.seq);
    assert_eq!(q1_done.options_json, expected_a);
    assert_eq!(q2_done.options_json, expected_b);
    assert!(q1_done.error_code.is_none() && q2_done.error_code.is_none());
    assert_eq!(a_pin.index_generation, seed_pin.index_generation);
    assert_eq!(b_pin.index_generation, a_pin.index_generation);
    assert_eq!(b_pin.index_revision, a_pin.index_revision + 1);
    assert!(
        a_pin.index_revision > predecessor_pin.index_revision + 1,
        "mandatory successor H publication after edit must precede Q1 A"
    );
    let h_pin = IndexPin {
        index_generation: a_pin.index_generation,
        index_revision: a_pin.index_revision - 1,
    };
    assert!(h_pin.index_revision > predecessor_pin.index_revision);
    let status = loop {
        observation_time!("status_attempt");
        verify_successor!("status_attempt", None::<reqwest::StatusCode>, &[]);
        let response = match client
            .get(format!("{url}/api/status"))
            .bearer_auth(TOKEN)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => fail_observation!("status_transport_error", None::<reqwest::StatusCode>, &[]),
        };
        let code = response.status();
        let body = match response.bytes().await {
            Ok(body) => body,
            Err(_) => fail_observation!("status_body_error", Some(code), &[]),
        };
        let value: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(_) => fail_observation!("invalid_status_json", Some(code), &body),
        };
        if code == reqwest::StatusCode::OK {
            if Instant::now() >= observation_until {
                fail_observation!("status_200_late_accept", Some(code), &body);
            }
            verify_successor!("status_200", Some(code), &body);
            if value["workspaceRoot"].as_str() != Some(expected_root.as_str())
                || value["revision"] != serde_json::to_value(b_pin).unwrap()
            {
                fail_observation!("stale_or_wrong_status_200", Some(code), &body);
            }
            if Instant::now() >= observation_until {
                fail_observation!("status_200_after_guard_late_accept", Some(code), &body);
            }
            break value;
        }
        let error_code = value.pointer("/error/code").and_then(|code| code.as_str());
        // Only the audited application-level contention shape may retry.
        // A direct raw SQLite BUSY maps to a different message and must RED.
        let exact_busy =
            serde_json::json!({"error":{"code":"storage_busy","message":"Storage is unavailable"}});
        let allowed = (code == reqwest::StatusCode::CONFLICT && value == exact_busy)
            || (code == reqwest::StatusCode::SERVICE_UNAVAILABLE
                && error_code == Some("index_not_ready"));
        if !allowed || Instant::now() >= observation_until {
            fail_observation!("unexpected_status_or_deadline", Some(code), &body);
        }
        verify_successor!("status_retry", Some(code), &body);
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    assert_eq!(status["revision"], serde_json::to_value(b_pin).unwrap());
    for pin in [h_pin, a_pin, b_pin] {
        let revision_text = pin.index_revision.to_string();
        let generation_text = pin.index_generation.to_string();
        loop {
            observation_time!("historical_source_attempt");
            verify_successor!(
                "historical_source_attempt",
                None::<reqwest::StatusCode>,
                &[]
            );
            let response = match client
                .get(format!("{url}/api/source"))
                .bearer_auth(TOKEN)
                .timeout(Duration::from_secs(10))
                .query(&[
                    ("path", "a.js"),
                    ("indexGeneration", generation_text.as_str()),
                    ("indexRevision", revision_text.as_str()),
                ])
                .send()
                .await
            {
                Ok(response) => response,
                Err(_) => fail_observation!(
                    "historical_source_transport_error",
                    None::<reqwest::StatusCode>,
                    &[]
                ),
            };
            let code = response.status();
            let body = match response.bytes().await {
                Ok(body) => body,
                Err(_) => fail_observation!("historical_source_body_error", Some(code), &[]),
            };
            let source: serde_json::Value = match serde_json::from_slice(&body) {
                Ok(value) => value,
                Err(_) => fail_observation!("historical_source_invalid_json", Some(code), &body),
            };
            if code == reqwest::StatusCode::OK {
                if Instant::now() >= observation_until {
                    fail_observation!("historical_source_200_late_accept", Some(code), &body);
                }
                verify_successor!("historical_source_200", Some(code), &body);
                if source["revision"] != serde_json::to_value(pin).unwrap()
                    || source["file"]["path"] != "a.js"
                    || source["file"]["text"] != AFTER
                {
                    fail_observation!("wrong_historical_pin_or_source_200", Some(code), &body);
                }
                if Instant::now() >= observation_until {
                    fail_observation!(
                        "historical_source_200_after_guard_late_accept",
                        Some(code),
                        &body
                    );
                }
                break;
            }
            let exact_busy = serde_json::json!({"error":{"code":"storage_busy","message":"Storage is unavailable"}});
            let typed_busy = code == reqwest::StatusCode::CONFLICT && source == exact_busy;
            if !typed_busy || Instant::now() >= observation_until {
                fail_observation!("historical_source_nonbusy_or_deadline", Some(code), &body);
            }
            verify_successor!("historical_source_retry", Some(code), &body);
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }
    // Product HTTP has authenticated every exact historical pin and source.
    // Now read one immutable native-revision options snapshot; fail named on
    // SQL contention rather than converting it to proof of a selected head.
    observation_time!("native_options_before_open");
    verify_successor!("native_options", None::<reqwest::StatusCode>, &[]);
    let sql_failure = |stage: &str, error: &rusqlite::Error| {
        let code = match error {
            rusqlite::Error::SqliteFailure(info, _) => {
                format!("{:?}/{}", info.code, info.extended_code)
            }
            _ => "other".to_owned(),
        };
        format!("{stage}_sqlite_{code}")
    };
    let index = match rusqlite::Connection::open_with_flags(
        &index_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(index) => index,
        Err(error) => fail_observation!(
            sql_failure("native_open", &error),
            None::<reqwest::StatusCode>,
            &[]
        ),
    };
    if let Err(error) = index.busy_timeout(Duration::ZERO) {
        fail_observation!(
            sql_failure("native_busy_timeout", &error),
            None::<reqwest::StatusCode>,
            &[]
        );
    }
    observation_time!("native_options_before_begin");
    if let Err(error) = index.execute_batch("BEGIN DEFERRED") {
        fail_observation!(
            sql_failure("native_begin", &error),
            None::<reqwest::StatusCode>,
            &[]
        );
    }
    observation_time!("native_selected_before_read");
    let selected: (String, i64, String) = match index.query_row(
        "SELECT index_generation,index_revision,reconcile_options FROM index_metadata WHERE singleton=1", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))) {
        Ok(selected) => selected,
        Err(error) => fail_observation!(sql_failure("native_selected", &error), None::<reqwest::StatusCode>, &[]),
    };
    observation_time!("native_selected_late_accept");
    assert_eq!(
        selected,
        (
            b_pin.index_generation.to_string(),
            b_pin.index_revision as i64,
            expected_b.clone()
        )
    );
    for (label, pin, options) in [
        ("H", h_pin, expected_h),
        ("Q1/A", a_pin, expected_a),
        ("Q2/B", b_pin, expected_b),
    ] {
        observation_time!("native_revision_before_read");
        let key = format!("pin:v1:{}:{}", pin.index_generation, pin.index_revision);
        let actual: String = match index.query_row(
            "SELECT reconcile_options FROM native_revisions WHERE id=?1 AND published_index_revision=?2",
            rusqlite::params![key, pin.index_revision as i64], |row| row.get(0)) {
            Ok(actual) => actual,
            Err(error) => fail_observation!(sql_failure("native_revision", &error), None::<reqwest::StatusCode>, &[]),
        };
        observation_time!("native_revision_late_accept");
        assert_eq!(
            actual, options,
            "{label} exact pin {key} must retain its canonical publication options"
        );
    }
    if let Err(error) = index.execute_batch("ROLLBACK") {
        fail_observation!(
            sql_failure("native_rollback", &error),
            None::<reqwest::StatusCode>,
            &[]
        );
    }
    observation_time!("final_owner_proof");
    verify_successor!("final", None::<reqwest::StatusCode>, &[]);
    observation_time!("final_late_accept");
}

#[test]
fn empty_checkout_final_inventory_preserves_explicit_pin() {
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let options = IndexOptions::new(workspace.path().to_owned());
    let cancel = Arc::new(AtomicBool::new(false));
    let (pin, owner) = index_coordinator::enqueue_and_wait(&store, &options, &cancel).unwrap();
    assert!(owner.is_leader());
    assert_eq!(
        store.status().unwrap().revision,
        pin,
        "verified empty final inventory must not publish a redundant head"
    );
    let row = store.current_request().unwrap().unwrap();
    assert_eq!(row.state, "done");
    assert_eq!(row.revision, Some(pin));
}
