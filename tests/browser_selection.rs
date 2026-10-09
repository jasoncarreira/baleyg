use baleyg::store::topology::WorkspaceIdentity;
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
    time::Duration,
};
use tempfile::TempDir;

fn command(home: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_baleyg"));
    cmd.env("HOME", home)
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_DATA_HOME");
    cmd
}

fn git(root: &std::path::Path, args: &[&str]) {
    let result = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

struct Server(std::process::Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn production_browser_selects_two_real_worktrees_without_global_attachment() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    fs::create_dir(&home).unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    fs::create_dir(&a).unwrap();
    git(&a, &["init", "-q"]);
    git(&a, &["config", "user.email", "test@example.invalid"]);
    git(&a, &["config", "user.name", "Test"]);
    fs::write(
        a.join("core.js"),
        "function alpha_checkout() { return 1; }\n",
    )
    .unwrap();
    git(&a, &["add", "core.js"]);
    git(&a, &["commit", "-qm", "base"]);
    git(
        &a,
        &["worktree", "add", "-qb", "other", b.to_str().unwrap()],
    );
    fs::write(
        b.join("core.js"),
        "function beta_checkout() { return 2; }\n",
    )
    .unwrap();
    for root in [&a, &b] {
        let result = command(&home)
            .arg("index")
            .arg("--workspace")
            .arg(root)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "index: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let a_key = WorkspaceIdentity::discover(Some(&a), &a).unwrap().root_key;
    let b_key = WorkspaceIdentity::discover(Some(&b), &b).unwrap().root_key;
    assert_ne!(a_key, b_key);
    let token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let token_file = home.join("token");
    fs::write(&token_file, token).unwrap();
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let log = temp.path().join("serve-stderr");
    let child = command(&home)
        .arg("serve")
        .arg("--workspace")
        .arg(&a)
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--token-file")
        .arg(token_file)
        .stdout(Stdio::null())
        .stderr(Stdio::from(fs::File::create(&log).unwrap()))
        .spawn()
        .unwrap();
    let mut server = Server(child);
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            if let Ok(response) = client.get(format!("{base}/healthz")).send().await
                && response.status().is_success()
            {
                break;
            }
            assert!(
                server.0.try_wait().unwrap().is_none(),
                "serve exited: {}",
                fs::read_to_string(&log).unwrap()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let global = client
        .get(format!("{base}/api/daemon/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(global.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(global.headers()["cache-control"], "no-store");
    let global = client
        .get(format!("{base}/api/daemon/status"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(global.status(), reqwest::StatusCode::OK);
    let state: Value = global.json().await.unwrap();
    assert_eq!(state["activeCheckouts"], 0);
    let listing = client
        .get(format!("{base}/api/checkouts"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(listing.status(), reqwest::StatusCode::OK);
    let listing: Value = listing.json().await.unwrap();
    for key in [&a_key, &b_key] {
        assert!(
            listing["checkouts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["rootKey"] == *key),
            "{listing}"
        );
    }
    let inert = client
        .get(format!("{base}/api/daemon/status"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(inert.json::<Value>().await.unwrap()["activeCheckouts"], 0);
    let bad_host = client
        .get(format!("{base}/api/checkouts/{a_key}/status"))
        .header("Host", "not-local:1")
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(bad_host.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(!bad_host.headers().contains_key("X-Baleyg-Workspace"));
    let bad_origin = client
        .get(format!("{base}/api/checkouts/{a_key}/status"))
        .header("Origin", "http://not-local:1")
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(bad_origin.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(!bad_origin.headers().contains_key("X-Baleyg-Workspace"));
    for (key, root, expected) in [
        (&a_key, &a, "alpha_checkout"),
        (&b_key, &b, "beta_checkout"),
    ] {
        let verified_root = root.canonicalize().unwrap();
        let status = tokio::time::timeout(Duration::from_secs(25), async {
            loop {
                let response = client
                    .get(format!("{base}/api/checkouts/{key}/status"))
                    .bearer_auth(token)
                    .send()
                    .await
                    .unwrap();
                if response.status() == reqwest::StatusCode::OK {
                    break response;
                }
                assert_eq!(
                    response.status(),
                    reqwest::StatusCode::SERVICE_UNAVAILABLE,
                    "{}",
                    response.text().await.unwrap_or_default()
                );
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            status.headers()["X-Baleyg-Workspace"],
            verified_root.to_str().unwrap()
        );
        let status: Value = status.json().await.unwrap();
        assert_eq!(status["workspaceRoot"], verified_root.to_str().unwrap());
        assert!(status["catchingUp"].is_boolean());
        let source = client
            .get(format!("{base}/api/checkouts/{key}/source"))
            .query(&[("path", "core.js")])
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(source.status(), reqwest::StatusCode::OK);
        assert_eq!(
            source.headers()["X-Baleyg-Workspace"],
            verified_root.to_str().unwrap()
        );
        assert!(source.headers().contains_key("X-Baleyg-Catching-Up"));
        let source: Value = source.json().await.unwrap();
        assert!(
            source["file"]["text"].as_str().unwrap().contains(expected),
            "{source}"
        );
    }
    let missing = client
        .get(format!("{base}/api/status"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
    assert!(!missing.headers().contains_key("X-Baleyg-Workspace"));
    let unknown = client
        .get(format!("{base}/api/checkouts/{}/status", "a".repeat(64)))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert!(!unknown.status().is_success());
    assert!(!unknown.headers().contains_key("X-Baleyg-Workspace"));
    let unknown: Value = unknown.json().await.unwrap();
    assert_eq!(unknown["error"]["code"], "workspace_selection_failed");
    let resolved = client
        .get(format!("{base}/api/checkouts/{a_key}/source"))
        .query(&[("path", "absent.js")])
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(resolved.status(), reqwest::StatusCode::NOT_FOUND);
    assert_eq!(
        resolved.headers()["X-Baleyg-Workspace"],
        a.canonicalize().unwrap().to_str().unwrap()
    );
    assert!(resolved.headers().contains_key("X-Baleyg-Catching-Up"));
    let invalid = client
        .get(format!("{base}/api/checkouts/not-a-key/status"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(!invalid.headers().contains_key("X-Baleyg-Workspace"));
    let changed = temp.path().join("moved-a");
    fs::rename(&a, &changed).unwrap();
    let drift = client
        .get(format!("{base}/api/checkouts/{a_key}/status"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(drift.status(), reqwest::StatusCode::CONFLICT);
    assert!(!drift.headers().contains_key("X-Baleyg-Workspace"));
}

#[test]
fn selected_browser_clock_has_separate_disconnect_release_and_exit_delays() {
    use baleyg::{daemon::registry::CheckoutRegistry, store::topology::TopologyRoots};
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("checkout");
    fs::create_dir(&root).unwrap();
    let identity = WorkspaceIdentity::discover(Some(&root), &root).unwrap();
    let start = std::time::Instant::now();
    let roots =
        TopologyRoots::isolated_for_tests(temp.path().join("cache"), temp.path().join("data"));
    let mut registry = CheckoutRegistry::with_roots_at(roots, start);
    registry.browser_request_at(&identity, start).unwrap();
    assert_eq!(registry.active_count(), 1);
    assert_eq!(registry.idle_exit_deadline(), None);
    assert!(
        !registry
            .advance(start + Duration::from_secs(15 * 60 - 1))
            .unwrap()
            .exit
    );
    assert_eq!(registry.active_count(), 1);
    registry
        .advance(start + Duration::from_secs(15 * 60))
        .unwrap();
    assert_eq!(
        registry.idle_exit_deadline(),
        Some(start + Duration::from_secs(45 * 60))
    );
    assert!(
        registry
            .advance(start + Duration::from_secs(30 * 60 - 1))
            .unwrap()
            .released
            .is_empty()
    );
    let release = registry
        .advance(start + Duration::from_secs(30 * 60))
        .unwrap();
    assert_eq!(release.released, vec![identity.root_key.clone()]);
    assert!(!release.exit);
    assert!(
        !registry
            .advance(start + Duration::from_secs(45 * 60 - 1))
            .unwrap()
            .exit
    );
    assert!(
        registry
            .advance(start + Duration::from_secs(45 * 60))
            .unwrap()
            .exit
    );
}

#[test]
fn selected_browser_renewal_does_not_extend_other_checkout() {
    use baleyg::{daemon::registry::CheckoutRegistry, store::topology::TopologyRoots};
    let temp = TempDir::new().unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();
    let a = WorkspaceIdentity::discover(Some(&a), &a).unwrap();
    let b = WorkspaceIdentity::discover(Some(&b), &b).unwrap();
    let start = std::time::Instant::now();
    let roots =
        TopologyRoots::isolated_for_tests(temp.path().join("cache"), temp.path().join("data"));
    let mut registry = CheckoutRegistry::with_roots_at(roots, start);
    registry.browser_request_at(&a, start).unwrap();
    registry.browser_request_at(&b, start).unwrap();
    registry
        .browser_request_at(&a, start + Duration::from_secs(10 * 60))
        .unwrap();
    let tick = registry
        .advance(start + Duration::from_secs(30 * 60))
        .unwrap();
    assert_eq!(tick.released, vec![b.root_key]);
    assert_eq!(registry.active_count(), 1);
    assert_eq!(
        registry.idle_exit_deadline(),
        Some(start + Duration::from_secs(55 * 60))
    );
    let tick = registry
        .advance(start + Duration::from_secs(40 * 60))
        .unwrap();
    assert_eq!(tick.released, vec![a.root_key]);
    assert!(!tick.exit);
}

#[test]
fn global_list_marks_corrupt_existing_index_without_attaching() {
    use baleyg::{daemon::registry::CheckoutRegistry, store::topology::TopologyRoots};
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("checkout");
    fs::create_dir(&root).unwrap();
    let identity = WorkspaceIdentity::discover(Some(&root), &root).unwrap();
    let roots =
        TopologyRoots::isolated_for_tests(temp.path().join("cache"), temp.path().join("data"));
    let index = roots.index_db(&identity);
    fs::create_dir_all(index.parent().unwrap()).unwrap();
    fs::write(index, b"not a sqlite index").unwrap();
    let registry = CheckoutRegistry::with_roots(roots);
    let rows = registry.browser_checkouts();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["rootKey"], identity.root_key);
    assert_eq!(rows[0]["state"], "corrupt");
    assert_eq!(rows[0]["active"], false);
    assert_eq!(registry.active_count(), 0);
}
