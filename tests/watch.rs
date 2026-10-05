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
    use std::os::unix::net::UnixStream;
    let Ok(root) = std::env::var("BALEYG_TEST_FINITE_CLI_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let socket = std::env::var("BALEYG_TEST_FINITE_CLI_SOCKET").unwrap();
    let roots = baleyg::store::topology::TopologyRoots::production().unwrap();
    let identity = baleyg::store::topology::WorkspaceIdentity::discover(
        Some(&root),
        &std::env::current_dir().unwrap(),
    )
    .unwrap();
    roots.reject_root_overlap(&identity).unwrap();
    let store = Store::open(roots, identity).unwrap();
    let options = IndexOptions::new(root);
    let channel = std::sync::Mutex::new(UnixStream::connect(socket).unwrap());
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
        unsafe {
            libc::kill(self.0.id() as i32, libc::SIGCONT);
        }
        let _ = self.0.kill();
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

#[tokio::test]
async fn cli_daemon_edit_during_cli_leadership_then_handoff_matches_cold_full() {
    use std::{os::fd::AsRawFd, os::unix::fs::PermissionsExt};
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&home).unwrap();
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
    fs::write(
        root.join("source0.js"),
        "function edited() { return 999; }\n",
    )
    .unwrap();
    let socket_path = temp.path().join("finite-cli.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("finite_cli_owner_child")
        .env("HOME", &home)
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_DATA_HOME")
        .env("BALEYG_TEST_FINITE_CLI_ROOT", &root)
        .env("BALEYG_TEST_FINITE_CLI_SOCKET", &socket_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut cli_owner = Server(child);
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    let (mut channel, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "finite CLI child did not connect"
                );
                assert!(
                    cli_owner.0.try_wait().unwrap().is_none(),
                    "finite CLI child exited before connect"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("finite CLI child socket failed: {error}"),
        }
    };
    channel.set_nonblocking(false).unwrap();
    channel
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    channel
        .set_write_timeout(Some(Duration::from_secs(20)))
        .unwrap();
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
    let first_row = queued_rows()
        .into_iter()
        .find(|row| row.2 == "queued")
        .unwrap();
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
        .stderr(std::process::Stdio::null())
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
    let contender_process = cli(&root, &home, "index")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
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
    channel.read_exact(&mut milestone).unwrap();
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
    assert!(
        second_status.success(),
        "independent CLI failed despite accepted FIFO row"
    );
    let mut second_stdout = Vec::new();
    contender
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut second_stdout)
        .unwrap();
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
    channel.write_all(b"D").unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    while cli_owner.0.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "first CLI owner did not exit after release"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let after = loop {
        let response = client
            .get(format!("{url}/api/status"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap();
        let status: serde_json::Value = response.json().await.unwrap();
        if status["revision"]["indexRevision"].as_u64().is_some() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "successor did not open reconciled status"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    assert!(after["revision"]["indexRevision"].as_u64().unwrap() >= 3);
    fs::write(
        root.join("source1.js"),
        "function posthandoff() { return 1; }\n",
    )
    .unwrap();
    let revision = after["revision"].clone();
    let deadline = Instant::now() + Duration::from_secs(12);
    let live = loop {
        let current: serde_json::Value = client
            .get(format!("{url}/api/status"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if current["revision"]["indexRevision"].as_u64().is_some()
            && current["revision"] != revision
        {
            let export = cli(&root, &home, "export").output().unwrap();
            if export.status.success() {
                let graph: serde_json::Value = serde_json::from_slice(&export.stdout).unwrap();
                if graph["files"].as_array().is_some_and(|files| {
                    files.iter().any(|file| {
                        file["path"] == "source1.js"
                            && file["text"] == "function posthandoff() { return 1; }\n"
                    })
                }) {
                    break graph;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "daemon successor did not watch post-handoff edit"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
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
