use serde_json::Value;
use std::{fs, process::Command};
use tempfile::TempDir;

fn isolated_command(home: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_baleyg"));
    // ProjectDirs uses inherited XDG roots before HOME on Linux.
    cmd.env("HOME", home)
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_DATA_HOME");
    cmd
}

fn command(root: &std::path::Path, state: &std::path::Path, sub: &str) -> Command {
    let mut c = isolated_command(state);
    c.arg(sub).arg("--workspace").arg(root);
    if sub == "serve" {
        c.arg("--token-file").arg(state.join("token"));
    }
    c
}

#[test]
fn cli_helper_uses_isolated_home_instead_of_inherited_xdg_roots() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace).unwrap();

    let mut cmd = command(&workspace, &home, "status");
    let overrides = cmd.get_envs().collect::<Vec<_>>();
    for key in ["XDG_CACHE_HOME", "XDG_DATA_HOME"] {
        assert!(
            overrides
                .iter()
                .any(|(name, value)| *name == key && value.is_none()),
            "{key} must be removed from the child environment"
        );
    }
    let output = cmd.output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("index_not_ready"));
    let cache = home.join(if cfg!(target_os = "macos") {
        "Library/Caches/dev.odin.baleyg"
    } else {
        ".cache/baleyg"
    });
    assert!(cache.exists());
}

