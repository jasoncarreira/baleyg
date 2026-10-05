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
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    fs::write(root.join("a.js"), "function after() {}\n").unwrap();
    let takeover = cli(&root, &home, "index").output().unwrap();
    assert!(
        takeover.status.success(),
        "successor did not reconcile missed edit"
    );
    let result: serde_json::Value = serde_json::from_slice(&takeover.stdout).unwrap();
    assert!(
        result["publishedRevision"]["indexRevision"]
            .as_u64()
            .unwrap()
            >= 2
    );
    assert_eq!(result["publishedRevision"], result["status"]["revision"]);
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
    let cli_log = temp.path().join("cli-stderr");
    let cli_child = cli(&root, &home, "index")
        .env("BALEYG_INDEX_DIAGNOSTICS", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(
            fs::File::create(&cli_log).unwrap(),
        ))
        .spawn()
        .unwrap();
    let mut cli_owner = Server(cli_child);
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut held = false;
    while Instant::now() < deadline {
        if let Some(path) = leader_lock_under(&home) {
            let file = fs::OpenOptions::new().read(true).open(path).unwrap();
            let locked =
                unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0;
            if !locked {
                unsafe {
                    libc::flock(file.as_raw_fd(), libc::LOCK_UN);
                }
            }
            if locked
                && fs::read_to_string(&cli_log)
                    .unwrap()
                    .contains("index-phase publish_ms=")
            {
                unsafe {
                    libc::kill(cli_owner.0.id() as i32, libc::SIGSTOP);
                }
                held = true;
                break;
            }
        }
        assert!(
            cli_owner.0.try_wait().unwrap().is_none(),
            "CLI left before leader proof"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(held, "CLI leader lock never observed");
    let leader_lock = leader_lock_under(&home).unwrap();
    let cli_incarnation = fs::read(&leader_lock).unwrap();
    assert_eq!(
        cli_incarnation.len(),
        36,
        "CLI must sync its leader incarnation before follower starts"
    );
    uuid::Uuid::parse_str(std::str::from_utf8(&cli_incarnation).unwrap()).unwrap();
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
    fs::write(
        root.join("source0.js"),
        "function edited() { return 999; }\n",
    )
    .unwrap();
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
    unsafe {
        libc::kill(cli_owner.0.id() as i32, libc::SIGCONT);
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut done_under_cli_lock = false;
    while Instant::now() < deadline {
        let state: serde_json::Value = client
            .get(format!("{url}/api/jobs/{}", job["id"].as_str().unwrap()))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if state["state"] == "done" {
            let cli_alive = cli_owner.0.try_wait().unwrap().is_none();
            let file = fs::OpenOptions::new()
                .read(true)
                .open(&leader_lock)
                .unwrap();
            let leader_ex =
                unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0;
            if !leader_ex {
                unsafe {
                    libc::flock(file.as_raw_fd(), libc::LOCK_UN);
                }
            }
            done_under_cli_lock =
                cli_alive && leader_ex && fs::read(&leader_lock).unwrap() == cli_incarnation;
            break;
        }
        assert_ne!(state["state"], "failed", "queued browser row failed");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        done_under_cli_lock,
        "browser row must finish while real CLI process still owns EX"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && cli_owner.0.try_wait().unwrap().is_none() {
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert!(
        cli_owner.0.try_wait().unwrap().unwrap().success(),
        "finite CLI must exit"
    );
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