#[test]
fn cli_round_trip_uses_persistent_store_and_never_executes_workspace() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    let state = temp.path().join("state");
    fs::create_dir(&root).unwrap();
    fs::write(
        root.join("flow.js"),
        "export function start() { return send(); }\nfunction send() {}\n",
    )
    .unwrap();
    fs::write(
        root.join("package.json"),
        r#"{"scripts":{"postinstall":"touch SHOULD_NOT_EXIST"}}"#,
    )
    .unwrap();
    let before = fs::read(root.join("flow.js")).unwrap();
    let output = command(&root, &state, "index").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"]["stats"]["files"], 1);
    assert_eq!(result["status"]["stats"]["semanticState"], "unavailable");
    let output = command(&root, &state, "symbols")
        .arg("--search")
        .arg("start")
        .output()
        .unwrap();
    assert!(output.status.success());
    let symbols: Value = serde_json::from_slice(&output.stdout).unwrap();
    let seed = symbols["items"][0]["id"].as_str().unwrap();
    let output = command(&root, &state, "query")
        .arg("--seed")
        .arg(seed)
        .output()
        .unwrap();
    assert!(output.status.success());
    let view: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(view["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(view["calls"].as_array().unwrap().len(), 1);
    assert!(view["calls"][0].get("resolution").is_none());
    assert!(view["calls"][0].get("target").is_none());
    let path = temp.path().join("graph.json");
    assert!(
        command(&root, &state, "export")
            .arg("--output")
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let bytes = fs::read(&path).unwrap();
    let graph: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        graph["files"][0]["text"],
        std::str::from_utf8(&before).unwrap()
    );
    assert!(
        !command(&root, &state, "export")
            .arg("--output")
            .arg(&path)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::read(root.join("flow.js")).unwrap(), before);
    assert!(!root.join("SHOULD_NOT_EXIST").exists());
    assert!(!root.join(".baleyg").exists());
}
#[test]
fn cli_refuses_remote_bind_and_invalid_limits() {
    let temp = TempDir::new().unwrap();
    let state = temp.path().join("state");
    let output = command(temp.path(), &state, "serve")
        .arg("--bind")
        .arg("0.0.0.0:8877")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("loopback"));
    let output = command(temp.path(), &state, "index")
        .arg("--max-file-bytes")
        .arg("0")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let output = Command::new(env!("CARGO_BIN_EXE_baleyg"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn live_jev_requires_paired_explicit_budget_flags() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let state = temp.path().join("state");
    for flags in [
        vec!["--jev-budget-dir", "unused"],
        vec!["--jev-budget-cents", "500"],
        vec!["--jev-budget-dir", "unused", "--jev-budget-cents", "501"],
        vec!["--jev-budget-dir", "unused", "--jev-budget-cents", "0"],
    ] {
        let output = command(&source, &state, "serve")
            .args(flags)
            .env_remove("JEV_KEY")
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
    assert!(
        !state.exists(),
        "invalid opt-in must fail before state creation"
    );
}

#[test]
fn live_jev_missing_key_fails_without_creating_budget() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let state = temp.path().join("state");
    let budget = temp.path().join("budget");
    let output = command(&source, &state, "serve")
        .args([
            "--bind",
            "127.0.0.1:0",
            "--jev-budget-cents",
            "500",
            "--jev-budget-dir",
        ])
        .arg(&budget)
        .env_remove("JEV_KEY")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("JEV_KEY must be configured"));
    assert!(!budget.exists());
}

#[cfg(unix)]
#[test]
fn malformed_jev_environment_never_echoes_credential_bytes() {
    use std::os::unix::ffi::OsStringExt;
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let budget = temp.path().join("budget");
    let output = command(&source, &temp.path().join("state"), "serve")
        .args([
            "--bind",
            "127.0.0.1:0",
            "--jev-budget-cents",
            "500",
            "--jev-budget-dir",
        ])
        .arg(&budget)
        .env(
            "JEV_KEY",
            std::ffi::OsString::from_vec(b"SYNTHETIC_INVALID_CREDENTIAL_\xff".to_vec()),
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("JEV_KEY must be configured"));
    assert!(!stderr.contains("SYNTHETIC_INVALID_CREDENTIAL"));
    assert!(!budget.exists());
}

#[test]
fn acp_requires_complete_explicit_allowance_flags() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let state = temp.path().join("state");
    for flags in [
        vec!["--acp-runner", "unused"],
        vec!["--acp-state-dir", "unused"],
        vec!["--acp-max-attempts", "1"],
        vec![
            "--acp-runner",
            "unused",
            "--acp-state-dir",
            "unused",
            "--acp-max-attempts",
            "0",
        ],
        vec![
            "--acp-runner",
            "unused",
            "--acp-state-dir",
            "unused",
            "--acp-max-attempts",
            "21",
        ],
    ] {
        let output = command(&source, &state, "serve")
            .args(flags)
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
    assert!(
        !state.exists(),
        "invalid opt-in must fail before state creation"
    );
}

#[test]
fn fixed_locations_and_removed_flag() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.js"), "function seed() {}").unwrap();
    let unready = command(&root, &home, "status").output().unwrap();
    assert!(!unready.status.success());
    assert!(String::from_utf8_lossy(&unready.stderr).contains("index_not_ready"));
    let status = command(&root, &home, "index").output().unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let first: Value = serde_json::from_slice(&status.stdout).unwrap();
    let first = &first["status"];
    assert_eq!(first["revision"]["indexRevision"], 1);
    let generation = first["revision"]["indexGeneration"].as_str().unwrap();
    assert_eq!(
        uuid::Uuid::parse_str(generation).unwrap().get_version_num(),
        4
    );
    let cache = home.join(if cfg!(target_os = "macos") {
        "Library/Caches/dev.odin.baleyg"
    } else {
        ".cache/baleyg"
    });
    assert!(
        cache.exists(),
        "fixed cache missing under {}",
        home.display()
    );
    assert!(!home.join("state/cache.db").exists());
    assert!(!home.join("state/workspace.db").exists());
    let removed = command(&root, &home, "status")
        .arg("--state-dir")
        .arg(home.join("override"))
        .output()
        .unwrap();
    assert!(!removed.status.success());
    assert!(String::from_utf8_lossy(&removed.stderr).contains("--state-dir"));
    assert_eq!(
        serde_json::from_slice::<Value>(&command(&root, &home, "status").output().unwrap().stdout)
            .unwrap()["revision"],
        first["revision"]
    );
}
#[test]
fn index_forwards_pair_and_reports_pair() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.js"), "function seed() {}").unwrap();
    let unready = command(&root, &home, "status").output().unwrap();
    assert!(!unready.status.success());
    let result = command(&root, &home, "index").output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let published: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        published["status"]["revision"],
        published["publishedRevision"]
    );
    assert_eq!(
        published["status"]["evidenceFormat"],
        "terminal-native-graph-v1"
    );
    assert_eq!(published["publishedRevision"]["indexRevision"], 1);
}
#[test]
fn export_path_refusals() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.js"), "function seed() {}").unwrap();
    assert!(
        command(&root, &home, "index")
            .output()
            .unwrap()
            .status
            .success()
    );
    let forbidden = root.join("graph.json");
    let denied = command(&root, &home, "export")
        .arg("--output")
        .arg(&forbidden)
        .output()
        .unwrap();
    assert!(!denied.status.success());
    assert!(!forbidden.exists());
    let valid = temp.path().join("graph.json");
    assert!(
        command(&root, &home, "export")
            .arg("--output")
            .arg(&valid)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(valid.exists());
}
#[test]
fn current_commands_pair_matrix() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.js"), "function seed() {}").unwrap();
    let index = command(&root, &home, "index").output().unwrap();
    assert!(index.status.success());
    let pin: Value =
        serde_json::from_slice::<Value>(&index.stdout).unwrap()["publishedRevision"].clone();
    let status: Value =
        serde_json::from_slice(&command(&root, &home, "status").output().unwrap().stdout).unwrap();
    assert_eq!(status["revision"], pin);
    let symbols: Value =
        serde_json::from_slice(&command(&root, &home, "symbols").output().unwrap().stdout).unwrap();
    assert_eq!(symbols["revision"], pin);
    let seed = symbols["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "seed")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let query: Value = serde_json::from_slice(
        &command(&root, &home, "query")
            .arg("--seed")
            .arg(seed)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(query["revision"], pin);
}

#[test]
fn external_destinations_are_rejected_before_git_marker_creation() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join(".git")).unwrap();
    let token = root.join("token");
    let serve = isolated_command(&home)
        .arg("serve")
        .arg("--workspace")
        .arg(&root)
        .arg("--token-file")
        .arg(&token)
        .output()
        .unwrap();
    assert!(!serve.status.success());
    assert!(!root.join(".git/baleyg/workspace-id").exists());
    assert!(!token.exists());
    let output = root.join("graph.json");
    let export = command(&root, &home, "export")
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();
    assert!(!export.status.success());
    assert!(!output.exists());
    assert!(!root.join(".git/baleyg/workspace-id").exists());
}

#[test]
fn overlapping_git_workspace_refuses_before_marker_or_managed_entries() {
    for fixed in ["cache", "data"] {
        for sub in ["status", "index", "symbols", "query", "export", "serve"] {
            let temp = TempDir::new().unwrap();
            let home = temp.path().join("home");
            let (cache, data) = if cfg!(target_os = "macos") {
                (
                    home.join("Library/Caches/dev.odin.baleyg"),
                    home.join("Library/Application Support/dev.odin.baleyg"),
                )
            } else {
                (home.join(".cache/baleyg"), home.join(".local/share/baleyg"))
            };
            let workspace = if fixed == "cache" { &cache } else { &data };
            fs::create_dir_all(workspace.join(".git")).unwrap();
            let mut cmd = command(workspace, &home, sub);
            cmd.env("XDG_CACHE_HOME", home.join(".cache"))
                .env("XDG_DATA_HOME", home.join(".local/share"));
            if sub == "query" {
                cmd.arg("--seed").arg("a");
            }
            let output = cmd.output().unwrap();
            assert!(!output.status.success(), "{fixed} {sub}");
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("workspace root overlaps fixed topology"),
                "{fixed} {sub}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                !workspace.join(".git/baleyg/workspace-id").exists(),
                "{fixed} {sub}"
            );
            assert!(!cache.join("indexes").exists(), "{fixed} {sub}");
            assert!(!data.join("workspaces").exists(), "{fixed} {sub}");
            assert!(!home.join("token").exists(), "{fixed} {sub}");
        }
    }
}

#[test]
fn gc_report_without_workspace_does_not_create_state() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let output = isolated_command(&home)
        .args(["gc", "--report"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let inventory: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(inventory["derived"], serde_json::json!([]));
    assert_eq!(inventory["records"], serde_json::json!([]));
    assert!(!home.exists());
    let denied = isolated_command(&home).arg("gc").output().unwrap();
    assert!(!denied.status.success());
    assert!(!home.exists());
    let root = temp.path().join("work");
    fs::create_dir(&root).unwrap();
    assert!(
        command(&root, &home, "index")
            .output()
            .unwrap()
            .status
            .success()
    );
    let output = isolated_command(&home)
        .args(["gc", "--report"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let inventory: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(inventory["derived"].as_array().unwrap().len(), 1);
    assert_eq!(inventory["derived"][0]["status"], "unknown");
    assert_eq!(inventory["derived"][0]["reason"], "recent_open");
}

#[test]
fn gc_cli_reports_multiple_indexes_in_sorted_order() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let mut workspaces = (0..3)
        .map(|n| {
            let root = temp.path().join(format!("sorted-index-{n}"));
            fs::create_dir(&root).unwrap();
            let identity =
                baleyg::store::topology::WorkspaceIdentity::discover(Some(&root), &root).unwrap();
            (identity.root_key, root)
        })
        .collect::<Vec<_>>();
    workspaces.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, root) in &workspaces {
        let status = command(root, &home, "index").output().unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
    }
    let output = isolated_command(&home)
        .args(["gc", "--report"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let actual = report["derived"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["rootKey"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let mut expected = workspaces
        .into_iter()
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(actual, expected);
}

#[test]
fn forget_cli_requires_confirmation_and_preserves_unrelated_state() {
    use baleyg::{
        model::SavedView,
        store::topology::{DurableRecords, TopologyRoots, UseGuard, WorkspaceIdentity},
    };
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let root = temp.path().join("work");
    fs::create_dir(&root).unwrap();
    let data = home.join(if cfg!(target_os = "macos") {
        "Library/Application Support/dev.odin.baleyg"
    } else {
        ".local/share/baleyg"
    });
    let roots = TopologyRoots::isolated_for_tests(home.join("cache"), data);
    let identity = WorkspaceIdentity::discover(Some(&root), &root).unwrap();
    let view: SavedView = serde_json::from_str(
        r#"{"id":"view1","title":"A view","query":{"seed":"symbol"},"pins":{}}"#,
    )
    .unwrap();
    let records = DurableRecords::new(&roots, &identity);
    records.put_view(&view).unwrap();
    let id = identity.record_id.as_str();
    let run = |which: &str, yes: bool| {
        let mut cmd = isolated_command(&home);
        cmd.arg("forget").arg(which);
        if yes {
            cmd.arg("--yes");
        }
        cmd.output().unwrap()
    };
    assert!(!run("../work", true).status.success());
    let denied = run(id, false);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("terminal or --yes"));
    assert!(roots.record_db(&identity).exists());
    let shared =
        UseGuard::acquire_existing(&roots.record_use_lock(&identity), false, true).unwrap();
    assert!(!run(id, true).status.success());
    drop(shared);
    let other = home.join("untouched");
    fs::write(&other, "still here").unwrap();
    let result = run(id, true);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("every checkout sharing this UUID"));
    assert!(stderr.contains("Saved views: 1; annotations: 0"));
    assert!(stderr.contains(identity.root.to_str().unwrap()));
    assert!(!roots.record_dir(&identity).exists());
    assert!(!roots.record_use_lock(&identity).exists());
    assert_eq!(fs::read_to_string(other).unwrap(), "still here");
    assert!(!run(id, true).status.success());
}

#[test]
fn forget_yes_refuses_unknown_sqlite_schema_without_removing_state() {
    use baleyg::{
        model::SavedView,
        store::topology::{DurableRecords, TopologyRoots, WorkspaceIdentity},
    };
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir(&work).unwrap();
    let data = home.join(if cfg!(target_os = "macos") {
        "Library/Application Support/dev.odin.baleyg"
    } else {
        ".local/share/baleyg"
    });
    let roots = TopologyRoots::isolated_for_tests(home.join("cache"), data);
    let identity = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    let view: SavedView = serde_json::from_str(
        r#"{"id":"view1","title":"A view","query":{"seed":"symbol"},"pins":{}}"#,
    )
    .unwrap();
    DurableRecords::new(&roots, &identity)
        .put_view(&view)
        .unwrap();
    let path = roots.record_db(&identity);
    let lock = roots.record_use_lock(&identity);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE unknown(id INTEGER)")
        .unwrap();
    drop(db);
    let before = fs::read(&path).unwrap();
    let lock_before = fs::read(&lock).unwrap();
    let unrelated = home.join("unrelated");
    fs::write(&unrelated, "keep").unwrap();
    let result = isolated_command(&home)
        .args(["forget", &identity.record_id, "--yes"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("unexpected SQLite schema"));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::read(&lock).unwrap(), lock_before);
    assert_eq!(fs::read(&unrelated).unwrap(), b"keep");
}

#[test]
fn cli_known_old_snapshot_is_unreadable_until_explicit_index_rotates_generation() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.js"), "function go() { foo(); }").unwrap();
    let first = command(&root, &home, "index").output().unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let old_pin = first["status"]["revision"].clone();
    let symbols = command(&root, &home, "symbols")
        .arg("--search")
        .arg("go")
        .output()
        .unwrap();
    assert!(symbols.status.success());
    let symbols: Value = serde_json::from_slice(&symbols.stdout).unwrap();
    let valid_seed = symbols["items"][0]["id"].as_str().unwrap().to_owned();
    let cached = if cfg!(target_os = "macos") {
        home.join("Library/Caches/dev.odin.baleyg/indexes")
    } else {
        home.join(".cache/baleyg/indexes")
    };
    let path = fs::read_dir(cached)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
        .join("index.db");
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.pragma_update(None, "foreign_keys", false).unwrap();
        let native_tables = db
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'native_%'")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for table in native_tables {
            db.execute_batch(&format!("DROP TABLE {table}")).unwrap();
        }
        for index in ["nodes_path", "calls_path", "regions_path"] {
            db.execute_batch(&format!("DROP INDEX {index}")).unwrap();
        }
        db.execute(
            "UPDATE index_metadata SET schema_version=4,extractor_version='native-v1'",
            [],
        )
        .unwrap();
        db.pragma_update(None, "user_version", 4).unwrap();
        db.execute("UPDATE calls SET payload=json_set(payload,'$.target','lexical-guess','$.resolution','internal')",[]).unwrap();
    }
    for sub in ["status", "symbols", "query", "export"] {
        let mut cmd = command(&root, &home, sub);
        if sub == "query" {
            cmd.arg("--seed").arg(&valid_seed);
        }
        let blocked_export = temp.path().join("old-export-must-not-exist.json");
        if sub == "export" {
            cmd.arg("--output").arg(&blocked_export);
        }
        let result = cmd.output().unwrap();
        assert!(!blocked_export.exists(), "old export wrote a destination");
        assert!(
            !result.status.success(),
            "{sub} unexpectedly read old index"
        );
        let visible = format!(
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(visible.contains("index_not_ready"), "{sub}: {visible}");
        assert!(!visible.contains(old_pin["indexGeneration"].as_str().unwrap()));
        assert!(!visible.contains("lexical-guess"));
    }
    // Real CLI exploit regression: an exact legacy4 DB with an extra trigger
    // cannot rebaseline into a forged schema5 publication or write anything.
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TRIGGER forged_call AFTER INSERT ON calls BEGIN
        UPDATE calls SET payload=json_set(payload,'$.calleeText','FORGED-NOT-MEASURED')
        WHERE id=NEW.id; END;",
    )
    .unwrap();
    drop(db);
    let dangerous_bytes = fs::read(&path).unwrap();
    let rejected = command(&root, &home, "index").output().unwrap();
    assert!(!rejected.status.success());
    let message = String::from_utf8_lossy(&rejected.stderr);
    assert!(message.contains("incompatible_index"), "{message}");
    assert_eq!(fs::read(&path).unwrap(), dangerous_bytes);
    let blocked_export = command(&root, &home, "export").output().unwrap();
    assert!(!blocked_export.status.success());
    assert!(!String::from_utf8_lossy(&blocked_export.stdout).contains("FORGED-NOT-MEASURED"));
    let db = rusqlite::Connection::open(&path).unwrap();
    let forged: i64 = db
        .query_row(
            "SELECT count(*) FROM calls WHERE payload LIKE '%FORGED-NOT-MEASURED%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(forged, 0, "unknown trigger executed despite refusal");
    db.execute_batch("DROP TRIGGER forged_call").unwrap();
    drop(db);
    let next = command(&root, &home, "index").output().unwrap();
    assert!(
        next.status.success(),
        "{}",
        String::from_utf8_lossy(&next.stderr)
    );
    let next: Value = serde_json::from_slice(&next.stdout).unwrap();
    assert_ne!(
        next["status"]["revision"]["indexGeneration"],
        old_pin["indexGeneration"]
    );
    assert_eq!(next["status"]["evidenceFormat"], "terminal-native-graph-v1");
    let status = command(&root, &home, "status").output().unwrap();
    assert!(status.status.success());
    let ready: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(ready["revision"], next["status"]["revision"]);
    let queried = command(&root, &home, "query")
        .arg("--seed")
        .arg(&valid_seed)
        .output()
        .unwrap();
    assert!(
        queried.status.success(),
        "{}",
        String::from_utf8_lossy(&queried.stderr)
    );
    let view: Value = serde_json::from_slice(&queried.stdout).unwrap();
    assert_eq!(view["revision"], ready["revision"]);
    assert!(
        view["calls"]
            .as_array()
            .unwrap()
            .iter()
            .all(|call| call.get("target").is_none())
    );
    assert!(!view.to_string().contains("lexical-guess"));
    let exported = command(&root, &home, "export").output().unwrap();
    assert!(exported.status.success());
    let graph: Value = serde_json::from_slice(&exported.stdout).unwrap();
    assert_eq!(graph["files"][0]["text"], "function go() { foo(); }");
    assert!(!graph.to_string().contains("lexical-guess"));
}

// Read the normalized publication, including every native row, from the real daemon's database.
fn real_index_db(home: &std::path::Path) -> std::path::PathBuf {
    fn find(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == "index.db") {
                return Some(path);
            }
            if path.is_dir()
                && let Some(db) = find(&path)
            {
                return Some(db);
            }
        }
        None
    }
    find(home).expect("published native database")
}
fn real_native_snapshot(home: &std::path::Path) -> Value {
    use rusqlite::types::ValueRef;
    let db = rusqlite::Connection::open(real_index_db(home)).unwrap();
    let source_set: (String, String) = db
        .query_row("SELECT id,root_id FROM native_source_sets", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    let revision: (String, String, String, String, String) = db.query_row(
        "SELECT id,source_set_id,toolchain_hash,config_hash,dependency_hash FROM native_revisions", [],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))
    ).unwrap();
    let mut documents = Vec::new();
    let mut stmt = db.prepare(
        "SELECT source_set_id,language,path,revision_id,content_hash,byte_length,source_bytes FROM native_documents ORDER BY path"
    ).unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, Vec<u8>>(6)?,
            ))
        })
        .unwrap();
    for row in rows {
        let (set, language, path, revision, hash, length, bytes) = row.unwrap();
        documents.push(serde_json::json!({"sourceSetId":set,"language":language,"path":path,
            "revisionId":revision,"contentHash":hash,"byteLength":length,"bytesHex":hex::encode(bytes)}));
    }
    let mut all_rows = serde_json::Map::new();
    let mut names = db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'native_%' ORDER BY name").unwrap();
    for name in names.query_map([], |r| r.get::<_, String>(0)).unwrap() {
        let name = name.unwrap();
        let mut table = db
            .prepare(&format!("SELECT * FROM {name} ORDER BY rowid"))
            .unwrap();
        let columns = table.column_count();
        let records = table
            .query_map([], |row| {
                let mut cells = Vec::new();
                for i in 0..columns {
                    let cell = match row.get_ref(i)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(n) => serde_json::json!(n),
                        ValueRef::Real(n) => serde_json::json!(n),
                        ValueRef::Text(bytes) => serde_json::json!(String::from_utf8_lossy(bytes)),
                        ValueRef::Blob(bytes) => serde_json::json!({"blobHex":hex::encode(bytes)}),
                    };
                    cells.push(cell);
                }
                Ok(cells)
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        all_rows.insert(name, serde_json::json!(records));
    }
    serde_json::json!({"sourceSet":{"id":source_set.0,"rootId":source_set.1},
        "revision":{"id":revision.0,"sourceSetId":revision.1,"toolchainHash":revision.2,
            "configHash":revision.3,"dependencyHash":revision.4},
        "documents":documents,"allRows":all_rows})
}

fn real_export(root: &std::path::Path, home: &std::path::Path) -> Value {
    let output = command(root, home, "export").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
async fn real_api(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    method: reqwest::Method,
    route: &str,
    body: Option<Value>,
) -> (reqwest::StatusCode, Value) {
    let mut request = client
        .request(method, format!("{url}{route}"))
        .header("Origin", url)
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    (status, response.json().await.unwrap())
}

// Exercise the actual executable on both sides of the coordinator, not an in-process router.
#[tokio::test]
async fn real_cli_and_authenticated_daemon_share_native_pair_for_every_language_and_empty_root() {
    use std::{io::Read, os::unix::fs::PermissionsExt, time::Duration};
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for (name, file, source) in [
        (
            "java",
            "Flow.java",
            "class Flow { void seed() { sink(); } void sink() {} }
",
        ),
        (
            "javascript",
            "flow.js",
            "function seed() { sink(); } function sink() {}
",
        ),
        (
            "python",
            "flow.py",
            "def seed():
    sink()
def sink():
    pass
",
        ),
        (
            "rust",
            "flow.rs",
            "fn seed() { sink(); } fn sink() {}
",
        ),
        ("empty", "", ""),
    ] {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("workspace");
        let home = tmp.path().join("home");
        fs::create_dir(&root).unwrap();
        if !file.is_empty() {
            fs::write(root.join(file), source).unwrap();
        }
        let indexed = command(&root, &home, "index").output().unwrap();
        assert!(
            indexed.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&indexed.stderr)
        );
        let first: Value = serde_json::from_slice(&indexed.stdout).unwrap();
        let old_pin = first["publishedRevision"].clone();
        assert_eq!(first["status"]["revision"], old_pin, "{name}");
        let native_before = real_native_snapshot(&home);
        let graph_before = real_export(&root, &home);
        assert_eq!(
            native_before["documents"].as_array().unwrap().len(),
            usize::from(!file.is_empty()),
            "{name}"
        );

        let token_file = tmp.path().join("token");
        fs::write(&token_file, TOKEN).unwrap();
        fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let stderr_path = tmp.path().join("daemon-stderr");
        let mut server = Server(
            isolated_command(&home)
                .arg("serve")
                .arg("--workspace")
                .arg(&root)
                .arg("--bind")
                .arg(format!("127.0.0.1:{port}"))
                .arg("--token-file")
                .arg(&token_file)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::from(
                    fs::File::create(&stderr_path).unwrap(),
                ))
                .spawn()
                .unwrap(),
        );
        let url = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::new();
        let mut ready = false;
        for _ in 0..100 {
            if client
                .get(format!("{url}/healthz"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if !ready {
            let status = server.0.try_wait().unwrap();
            let mut diagnostic = String::new();
            fs::File::open(&stderr_path)
                .unwrap()
                .take(2048)
                .read_to_string(&mut diagnostic)
                .unwrap();
            let diagnostic = diagnostic.replace(TOKEN, "[redacted]");
            panic!("{name}: daemon did not start: status={status:?}, stderr={diagnostic}");
        }
        let request = || {
            client
                .post(format!("{url}/api/index"))
                .header("Origin", &url)
                .json(&serde_json::json!({"expectedRevision":old_pin}))
        };
        let denied = request().bearer_auth("incorrect").send().await.unwrap();
        assert_eq!(denied.status(), 401, "{name}");
        let invalid_origin = client
            .post(format!("{url}/api/index"))
            .header("Origin", "https://evil.example")
            .bearer_auth(TOKEN)
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(invalid_origin.status(), 403, "{name}");
        let current: Value = client
            .get(format!("{url}/api/jobs/current"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(
            current.is_null(),
            "{name}: rejected auth/origin must not start work"
        );
        let accepted = request().bearer_auth(TOKEN).send().await.unwrap();
        assert_eq!(accepted.status(), 202, "{name}");
        let started: Value = accepted.json().await.unwrap();
        let id = started["id"].as_str().unwrap();
        let completed = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let job: Value = client
                    .get(format!("{url}/api/jobs/{id}"))
                    .bearer_auth(TOKEN)
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if !job["finishedAt"].is_null() {
                    break job;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(completed["state"], "completed", "{name}: {completed}");
        let pin = completed["revision"].clone();
        assert_eq!(pin["indexGeneration"], old_pin["indexGeneration"], "{name}");
        assert_eq!(pin["indexRevision"], 2, "{name}");
        let status: Value = client
            .get(format!("{url}/api/status"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(status["revision"], pin, "{name}");
        assert_eq!(
            status["stats"]["files"],
            usize::from(!file.is_empty()),
            "{name}"
        );
        use sha2::{Digest, Sha256};
        let native_after = real_native_snapshot(&home);
        let graph_after = real_export(&root, &home);
        assert_eq!(
            native_after, native_before,
            "{name}: complete normalized native rows must be stable"
        );
        assert_eq!(
            graph_after, graph_before,
            "{name}: identical bytes must reproduce graph"
        );
        let root_id =
            baleyg::store::topology::WorkspaceIdentity::discover_unattached(Some(&root), &root)
                .unwrap()
                .record_id;
        assert_eq!(native_after["sourceSet"]["rootId"], root_id, "{name}");
        assert_eq!(
            native_after["revision"]["sourceSetId"], native_after["sourceSet"]["id"],
            "{name}"
        );
        for field in ["toolchainHash", "configHash", "dependencyHash"] {
            assert_eq!(
                native_after["revision"][field].as_str().unwrap().len(),
                64,
                "{name}: {field}"
            );
        }
        let documents = native_after["documents"].as_array().unwrap();
        let graph_files = graph_after["files"].as_array().unwrap();
        assert_eq!(
            documents.len(),
            graph_files.len(),
            "{name}: every graph source needs native bytes"
        );
        for doc in documents {
            let path = doc["path"].as_str().unwrap();
            let graph_file = graph_files.iter().find(|row| row["path"] == path).unwrap();
            let bytes = graph_file["text"].as_str().unwrap().as_bytes();
            let digest = hex::encode(Sha256::digest(bytes));
            assert_eq!(
                doc["sourceSetId"], native_after["sourceSet"]["id"],
                "{name}"
            );
            assert_eq!(doc["revisionId"], native_after["revision"]["id"], "{name}");
            assert_eq!(doc["language"], graph_file["language"], "{name}");
            assert_eq!(doc["contentHash"], digest, "{name}");
            assert_eq!(doc["contentHash"], graph_file["hash"], "{name}");
            assert_eq!(doc["byteLength"], bytes.len(), "{name}");
            assert_eq!(doc["bytesHex"], hex::encode(bytes), "{name}");
            let route = format!(
                "/api/source?path={path}&indexGeneration={}&indexRevision={}",
                pin["indexGeneration"].as_str().unwrap(),
                pin["indexRevision"].as_u64().unwrap()
            );
            let (code, source_at) =
                real_api(&client, &url, TOKEN, reqwest::Method::GET, &route, None).await;
            assert_eq!(code, 200, "{name}: {source_at}");
            assert_eq!(source_at["revision"], pin, "{name}");
            assert_eq!(
                source_at["file"], *graph_file,
                "{name}: pinned source equals graph and native bytes"
            );
        }
        if !file.is_empty() {
            assert_eq!(documents[0]["path"], file, "{name}");
            assert_eq!(graph_files[0]["text"], source, "{name}");
            let pinned = format!(
                "indexGeneration={}&indexRevision={}",
                pin["indexGeneration"].as_str().unwrap(),
                pin["indexRevision"].as_u64().unwrap()
            );
            let (code, classes) = real_api(
                &client,
                &url,
                TOKEN,
                reqwest::Method::GET,
                &format!("/api/classes?{pinned}"),
                None,
            )
            .await;
            assert_eq!(code, 200, "{name}: {classes}");
            assert_eq!(classes["revision"], pin, "{name}");
            let (code, symbols) = real_api(
                &client,
                &url,
                TOKEN,
                reqwest::Method::GET,
                "/api/symbols?q=seed",
                None,
            )
            .await;
            assert_eq!(code, 200, "{name}: {symbols}");
            assert_eq!(symbols["revision"], pin, "{name}");
            let seed = symbols["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["name"] == "seed")
                .expect("measured seed declaration")["id"]
                .as_str()
                .unwrap();
            for (route, body) in [
                (
                    "/api/navigation",
                    serde_json::json!({"expectedRevision":pin,"path":file,"line":1}),
                ),
                (
                    "/api/sequence",
                    serde_json::json!({"expectedRevision":pin,"seed":seed}),
                ),
            ] {
                let (code, result) = real_api(
                    &client,
                    &url,
                    TOKEN,
                    reqwest::Method::POST,
                    route,
                    Some(body),
                )
                .await;
                assert_eq!(code, 200, "{name}: {route}: {result}");
                assert_eq!(result["revision"], pin, "{name}: {route}");
                assert!(
                    !result.to_string().contains("lexical-guess"),
                    "{name}: {route}"
                );
                assert!(
                    !result.to_string().contains(r#""resolution":"internal""#),
                    "{name}: {route}"
                );
            }
            let (code, preview) = real_api(
                &client,
                &url,
                TOKEN,
                reqwest::Method::POST,
                "/api/questions/preview",
                Some(serde_json::json!({
                    "seed":seed,"question":"what happens?","expectedRevision":pin
                })),
            )
            .await;
            assert_eq!(code, 200, "{name}: {preview}");
            assert_eq!(preview["packet"]["revision"], pin, "{name}");
            assert!(!preview.to_string().contains("lexical-guess"), "{name}");
            let packet = preview["packet"]["packetId"].as_str().unwrap();
            let (code, export) = real_api(
                &client,
                &url,
                TOKEN,
                reqwest::Method::GET,
                &format!("/api/questions/{packet}/jev-request"),
                None,
            )
            .await;
            assert_eq!(code, 200, "{name}: {export}");
            assert!(!export.to_string().contains("lexical-guess"), "{name}");
        }
        let stale = request().bearer_auth(TOKEN).send().await.unwrap();
        assert_eq!(
            stale.status(),
            409,
            "{name}: stale entire pair refused before work"
        );
        assert_eq!(real_native_snapshot(&home), native_before, "{name}");
        drop(server);
    }
}

#[tokio::test]
async fn real_daemon_post_capture_failure_preserves_pair_source_graph_and_cached_packet() {
    use std::{os::unix::fs::PermissionsExt, time::Duration};
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    struct Writer(rusqlite::Connection);
    impl Drop for Writer {
        fn drop(&mut self) {
            let _ = self.0.execute_batch("ROLLBACK");
        }
    }
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("workspace");
    let home = tmp.path().join("home");
    fs::create_dir(&root).unwrap();
    let original = "function seed() { sink(); } function sink() {}
";
    fs::write(root.join("flow.js"), original).unwrap();
    let initial = command(&root, &home, "index").output().unwrap();
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    let token_file = tmp.path().join("token");
    fs::write(&token_file, TOKEN).unwrap();
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let server = Server(
        isolated_command(&home)
            .arg("serve")
            .arg("--workspace")
            .arg(&root)
            .arg("--bind")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--token-file")
            .arg(&token_file)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(
                fs::File::create(tmp.path().join("daemon-stderr")).unwrap(),
            ))
            .spawn()
            .unwrap(),
    );
    let url = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if client
                .get(format!("{url}/healthz"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("real daemon readiness");
    let (code, status) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        "/api/status",
        None,
    )
    .await;
    assert_eq!(code, 200, "{status}");
    let pin = status["revision"].clone();
    let native_before = real_native_snapshot(&home);
    let graph_before = real_export(&root, &home);
    let pinned_source = format!(
        "/api/source?path=flow.js&indexGeneration={}&indexRevision={}",
        pin["indexGeneration"].as_str().unwrap(),
        pin["indexRevision"].as_u64().unwrap()
    );
    let (code, source_before) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        &pinned_source,
        None,
    )
    .await;
    assert_eq!(code, 200, "{source_before}");
    let (code, symbols) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        "/api/symbols?q=seed",
        None,
    )
    .await;
    assert_eq!(code, 200, "{symbols}");
    let seed = symbols["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "seed")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let (code, preview) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::POST,
        "/api/questions/preview",
        Some(serde_json::json!({"seed":seed,"question":"what happens?","expectedRevision":pin})),
    )
    .await;
    assert_eq!(code, 200, "{preview}");
    let packet_id = preview["packet"]["packetId"].as_str().unwrap();
    let packet_route = format!("/api/questions/{packet_id}/jev-request");
    let (code, packet_before) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        &packet_route,
        None,
    )
    .await;
    assert_eq!(code, 200, "{packet_before}");

    // A bounded multi-file scan lets the client acquire a real SQLite writer lock
    // after the HTTP job is accepted but before projection completes.
    let padding = "x".repeat(48 * 1024);
    for i in 0..80 {
        fs::write(
            root.join(format!("extra{i:03}.js")),
            format!("// {padding}\nfunction extra{i}() {{}}\n"),
        )
        .unwrap();
    }
    let (code, accepted) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::POST,
        "/api/index",
        Some(serde_json::json!({"expectedRevision":pin})),
    )
    .await;
    assert_eq!(code, 202, "{accepted}");
    let id = accepted["id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (code, job) = real_api(
                &client,
                &url,
                TOKEN,
                reqwest::Method::GET,
                &format!("/api/jobs/{id}"),
                None,
            )
            .await;
            assert_eq!(code, 200, "{job}");
            assert!(
                job["finishedAt"].is_null(),
                "job finished before lock: {job}"
            );
            if job["progress"]["phase"] == "scan"
                && job["progress"]["completed"].as_u64().unwrap() < 81
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("scan progress before SQLite lock");
    let writer = Writer(rusqlite::Connection::open(real_index_db(&home)).unwrap());
    writer.0.busy_timeout(Duration::from_secs(1)).unwrap();
    writer.0.execute_batch("BEGIN IMMEDIATE").unwrap();
    let after_capture = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let (code, job) = real_api(
                &client,
                &url,
                TOKEN,
                reqwest::Method::GET,
                &format!("/api/jobs/{id}"),
                None,
            )
            .await;
            assert_eq!(code, 200, "{job}");
            if job["progress"]["phase"] == "complete" {
                break job;
            }
            assert!(
                job["finishedAt"].is_null(),
                "failed before captured graph: {job}"
            );
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("captured graph must complete before cancellation");
    assert_eq!(after_capture["progress"]["completed"], 81);
    let (code, _) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::POST,
        &format!("/api/jobs/{id}/cancel"),
        None,
    )
    .await;
    assert_eq!(code, 200);
    drop(writer); // Always rolls back the external writer lock, including on panic.
    let terminal = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (code, job) = real_api(
                &client,
                &url,
                TOKEN,
                reqwest::Method::GET,
                &format!("/api/jobs/{id}"),
                None,
            )
            .await;
            assert_eq!(code, 200, "{job}");
            if !job["finishedAt"].is_null() {
                break job;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("failed or cancelled job terminal");
    assert!(
        matches!(terminal["state"].as_str(), Some("failed" | "cancelled")),
        "{terminal}"
    );
    assert_eq!(terminal["progress"]["phase"], "complete");
    let (code, current) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        "/api/status",
        None,
    )
    .await;
    assert_eq!(code, 200, "{current}");
    assert_eq!(
        current["revision"], pin,
        "failed job may not advance full pair"
    );
    assert_eq!(
        real_native_snapshot(&home),
        native_before,
        "all native rows remain unchanged"
    );
    assert_eq!(
        real_export(&root, &home),
        graph_before,
        "all graph rows remain unchanged"
    );
    let (code, source_after) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        &pinned_source,
        None,
    )
    .await;
    assert_eq!(code, 200, "{source_after}");
    assert_eq!(
        source_after, source_before,
        "previous pinned source remains usable"
    );
    let (code, packet_after) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        &packet_route,
        None,
    )
    .await;
    assert_eq!(code, 200, "{packet_after}");
    assert_eq!(
        packet_after, packet_before,
        "cached packet must remain usable after failed publication"
    );
    let (code, retry) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::POST,
        "/api/index",
        Some(serde_json::json!({"expectedRevision":pin})),
    )
    .await;
    assert_eq!(code, 202, "{retry}");
    let retry_id = retry["id"].as_str().unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let (code, job) = real_api(
                &client,
                &url,
                TOKEN,
                reqwest::Method::GET,
                &format!("/api/jobs/{retry_id}"),
                None,
            )
            .await;
            assert_eq!(code, 200, "{job}");
            if !job["finishedAt"].is_null() {
                break job;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("later successful job");
    assert_eq!(completed["state"], "completed", "{completed}");
    assert_eq!(
        completed["revision"]["indexGeneration"],
        pin["indexGeneration"]
    );
    assert_eq!(
        completed["revision"]["indexRevision"],
        pin["indexRevision"].as_u64().unwrap() + 1
    );
    drop(server);
}
