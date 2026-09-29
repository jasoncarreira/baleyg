use baleyg::store::Store;
use std::{fs, os::unix::fs::PermissionsExt, path::Path, sync::mpsc};
use tempfile::TempDir;

fn fixture() -> (TempDir, TempDir) {
    let state = tempfile::tempdir().unwrap();
    fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
    (state, tempfile::tempdir().unwrap())
}
fn projection_fixture() -> (TempDir, TempDir, Store, baleyg::model::IndexPin) {
    use baleyg::{index_coordinator::IndexJobCoordinator, indexer::IndexOptions};
    use std::sync::{Arc, atomic::AtomicBool};
    let (state, workspace) = fixture();
    fs::write(
        workspace.path().join("flow.js"),
        "function go() { measured(); }\n",
    )
    .unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let pin = IndexJobCoordinator::prepare(&store, None)
        .unwrap()
        .run(
            &IndexOptions::new(workspace.path().to_owned()),
            &Arc::new(AtomicBool::new(false)),
            |_| {},
        )
        .unwrap();
    (state, workspace, store, pin)
}
fn index_dir(state: &Path) -> std::path::PathBuf {
    fs::read_dir(state.join("cache/indexes"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
}
fn staged_files(dir: &Path) -> Vec<std::path::PathBuf> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("index.db.tmp-")
        })
        .collect()
}

#[test]
fn first_index_is_invisible_until_validated_and_synced() {
    let (state, workspace) = fixture();
    let (ready, arrived) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let state_path = state.path().to_owned();
    let workspace_path = workspace.path().to_owned();
    let opener = std::thread::spawn(move || {
        Store::open_for_tests_with_index_stage_hook(&state_path, &workspace_path, |staged| {
            ready.send(staged.to_owned()).unwrap();
            proceed.recv().unwrap();
            Ok(())
        })
    });
    let staged = arrived.recv().unwrap();
    let dir = index_dir(state.path());
    assert_eq!(staged.parent(), Some(dir.as_path()));
    assert!(!dir.join("index.db").exists());
    assert_eq!(staged_files(&dir), vec![staged.clone()]);
    assert!(fs::metadata(&staged).unwrap().len() > 0);
    let rival = Store::open_for_tests(state.path(), workspace.path());
    assert!(
        rival.unwrap_err().to_string().starts_with("storage_busy:"),
        "a concurrent first opener must not read an incomplete index"
    );
    release.send(()).unwrap();
    let store = opener.join().unwrap().unwrap();
    assert_eq!(store.index_baseline().unwrap().index_revision, 0);
    assert_eq!(staged_files(&dir), Vec::<std::path::PathBuf>::new());
    drop(store);
    let reopened = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    assert_eq!(reopened.index_baseline().unwrap().index_revision, 0);
}

#[test]
fn failed_stage_does_not_publish_or_leave_its_temp_file() {
    let (state, workspace) = fixture();
    let error = Store::open_for_tests_with_index_stage_hook(state.path(), workspace.path(), |_| {
        anyhow::bail!("injected stage failure")
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "injected stage failure");
    let dir = index_dir(state.path());
    assert!(!dir.join("index.db").exists());
    assert!(staged_files(&dir).is_empty());
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    assert_eq!(store.index_baseline().unwrap().index_revision, 0);
    assert!(staged_files(&dir).is_empty());
}

#[test]
fn truncated_stage_is_rejected_before_publication() {
    let (state, workspace) = fixture();
    let error =
        Store::open_for_tests_with_index_stage_hook(state.path(), workspace.path(), |staged| {
            fs::OpenOptions::new()
                .write(true)
                .open(staged)?
                .set_len(0)?;
            Ok(())
        })
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("exceptional index format recovery deferred"),
        "{error:#}"
    );
    let dir = index_dir(state.path());
    assert!(!dir.join("index.db").exists());
    assert!(staged_files(&dir).is_empty());
    assert_eq!(
        Store::open_for_tests(state.path(), workspace.path())
            .unwrap()
            .index_baseline()
            .unwrap()
            .index_revision,
        0
    );
}

#[test]
fn crash_left_partial_stage_is_not_published_or_mistaken_for_the_index() {
    let (state, workspace) = fixture();
    let _ = Store::open_for_tests_with_index_stage_hook(state.path(), workspace.path(), |staged| {
        // Model a previous process stopping before publish. Its private staging file
        // cannot be confused with the final index pathname on a later open.
        fs::write(
            staged.with_file_name("index.db.tmp-crashed"),
            b"SQLite form",
        )
        .unwrap();
        anyhow::bail!("injected crash")
    });
    let dir = index_dir(state.path());
    assert!(!dir.join("index.db").exists());
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    assert_eq!(store.index_baseline().unwrap().index_revision, 0);
    assert_eq!(
        fs::read(dir.join("index.db.tmp-crashed")).unwrap(),
        b"SQLite form"
    );
    assert_eq!(staged_files(&dir).len(), 1);
}

#[test]
fn preexisting_corrupt_index_is_not_reinitialized() {
    use std::os::unix::fs::MetadataExt;

    let (state, workspace) = fixture();
    drop(Store::open_for_tests(state.path(), workspace.path()).unwrap());
    let dir = index_dir(state.path());
    let index = dir.join("index.db");
    fs::OpenOptions::new()
        .write(true)
        .open(&index)
        .unwrap()
        .set_len(0)
        .unwrap();
    let before = fs::metadata(&index).unwrap();
    assert_eq!(before.len(), 0);
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    for refusal in [
        store.status().unwrap_err(),
        store.index_baseline().unwrap_err(),
        store
            .leader()
            .expect_err("exceptional format leader refusal"),
    ] {
        assert!(
            refusal
                .to_string()
                .contains("exceptional index recovery deferred"),
            "{refusal:#}"
        );
    }
    let after = fs::metadata(&index).unwrap();
    assert_eq!(after.ino(), before.ino());
    assert_eq!(after.len(), 0);
    assert!(staged_files(&dir).is_empty());
}

#[test]
fn failed_capture_leaves_existing_index_revision_unchanged() {
    use baleyg::{
        indexer::{IndexOptions, index_workspace},
        model::CancelFlag,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let (state, workspace) = fixture();
    fs::write(workspace.path().join("main.js"), "f();").unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let before = store.index_baseline().unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let error = index_workspace(
        &IndexOptions::new(workspace.path().to_owned()),
        &cancel,
        |progress| {
            if progress.phase == "parse" {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("cancelled"));
    assert_eq!(store.index_baseline().unwrap(), before);
    assert_eq!(
        Store::open_for_tests(state.path(), workspace.path())
            .unwrap()
            .index_baseline()
            .unwrap(),
        before
    );
}

#[test]
fn drift_refusal_preserves_populated_store_pair_and_graph() {
    use baleyg::{
        indexer::{IndexOptions, index_workspace},
        model::CancelFlag,
    };
    use std::sync::{Arc, atomic::AtomicBool};
    let (state, workspace) = fixture();
    let root = workspace.path();
    fs::write(root.join("main.js"), "function f() {} f();").unwrap();
    let options = IndexOptions::new(root.to_owned());
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let store = Store::open_for_tests(state.path(), root).unwrap();
    let graph = index_workspace(&options, &cancel, |_| {}).unwrap();
    let before = publish_bundle(
        &store,
        &graph,
        root,
        &store.leader().unwrap(),
        store.index_baseline().unwrap(),
        &cancel,
    )
    .unwrap();
    assert_eq!(before.index_revision, 1);
    let error = index_workspace(&options, &cancel, |progress| {
        if progress.phase == "parse" {
            fs::write(root.join("main.js"), "function f() {} g();").unwrap();
        }
    })
    .unwrap_err();
    assert!(error.to_string().contains("drift"), "{error:#}");
    let reopened = Store::open_for_tests(state.path(), root).unwrap();
    assert_eq!(reopened.status().unwrap().revision, before);
    assert_eq!(store.index_baseline().unwrap(), before);
}

fn publish_bundle(
    store: &baleyg::store::Store,
    graph: &baleyg::model::Graph,
    workspace: &std::path::Path,
    leader: &baleyg::store::topology::LeaderGuard,
    expected: baleyg::model::IndexPin,
    cancel: &baleyg::model::CancelFlag,
) -> anyhow::Result<baleyg::model::IndexPin> {
    let (indexed, native, capture) = baleyg::indexer::index_workspace_bundle(
        &baleyg::indexer::IndexOptions::new(workspace.to_owned()),
        store.root_id(),
        cancel,
        |_| {},
    )?;
    assert_eq!(
        &indexed, graph,
        "published graph must match captured source"
    );
    store.publish_native(&indexed, &capture, &native, leader, expected, cancel)
}

#[test]
fn coordinator_rejects_drift_cancel_and_stale_pair_without_partial_publication() {
    use baleyg::{
        index_coordinator::IndexJobCoordinator, indexer::IndexOptions, model::CancelFlag,
    };
    use sha2::{Digest, Sha256};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let (state, workspace) = fixture();
    let root = workspace.path();
    fs::write(
        root.join("main.js"),
        "function seed() { sink(); } function sink() {}
",
    )
    .unwrap();
    let store = Store::open_for_tests(state.path(), root).unwrap();
    let options = IndexOptions::new(root.to_owned());
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let first = IndexJobCoordinator::prepare(&store, None)
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();
    let (old_pin, old_file) = store.source_at("main.js", Some(first)).unwrap().unwrap();
    assert_eq!(old_pin, first);
    let original = old_file.text;
    let original_hash = hex::encode(Sha256::digest(original.as_bytes()));
    let assert_public_closed = || {
        let status_error = store.status().unwrap_err();
        assert!(
            status_error.to_string().contains("index_not_ready"),
            "{status_error:#}"
        );
        let source_error = store.source_at("main.js", Some(first)).unwrap_err();
        assert!(
            source_error.to_string().contains("index_not_ready"),
            "{source_error:#}"
        );
    };
    let index = index_dir(state.path()).join("index.db");
    let persisted_source = || {
        let db = rusqlite::Connection::open(&index).unwrap();
        let (payload, native_bytes, native_hash): (String, Vec<u8>, String) = db
            .query_row(
                "SELECT f.payload,d.source_bytes,d.content_hash FROM files f JOIN native_documents d ON d.path=f.path WHERE f.path='main.js'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        (
            serde_json::from_str::<baleyg::model::SourceFile>(&payload)
                .unwrap()
                .text,
            native_bytes,
            native_hash,
        )
    };
    let cancelled = IndexJobCoordinator::prepare(&store, Some(first)).unwrap();
    cancel.store(true, Ordering::Release);
    assert!(
        cancelled
            .run(&options, &cancel, |_| {})
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    cancel.store(false, Ordering::Release);
    assert_eq!(store.index_baseline().unwrap(), first);
    assert_public_closed();
    assert_eq!(
        persisted_source(),
        (
            original.clone(),
            original.as_bytes().to_vec(),
            original_hash.clone()
        )
    );

    // Cancellation after admission and projection, before publication, also rolls back.
    fs::write(root.join("main.js"), "function late() {}\n").unwrap();
    let late = IndexJobCoordinator::prepare(&store, Some(first)).unwrap();
    let late_cancel = cancel.clone();
    let mut phases = Vec::new();
    let phase_log = std::sync::Mutex::new(&mut phases);
    let error = late
        .run(&options, &cancel, |p| {
            phase_log.lock().unwrap().push(p.phase.clone());
            if p.phase == "complete" {
                late_cancel.store(true, Ordering::Release);
            }
        })
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error:#}");
    assert!(
        phases.iter().any(|p| p == "scan") && phases.iter().any(|p| p == "complete"),
        "cancel must follow capture: {phases:?}"
    );
    cancel.store(false, Ordering::Release);
    assert_eq!(store.index_baseline().unwrap(), first);
    assert_public_closed();
    assert_eq!(
        persisted_source(),
        (
            original.clone(),
            original.as_bytes().to_vec(),
            original_hash.clone()
        )
    );
    fs::write(root.join("main.js"), &original).unwrap();

    let drift = IndexJobCoordinator::prepare(&store, Some(first)).unwrap();
    let error = drift
        .run(&options, &cancel, |p| {
            if p.phase == "parse" {
                fs::write(
                    root.join("main.js"),
                    "function changed() {}
",
                )
                .unwrap();
            }
        })
        .unwrap_err();
    assert!(error.to_string().contains("drift"), "{error:#}");
    assert_eq!(store.index_baseline().unwrap(), first);
    assert_public_closed();
    assert_eq!(
        persisted_source(),
        (
            original.clone(),
            original.as_bytes().to_vec(),
            original_hash.clone()
        )
    );
    let stale = IndexJobCoordinator::prepare(
        &store,
        Some(baleyg::model::IndexPin {
            index_generation: first.index_generation,
            index_revision: first.index_revision - 1,
        }),
    );
    assert!(
        matches!(stale, Err(error) if error.to_string().contains("revision conflict")),
        "stale pair must refuse before work"
    );
    assert_eq!(store.index_baseline().unwrap(), first);
    assert_public_closed();
    assert_eq!(
        persisted_source(),
        (
            original.clone(),
            original.as_bytes().to_vec(),
            original_hash.clone()
        )
    );
    let next = IndexJobCoordinator::prepare(&store, Some(first))
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();
    assert_eq!(next.index_generation, first.index_generation);
    assert_eq!(next.index_revision, first.index_revision + 1);
    assert!(store.source_at("main.js", Some(first)).is_err());
    assert_eq!(
        store
            .source_at("main.js", Some(next))
            .unwrap()
            .unwrap()
            .1
            .text,
        "function changed() {}
"
    );
}

#[test]
fn full_reconcile_replaces_persisted_source_and_input_inventory() {
    use baleyg::{
        index_coordinator::IndexJobCoordinator, indexer::IndexOptions, model::CancelFlag,
    };
    use std::sync::{Arc, atomic::AtomicBool};
    let (state, workspace) = fixture();
    fs::write(workspace.path().join("one.js"), "function one() {}\n").unwrap();
    fs::write(
        workspace.path().join("package.json"),
        "{\"name\":\"first\"}\n",
    )
    .unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let options = IndexOptions::new(workspace.path().to_owned());
    let first = IndexJobCoordinator::prepare(&store, None)
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();
    fs::remove_file(workspace.path().join("one.js")).unwrap();
    fs::write(workspace.path().join("two.js"), "function two() {}\n").unwrap();
    fs::write(
        workspace.path().join("package.json"),
        "{\"name\":\"second\"}\n",
    )
    .unwrap();
    let second = IndexJobCoordinator::prepare(&store, Some(first))
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();
    assert_eq!(second.index_generation, first.index_generation);
    assert_eq!(second.index_revision, first.index_revision + 1);

    let db = rusqlite::Connection::open(index_dir(state.path()).join("index.db")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        7
    );
    let files = db
        .prepare("SELECT path FROM files ORDER BY path")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(files, vec!["two.js"]);
    let capture_stat: String = db
        .query_row(
            "SELECT capture_stat FROM files WHERE path='two.js'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&capture_stat).unwrap()["version"],
        1
    );
    let package: String = db
        .query_row(
            "SELECT payload FROM capture_inputs WHERE input_key='config:package.json'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&package).unwrap()["state"],
        "present"
    );
    let absent: i64 = db
        .query_row(
            "SELECT count(*) FROM capture_inputs WHERE payload='{\"state\":\"absent\"}'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(absent > 0);
    let options_json: String = db
        .query_row("SELECT reconcile_options FROM index_metadata", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&options_json).unwrap()["maxFileBytes"],
        2 * 1024 * 1024
    );
}

#[test]
fn schema_six_rebuild_is_same_file_with_fresh_generation_and_revision_one() {
    use baleyg::{
        index_coordinator::IndexJobCoordinator, indexer::IndexOptions, model::CancelFlag,
    };
    use std::{
        os::unix::fs::MetadataExt,
        sync::{Arc, atomic::AtomicBool},
    };
    let (state, workspace) = fixture();
    fs::write(workspace.path().join("one.js"), "function one() {}\n").unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let options = IndexOptions::new(workspace.path().to_owned());
    let current = IndexJobCoordinator::prepare(&store, None)
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();
    let path = index_dir(state.path()).join("index.db");
    let inode = fs::metadata(&path).unwrap().ino();
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "DROP TABLE capture_inputs;
        ALTER TABLE files DROP COLUMN capture_stat;
        ALTER TABLE index_metadata DROP COLUMN reconcile_options;
        ALTER TABLE index_metadata DROP COLUMN reconciled_incarnation;",
    )
    .unwrap();
    db.execute("UPDATE index_metadata SET schema_version=6", [])
        .unwrap();
    db.pragma_update(None, "user_version", 6).unwrap();
    drop(db);

    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    assert_eq!(store.index_baseline().unwrap(), current);
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    let rebuilt = IndexJobCoordinator::prepare(&store, Some(current))
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();
    assert_ne!(rebuilt.index_generation, current.index_generation);
    assert_eq!(rebuilt.index_revision, 1);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(store.status().unwrap().revision, rebuilt);
}

fn sqlite_snapshot(path: &Path) -> Vec<(String, Vec<Vec<String>>)> {
    use rusqlite::types::Value;
    let db = rusqlite::Connection::open(path).unwrap();
    let tables = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name!='index_metadata' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let escaped = table.replace('"', "\"\"");
            let mut statement = db.prepare(&format!("SELECT * FROM \"{escaped}\"")).unwrap();
            let columns = statement.column_count();
            let mut rows = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|column| match row.get::<_, Value>(column)? {
                            Value::Null => Ok("null".to_owned()),
                            Value::Integer(value) => Ok(format!("i:{value}")),
                            Value::Real(value) => Ok(format!("r:{value:?}")),
                            Value::Text(value) => Ok(format!("t:{value}")),
                            Value::Blob(value) => Ok(format!("b:{}", hex::encode(value))),
                        })
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            rows.sort();
            (table, rows)
        })
        .collect()
}

#[test]
fn reconcile_matches_fresh_full_snapshot_after_add_edit_delete_rename_and_ignore_change() {
    use baleyg::{
        index_coordinator::IndexJobCoordinator, indexer::IndexOptions, model::CancelFlag,
    };
    use std::sync::{Arc, atomic::AtomicBool};
    let (state, workspace) = fixture();
    for (name, source) in [
        ("keep.js", "function keep() { oldCall(); }\n"),
        ("delete.js", "function deleted() {}\n"),
        ("rename-old.js", "function renamed() {}\n"),
        ("ignored.js", "function admittedAfterRuleChange() {}\n"),
    ] {
        fs::write(workspace.path().join(name), source).unwrap();
    }
    fs::write(workspace.path().join(".gitignore"), "ignored.js\n").unwrap();
    fs::write(
        workspace.path().join("package.json"),
        "{\"name\":\"before\"}\n",
    )
    .unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let options = IndexOptions::new(workspace.path().to_owned());
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let first = IndexJobCoordinator::prepare(&store, None)
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();

    fs::write(
        workspace.path().join("keep.js"),
        "function keep() { newCall(); }\n",
    )
    .unwrap();
    fs::remove_file(workspace.path().join("delete.js")).unwrap();
    fs::rename(
        workspace.path().join("rename-old.js"),
        workspace.path().join("rename-new.js"),
    )
    .unwrap();
    fs::write(workspace.path().join("added.rs"), "fn added() {}\n").unwrap();
    fs::write(workspace.path().join(".gitignore"), "delete.js\n").unwrap();
    fs::write(
        workspace.path().join("package.json"),
        "{\"name\":\"after!\"}\n",
    )
    .unwrap();
    IndexJobCoordinator::prepare(&store, Some(first))
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();

    let fresh_state = tempfile::tempdir().unwrap();
    fs::set_permissions(fresh_state.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let fresh = Store::open_for_tests(fresh_state.path(), workspace.path()).unwrap();
    IndexJobCoordinator::prepare(&fresh, None)
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();
    let reconciled_path = index_dir(state.path()).join("index.db");
    let fresh_path = index_dir(fresh_state.path()).join("index.db");
    assert_eq!(
        sqlite_snapshot(&reconciled_path),
        sqlite_snapshot(&fresh_path)
    );
    let metadata = |path: &Path| {
        let db = rusqlite::Connection::open(path).unwrap();
        db.query_row(
            "SELECT reconcile_options,stats,diagnostics FROM index_metadata",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .unwrap()
    };
    assert_eq!(metadata(&reconciled_path), metadata(&fresh_path));
}

#[cfg(unix)]
#[test]
fn full_scan_detects_same_size_preserved_mtime_edit_through_persisted_ctime() {
    use baleyg::{
        index_coordinator::IndexJobCoordinator, indexer::IndexOptions, model::CancelFlag,
    };
    use std::{
        os::unix::{ffi::OsStrExt, fs::MetadataExt},
        sync::{Arc, atomic::AtomicBool},
    };
    let (state, workspace) = fixture();
    let source = workspace.path().join("same.js");
    fs::write(&source, "function same() { aa(); }\n").unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let options = IndexOptions::new(workspace.path().to_owned());
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let first = IndexJobCoordinator::prepare(&store, None)
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();
    let path = index_dir(state.path()).join("index.db");
    let read_stat = || {
        let db = rusqlite::Connection::open(&path).unwrap();
        let payload: String = db
            .query_row(
                "SELECT capture_stat FROM files WHERE path='same.js'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str::<serde_json::Value>(&payload).unwrap()
    };
    let before = read_stat();
    let metadata = fs::metadata(&source).unwrap();
    fs::write(&source, "function same() { bb(); }\n").unwrap();
    let path_bytes = source.as_os_str().as_bytes();
    let c_path = std::ffi::CString::new(path_bytes).unwrap();
    let times = [
        libc::timespec {
            tv_sec: metadata.atime(),
            tv_nsec: metadata.atime_nsec(),
        },
        libc::timespec {
            tv_sec: metadata.mtime(),
            tv_nsec: metadata.mtime_nsec(),
        },
    ];
    assert_eq!(
        unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) },
        0
    );
    let second = IndexJobCoordinator::prepare(&store, Some(first))
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();
    let after = read_stat();
    assert_eq!(before["size"], after["size"]);
    assert_eq!(before["mtimeSeconds"], after["mtimeSeconds"]);
    assert_eq!(before["mtimeNanoseconds"], after["mtimeNanoseconds"]);
    assert_ne!(
        (
            before["ctimeSeconds"].clone(),
            before["ctimeNanoseconds"].clone()
        ),
        (
            after["ctimeSeconds"].clone(),
            after["ctimeNanoseconds"].clone()
        )
    );
    assert_eq!(second.index_generation, first.index_generation);
    assert_eq!(second.index_revision, first.index_revision + 1);
    assert!(
        store
            .source_at("same.js", Some(second))
            .unwrap()
            .unwrap()
            .1
            .text
            .contains("bb()")
    );
}

#[test]
fn extractor_and_typed_mismatch_rebuild_in_place_and_failed_rebuild_stays_closed() {
    use baleyg::{
        index_coordinator::IndexJobCoordinator, indexer::IndexOptions, model::CancelFlag,
    };
    use std::{
        os::unix::fs::MetadataExt,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    let (state, workspace) = fixture();
    fs::write(workspace.path().join("one.js"), "function one() {}\n").unwrap();
    fs::write(workspace.path().join("good.js"), "function good() {}\n").unwrap();
    let options = IndexOptions::new(workspace.path().to_owned());
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let first = IndexJobCoordinator::prepare(&store, None)
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();
    let path = index_dir(state.path()).join("index.db");
    let inode = fs::metadata(&path).unwrap().ino();
    drop(store);

    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE index_metadata SET extractor_version='obsolete-extractor'",
        [],
    )
    .unwrap();
    drop(db);
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    let rebuilt = IndexJobCoordinator::prepare(&store, Some(first))
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();
    assert_eq!(rebuilt.index_revision, 1);
    assert_ne!(rebuilt.index_generation, first.index_generation);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    drop(store);

    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE files SET payload='not-json' WHERE path='one.js'",
        [],
    )
    .unwrap();
    drop(db);
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let clone = store.clone();
    let separate = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    assert_eq!(store.status().unwrap().revision, rebuilt);
    assert!(
        store
            .source_at("missing.js", Some(rebuilt))
            .unwrap()
            .is_none()
    );
    let wrong = baleyg::model::IndexPin {
        index_generation: uuid::Uuid::new_v4(),
        index_revision: rebuilt.index_revision,
    };
    assert!(store.source_at("good.js", Some(wrong)).is_err());
    assert_eq!(store.status().unwrap().revision, rebuilt);
    let blocker = rusqlite::Connection::open(&path).unwrap();
    blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let busy = store.source_at("good.js", Some(rebuilt)).unwrap_err();
    assert!(busy.to_string().contains("storage_busy"), "{busy:#}");
    blocker.execute_batch("ROLLBACK").unwrap();
    assert_eq!(store.status().unwrap().revision, rebuilt);
    assert!(store.source_at("good.js", Some(rebuilt)).unwrap().is_some());
    assert_eq!(separate.status().unwrap().revision, rebuilt);
    assert!(
        separate
            .source_at("good.js", Some(rebuilt))
            .unwrap()
            .is_some()
    );
    let selected = store.source_at("one.js", Some(rebuilt)).unwrap_err();
    assert!(
        selected.to_string().contains("incompatible_index"),
        "{selected:#}"
    );
    assert!(store.status().is_err());
    assert!(clone.status().is_err());
    assert_eq!(separate.status().unwrap().revision, rebuilt);
    assert!(separate.source_at("one.js", Some(rebuilt)).is_err());
    assert!(separate.status().is_err());
    drop(separate);

    let failed_cancel: CancelFlag = Arc::new(AtomicBool::new(true));
    let failure = IndexJobCoordinator::prepare(&store, Some(rebuilt))
        .unwrap()
        .run(&options, &failed_cancel, |_| {})
        .unwrap_err();
    assert!(failure.to_string().contains("cancelled"));
    let db = rusqlite::Connection::open(&path).unwrap();
    let malformed: String = db
        .query_row("SELECT payload FROM files WHERE path='one.js'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(malformed, "not-json");
    drop(db);
    assert!(store.status().is_err());
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    failed_cancel.store(false, Ordering::Release);
    let mut recovered = IndexJobCoordinator::prepare(&store, Some(rebuilt))
        .unwrap()
        .run(&options, &failed_cancel, |_| {})
        .unwrap();
    assert_eq!(recovered.index_revision, 1);
    assert_ne!(recovered.index_generation, rebuilt.index_generation);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(store.status().unwrap().revision, recovered);

    // A SQLite type mismatch in any normalized derived row is replaceable
    // derived corruption, not a hard Store-open failure.
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    let (syntax_id, lookup_key): (String, String) = db
        .query_row(
            "SELECT syntax_id,lookup_key FROM native_declarations WHERE lookup_key IS NOT NULL AND language='javascript' ORDER BY syntax_id LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        db.execute(
            "UPDATE native_declarations SET key_ordinal=CAST(key_ordinal + 0.5 AS REAL) WHERE syntax_id=?1",
            [&syntax_id],
        )
        .unwrap(),
        1
    );
    drop(db);
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    assert_eq!(store.status().unwrap().revision, recovered);
    let type_error = store
        .native_declarations_at(recovered, "javascript", &lookup_key)
        .unwrap_err();
    assert!(
        type_error.to_string().contains("incompatible_index"),
        "{type_error:#}"
    );
    assert!(store.status().is_err());
    failed_cancel.store(true, Ordering::Release);
    let failure = IndexJobCoordinator::prepare(&store, Some(recovered))
        .unwrap()
        .run(&options, &failed_cancel, |_| {})
        .unwrap_err();
    assert!(failure.to_string().contains("cancelled"), "{failure:#}");
    let db = rusqlite::Connection::open(&path).unwrap();
    let stored_type: String = db
        .query_row(
            "SELECT typeof(key_ordinal) FROM native_declarations WHERE syntax_id=?1",
            [&syntax_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_type, "real");
    drop(db);
    assert!(store.status().is_err());
    failed_cancel.store(false, Ordering::Release);
    let typed_recovered = IndexJobCoordinator::prepare(&store, Some(recovered))
        .unwrap()
        .run(&options, &failed_cancel, |_| {})
        .unwrap();
    assert_eq!(typed_recovered.index_revision, 1);
    assert_ne!(typed_recovered.index_generation, recovered.index_generation);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    recovered = typed_recovered;
    assert_eq!(store.status().unwrap().revision, recovered);

    // A newly acquired writer is a takeover until it completes publication.
    // The latch is shared by Store clones and remains closed after the guard drops.
    let clone = store.clone();
    drop(store.leader().unwrap());
    assert!(store.status().is_err());
    assert!(clone.status().is_err());
    // A separately opened Store performs its own full recovery admission.
    let separate = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    assert_eq!(separate.index_baseline().unwrap(), recovered);
    drop(separate);
    let next = IndexJobCoordinator::prepare(&clone, Some(recovered))
        .unwrap()
        .run(&options, &failed_cancel, |_| {})
        .unwrap();
    assert_eq!(clone.status().unwrap().revision, next);
    assert_eq!(store.status().unwrap().revision, next);
}

#[test]
fn bounded_control_decode_rebuilds_and_failed_attempt_stays_closed() {
    use baleyg::{
        index_coordinator::IndexJobCoordinator, indexer::IndexOptions, model::CancelFlag,
    };
    use std::{
        os::unix::fs::MetadataExt,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    let (state, workspace) = fixture();
    fs::write(workspace.path().join("one.js"), "function one() {}\n").unwrap();
    let options = IndexOptions::new(workspace.path().to_owned());
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let pin = IndexJobCoordinator::prepare(&store, None)
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();
    let path = index_dir(state.path()).join("index.db");
    let inode = fs::metadata(&path).unwrap().ino();
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE index_metadata SET stats='not-json'", [])
        .unwrap();
    drop(db);

    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    cancel.store(true, Ordering::Release);
    let failure = IndexJobCoordinator::prepare(&store, Some(pin))
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap_err();
    assert!(failure.to_string().contains("cancelled"), "{failure:#}");
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    let db = rusqlite::Connection::open(&path).unwrap();
    let stats: String = db
        .query_row("SELECT stats FROM index_metadata", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stats, "not-json");
    drop(db);
    assert!(store.status().is_err());

    cancel.store(false, Ordering::Release);
    let repaired = IndexJobCoordinator::prepare(&store, Some(pin))
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();
    assert_eq!(repaired.index_revision, 1);
    assert_ne!(repaired.index_generation, pin.index_generation);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(store.status().unwrap().revision, repaired);
}

#[test]
fn exceptional_format_is_typed_deferred_refusal_without_file_mutation() {
    use baleyg::{
        index_coordinator::IndexJobCoordinator, indexer::IndexOptions, model::CancelFlag,
    };
    use std::{
        io::{Seek, SeekFrom, Write},
        os::unix::fs::MetadataExt,
        sync::{Arc, atomic::AtomicBool},
    };
    let (state, workspace) = fixture();
    fs::write(workspace.path().join("one.js"), "function one() {}\n").unwrap();
    let options = IndexOptions::new(workspace.path().to_owned());
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    IndexJobCoordinator::prepare(&store, None)
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();
    let path = index_dir(state.path()).join("index.db");
    drop(store);
    let inode = fs::metadata(&path).unwrap().ino();
    let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"not sqlite format").unwrap();
    file.sync_all().unwrap();
    drop(file);
    let corrupt = fs::read(&path).unwrap();

    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    for refusal in [
        store.status().unwrap_err(),
        store.index_baseline().unwrap_err(),
        store
            .leader()
            .expect_err("exceptional format leader refusal"),
    ] {
        let text = refusal.to_string();
        assert!(
            text.contains("exceptional index recovery deferred"),
            "{refusal:#}"
        );
    }
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(fs::read(&path).unwrap(), corrupt);
}

#[test]
fn schema_seven_inventory_validation_rejects_missing_unknown_and_unsupported_state() {
    use baleyg::{
        index_coordinator::IndexJobCoordinator, indexer::IndexOptions, model::CancelFlag,
    };
    use std::sync::{Arc, atomic::AtomicBool};
    for mutation in [
        "DELETE FROM capture_inputs WHERE input_key='config:package.json'",
        "INSERT INTO capture_inputs(input_key,payload) VALUES('unknown:slot','{\"state\":\"absent\"}')",
        "UPDATE index_metadata SET reconcile_options=json_set(reconcile_options,'$.version',2)",
    ] {
        let (state, workspace) = fixture();
        fs::write(workspace.path().join("one.js"), "function one() {}\n").unwrap();
        let scip = workspace.path().join("labels.scip");
        let manifest = workspace.path().join("labels.json");
        fs::write(&scip, b"captured presentation bytes").unwrap();
        fs::write(&manifest, b"{}").unwrap();
        let mut options = IndexOptions::new(workspace.path().to_owned());
        options.scip_path = Some(scip.clone());
        options.manifest_path = Some(manifest.clone());
        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
        let pin = IndexJobCoordinator::prepare(&store, None)
            .unwrap()
            .run(&options, &cancel, |_| {})
            .unwrap();
        let path = index_dir(state.path()).join("index.db");
        drop(store);
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(mutation).unwrap();
        drop(db);
        let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
        assert!(
            store.status().is_err(),
            "invalid inventory was publicly readable: {mutation}"
        );
        let repaired = IndexJobCoordinator::prepare(&store, Some(pin))
            .unwrap()
            .run(&options, &cancel, |_| {})
            .unwrap();
        assert_eq!(repaired.index_revision, 1);
        assert_ne!(repaired.index_generation, pin.index_generation);
        assert_eq!(store.status().unwrap().revision, repaired);
        let db = rusqlite::Connection::open(&path).unwrap();
        let keys = db
            .prepare("SELECT input_key FROM capture_inputs ORDER BY input_key")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(keys.contains(&format!("presentation-scip:{}", scip.display())));
        assert!(keys.contains(&format!("presentation-manifest:{}", manifest.display())));
        assert!(keys.contains(&"config:package.json".to_owned()));
        assert!(!keys.iter().any(|key| key.starts_with("unknown:")));
    }
}

#[test]
fn selected_projection_json_failures_latch_only_selected_reads() {
    // Selected file JSON is bounded to the requested page.
    let (state, _workspace, store, pin) = projection_fixture();
    let clone = store.clone();
    let db = rusqlite::Connection::open(index_dir(state.path()).join("index.db")).unwrap();
    db.execute(
        "UPDATE files SET payload='not-json' WHERE path='flow.js'",
        [],
    )
    .unwrap();
    let error = store.files_at(Some(pin), 0, 10).unwrap_err();
    assert!(
        error.to_string().contains("incompatible_index"),
        "{error:#}"
    );
    assert!(
        clone
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );

    // A matching invalid node is selected by the page even when file JSON is valid.
    let (state, _workspace, store, pin) = projection_fixture();
    let clone = store.clone();
    let db = rusqlite::Connection::open(index_dir(state.path()).join("index.db")).unwrap();
    db.execute(
        "UPDATE nodes SET payload='not-json' WHERE path='flow.js'",
        [],
    )
    .unwrap();
    let error = store.files_at(Some(pin), 0, 10).unwrap_err();
    assert!(
        error.to_string().contains("incompatible_index"),
        "{error:#}"
    );
    assert!(
        clone
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );

    // The methods route types both invalid JSON and valid JSON with bad Symbol shape.
    for payload in ["not-json", r#"{"kind":"function"}"#] {
        let (state, _workspace, store, pin) = projection_fixture();
        let clone = store.clone();
        let db = rusqlite::Connection::open(index_dir(state.path()).join("index.db")).unwrap();
        db.execute(
            "UPDATE nodes SET payload=?1 WHERE path='flow.js'",
            [payload],
        )
        .unwrap();
        let error = store.methods_at("flow.js", Some(pin)).unwrap_err();
        assert!(
            error.to_string().contains("incompatible_index"),
            "{error:#}"
        );
        assert!(
            clone
                .status()
                .unwrap_err()
                .to_string()
                .contains("index_not_ready")
        );
    }

    // A valid selected file JSON value with a non-string language is a typed
    // SQLite extraction conversion, not a generic SQLITE_ERROR.
    let (state, _workspace, store, pin) = projection_fixture();
    let clone = store.clone();
    let db = rusqlite::Connection::open(index_dir(state.path()).join("index.db")).unwrap();
    db.execute(
        "UPDATE files SET payload=json_set(payload,'$.language',7) WHERE path='flow.js'",
        [],
    )
    .unwrap();
    let error = store.files_at(Some(pin), 0, 10).unwrap_err();
    assert!(
        error.to_string().contains("incompatible_index"),
        "{error:#}"
    );
    assert!(
        clone
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );

    // Tree enrichment evaluates only its visible file and latches its invalid node.
    let (state, workspace, store, _pin) = projection_fixture();
    let clone = store.clone();
    let db = rusqlite::Connection::open(index_dir(state.path()).join("index.db")).unwrap();
    db.execute(
        "UPDATE nodes SET payload='not-json' WHERE path='flow.js'",
        [],
    )
    .unwrap();
    let mut items = vec![baleyg::file_tree::Entry {
        name: "flow.js".into(),
        path: "flow.js".into(),
        kind: "file",
        indexed_path: None,
        method_count: None,
        unindexed_reason: None,
    }];
    let tree_root = workspace.path().canonicalize().unwrap();
    let error = store.tree_metadata(&tree_root, &mut items).unwrap_err();
    assert!(
        error.to_string().contains("incompatible_index"),
        "{error:#}"
    );
    assert!(
        clone
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );

    // Benign absence and a bad pin remain non-latching request outcomes.
    let (_state, _workspace, store, pin) = projection_fixture();
    let clone = store.clone();
    assert!(store.methods_at("missing.js", Some(pin)).unwrap().is_none());
    let wrong = baleyg::model::IndexPin {
        index_generation: uuid::Uuid::new_v4(),
        index_revision: pin.index_revision,
    };
    assert!(
        store
            .files_at(Some(wrong), 0, 10)
            .unwrap_err()
            .to_string()
            .contains("revision conflict")
    );
    assert_eq!(store.status().unwrap().revision, pin);
    assert_eq!(clone.status().unwrap().revision, pin);
}

#[test]
fn selected_projection_rebuild_is_same_inode_and_clears_clones_only_after_commit() {
    use baleyg::{
        index_coordinator::IndexJobCoordinator, indexer::IndexOptions, model::CancelFlag,
    };
    use std::os::unix::fs::MetadataExt;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    let (state, workspace, store, pin) = projection_fixture();
    let clone = store.clone();
    let path = index_dir(state.path()).join("index.db");
    let inode = fs::metadata(&path).unwrap().ino();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE files SET payload='not-json' WHERE path='flow.js'",
        [],
    )
    .unwrap();
    drop(db);
    let error = store.files_at(Some(pin), 0, 10).unwrap_err();
    assert!(
        error.to_string().contains("incompatible_index"),
        "{error:#}"
    );
    assert!(
        clone
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );

    let cancel: CancelFlag = Arc::new(AtomicBool::new(true));
    let failed = IndexJobCoordinator::prepare(&store, Some(pin))
        .unwrap()
        .run(
            &IndexOptions::new(workspace.path().to_owned()),
            &cancel,
            |_| {},
        )
        .unwrap_err();
    assert!(failed.to_string().contains("cancelled"), "{failed:#}");
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    assert!(
        clone
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);

    cancel.store(false, Ordering::Release);
    let recovered = IndexJobCoordinator::prepare(&store, Some(pin))
        .unwrap()
        .run(
            &IndexOptions::new(workspace.path().to_owned()),
            &cancel,
            |_| {},
        )
        .unwrap();
    assert_eq!(recovered.index_revision, 1);
    assert_ne!(recovered.index_generation, pin.index_generation);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(store.status().unwrap().revision, recovered);
    assert_eq!(clone.status().unwrap().revision, recovered);
    assert_eq!(
        store.files_at(Some(recovered), 0, 10).unwrap()["items"][0]["path"],
        "flow.js"
    );
}

#[test]
fn metadata_schema_marker_mismatch_rebuilds_with_valid_pin_same_inode() {
    use baleyg::{
        index_coordinator::IndexJobCoordinator, indexer::IndexOptions, model::CancelFlag,
    };
    use std::{
        os::unix::fs::MetadataExt,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    let (state, workspace, initial, old) = projection_fixture();
    let path = index_dir(state.path()).join("index.db");
    let inode = fs::metadata(&path).unwrap().ino();
    drop(initial);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE index_metadata SET schema_version=6", [])
        .unwrap();
    drop(db);

    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let clone = store.clone();
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    assert!(
        clone
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    assert_eq!(store.index_baseline().unwrap(), old);

    let stale = IndexJobCoordinator::prepare(&store, Some(old)).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE index_metadata SET extractor_version='changed-after-admission'",
        [],
    )
    .unwrap();
    drop(db);
    let error = stale
        .run(
            &IndexOptions::new(workspace.path().to_owned()),
            &Arc::new(AtomicBool::new(false)),
            |_| {},
        )
        .unwrap_err();
    assert!(error.to_string().contains("revision conflict"), "{error:#}");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE index_metadata SET extractor_version='native-paired-v1'",
        [],
    )
    .unwrap();
    drop(db);

    let cancel: CancelFlag = Arc::new(AtomicBool::new(true));
    let error = IndexJobCoordinator::prepare(&store, Some(old))
        .unwrap()
        .run(
            &IndexOptions::new(workspace.path().to_owned()),
            &cancel,
            |_| {},
        )
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error:#}");
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("SELECT schema_version FROM index_metadata", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        6
    );
    drop(db);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );

    cancel.store(false, Ordering::Release);
    let recovered = IndexJobCoordinator::prepare(&store, Some(old))
        .unwrap()
        .run(
            &IndexOptions::new(workspace.path().to_owned()),
            &cancel,
            |_| {},
        )
        .unwrap();
    assert_eq!(recovered.index_revision, 1);
    assert_ne!(recovered.index_generation, old.index_generation);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(store.status().unwrap().revision, recovered);
    assert_eq!(clone.status().unwrap().revision, recovered);
    assert!(
        IndexJobCoordinator::prepare(&store, Some(old))
            .err()
            .expect("old pin must conflict")
            .to_string()
            .contains("revision conflict")
    );
}

#[test]
fn metadata_real_revision_uses_private_witness_and_recovers_same_inode() {
    use baleyg::{
        index_coordinator::IndexJobCoordinator, indexer::IndexOptions, model::CancelFlag,
    };
    use rusqlite::types::ValueRef;
    use std::{
        os::unix::fs::MetadataExt,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    let (state, workspace, initial, old) = projection_fixture();
    let path = index_dir(state.path()).join("index.db");
    let inode = fs::metadata(&path).unwrap().ino();
    drop(initial);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE index_metadata SET index_revision=CAST(1.5 AS REAL)",
        [],
    )
    .unwrap();
    drop(db);

    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let clone = store.clone();
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    assert!(
        store
            .index_baseline()
            .unwrap_err()
            .to_string()
            .contains("not decodable")
    );
    assert!(
        IndexJobCoordinator::prepare(&store, Some(old))
            .err()
            .expect("undecodable pin must conflict")
            .to_string()
            .contains("revision conflict")
    );

    let cancel: CancelFlag = Arc::new(AtomicBool::new(true));
    let error = IndexJobCoordinator::prepare(&store, None)
        .unwrap()
        .run(
            &IndexOptions::new(workspace.path().to_owned()),
            &cancel,
            |_| {},
        )
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error:#}");
    let db = rusqlite::Connection::open(&path).unwrap();
    let bits = db
        .query_row(
            "SELECT index_revision FROM index_metadata",
            [],
            |row| match row.get_ref(0)? {
                ValueRef::Real(value) => Ok(value.to_bits()),
                _ => Err(rusqlite::Error::InvalidQuery),
            },
        )
        .unwrap();
    assert_eq!(bits, 1.5f64.to_bits());
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    drop(db);
    assert!(
        clone
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );

    cancel.store(false, Ordering::Release);
    let recovered = IndexJobCoordinator::prepare(&store, None)
        .unwrap()
        .run(
            &IndexOptions::new(workspace.path().to_owned()),
            &cancel,
            |_| {},
        )
        .unwrap();
    assert_eq!(recovered.index_revision, 1);
    assert_ne!(recovered.index_generation, old.index_generation);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(store.status().unwrap().revision, recovered);
    assert_eq!(clone.status().unwrap().revision, recovered);
}

#[test]
fn private_metadata_witness_distinguishes_nul_real_and_multichunk_blob() {
    use baleyg::{index_coordinator::IndexJobCoordinator, indexer::IndexOptions};
    use rusqlite::MAIN_DB;
    use std::{
        os::unix::fs::MetadataExt,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    let conflict = |first: &str, mutate: &dyn Fn(&rusqlite::Connection)| {
        let (state, workspace, initial, _old) = projection_fixture();
        let path = index_dir(state.path()).join("index.db");
        drop(initial);
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute(first, []).unwrap();
        drop(db);
        let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
        let coordinator = IndexJobCoordinator::prepare(&store, None).unwrap();
        let db = rusqlite::Connection::open(&path).unwrap();
        mutate(&db);
        drop(db);
        let error = coordinator
            .run(
                &IndexOptions::new(workspace.path().to_owned()),
                &Arc::new(AtomicBool::new(false)),
                |_| {},
            )
            .unwrap_err();
        assert!(error.to_string().contains("revision conflict"), "{error:#}");
        assert!(
            store
                .status()
                .unwrap_err()
                .to_string()
                .contains("index_not_ready")
        );
    };

    conflict(
        "UPDATE index_metadata SET index_generation='bad' || char(0) || 'one'",
        &|db| {
            db.execute(
                "UPDATE index_metadata SET index_generation='bad' || char(0) || 'two'",
                [],
            )
            .unwrap();
        },
    );
    conflict(
        "UPDATE index_metadata SET index_revision=CAST(1.5 AS REAL)",
        &|db| {
            db.execute(
                "UPDATE index_metadata SET index_revision=?1",
                [f64::from_bits(1.5f64.to_bits() + 1)],
            )
            .unwrap();
        },
    );

    let (state, workspace, initial, _old) = projection_fixture();
    let path = index_dir(state.path()).join("index.db");
    drop(initial);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE index_metadata SET index_generation=zeroblob(131073)",
        [],
    )
    .unwrap();
    drop(db);
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let coordinator = IndexJobCoordinator::prepare(&store, None).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    let mut blob = db
        .blob_open(MAIN_DB, "index_metadata", "index_generation", 1, false)
        .unwrap();
    blob.write_at(&[1], 70_000).unwrap();
    blob.close().unwrap();
    drop(db);
    let error = coordinator
        .run(
            &IndexOptions::new(workspace.path().to_owned()),
            &Arc::new(AtomicBool::new(false)),
            |_| {},
        )
        .unwrap_err();
    assert!(error.to_string().contains("revision conflict"), "{error:#}");
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    let inode = fs::metadata(&path).unwrap().ino();
    let cancel = Arc::new(AtomicBool::new(true));
    let error = IndexJobCoordinator::prepare(&store, None)
        .unwrap()
        .run(
            &IndexOptions::new(workspace.path().to_owned()),
            &cancel,
            |_| {},
        )
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error:#}");
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    cancel.store(false, Ordering::Release);
    let recovered = IndexJobCoordinator::prepare(&store, None)
        .unwrap()
        .run(
            &IndexOptions::new(workspace.path().to_owned()),
            &cancel,
            |_| {},
        )
        .unwrap();
    assert_eq!(recovered.index_revision, 1);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(store.status().unwrap().revision, recovered);

    let (state, workspace, initial, _old) = projection_fixture();
    let path = index_dir(state.path()).join("index.db");
    drop(initial);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE index_metadata SET root_spelling='wrong' || char(0) || 'root',index_revision=CAST(1.5 AS REAL)", []).unwrap();
    drop(db);
    let error = Store::open_for_tests(state.path(), workspace.path()).unwrap_err();
    assert!(
        error.to_string().contains("root_key_collision"),
        "{error:#}"
    );

    let (state, workspace, initial, _old) = projection_fixture();
    let path = index_dir(state.path()).join("index.db");
    drop(initial);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE index_metadata SET root_spelling=CAST(zeroblob(131073) AS TEXT),index_revision=CAST(1.5 AS REAL)", []).unwrap();
    drop(db);
    let error = Store::open_for_tests(state.path(), workspace.path()).unwrap_err();
    assert!(
        error.to_string().contains("root_key_collision"),
        "{error:#}"
    );

    let (state, workspace, initial, _old) = projection_fixture();
    let path = index_dir(state.path()).join("index.db");
    drop(initial);
    let locker = rusqlite::Connection::open(&path).unwrap();
    locker.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let error = Store::open_for_tests(state.path(), workspace.path()).unwrap_err();
    assert!(error.to_string().contains("storage_busy"), "{error:#}");
    locker.execute_batch("ROLLBACK").unwrap();
    assert!(Store::open_for_tests(state.path(), workspace.path()).is_ok());
}

#[test]
fn live_control_decode_corruption_latches_direct_reads_and_control_clones() {
    // Malformed stats must be reported by a direct pinned read, not only status.
    let (state, _workspace, store, pin) = projection_fixture();
    let clone = store.clone();
    let path = index_dir(state.path()).join("index.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE index_metadata SET stats='not-json'", [])
        .unwrap();
    drop(db);
    let error = store.source_at("flow.js", Some(pin)).unwrap_err();
    assert!(
        error.to_string().starts_with("incompatible_index:"),
        "{error:#}"
    );
    assert!(
        clone
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    assert!(
        store
            .views()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    assert!(
        store
            .view("missing")
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    assert!(
        store
            .annotations()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );

    // REAL revision must fail before pin comparison and close every clone.
    let (state, _workspace, store, pin) = projection_fixture();
    let clone = store.clone();
    let symbol = store.symbols_at("go", 10).unwrap().1.remove(0).id;
    let path = index_dir(state.path()).join("index.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE index_metadata SET index_revision=CAST(1.5 AS REAL)",
        [],
    )
    .unwrap();
    drop(db);
    let error = store.symbol_at(&symbol, Some(pin)).unwrap_err();
    assert!(
        error.to_string().starts_with("incompatible_index:"),
        "{error:#}"
    );
    assert!(
        clone
            .views()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );

    // Malformed persisted input is reached by the existing full-read inventory pass.
    let (state, _workspace, store, pin) = projection_fixture();
    let clone = store.clone();
    let path = index_dir(state.path()).join("index.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE capture_inputs SET payload='not-json' WHERE input_key='root:.'",
        [],
    )
    .unwrap();
    drop(db);
    let error = store.classes_at(None, "", Some(pin), 0, 10).unwrap_err();
    assert!(
        error.to_string().starts_with("incompatible_index:"),
        "{error:#}"
    );
    assert!(
        clone
            .annotations()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
}

#[test]
fn live_structural_inventory_and_control_first_reads_fail_closed() {
    // This is an already executed structural check, not a new derived invariant.
    let (state, _workspace, store, pin) = projection_fixture();
    let clone = store.clone();
    let path = index_dir(state.path()).join("index.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE capture_inputs SET input_key='unexpected:role' WHERE input_key='root:.'",
        [],
    )
    .unwrap();
    drop(db);
    let error = store.source_at("flow.js", Some(pin)).unwrap_err();
    assert!(
        error.to_string().starts_with("incompatible_index:"),
        "{error:#}"
    );
    assert!(
        clone
            .views()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    assert!(
        clone
            .view("missing")
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
    assert!(
        clone
            .annotations()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );

    // A control-only read can also be the first observer of typed control corruption.
    let (state, _workspace, store, _pin) = projection_fixture();
    let clone = store.clone();
    let path = index_dir(state.path()).join("index.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE index_metadata SET stats='not-json'", [])
        .unwrap();
    drop(db);
    let error = store.views().unwrap_err();
    assert!(
        error.to_string().starts_with("incompatible_index:"),
        "{error:#}"
    );
    assert!(
        clone
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );
}

#[test]
fn live_root_mismatch_precedes_typed_control_corruption_without_latching() {
    let (state, _workspace, store, pin) = projection_fixture();
    let clone = store.clone();
    let path = index_dir(state.path()).join("index.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    let original_root: String = db
        .query_row("SELECT root_spelling FROM index_metadata", [], |row| {
            row.get(0)
        })
        .unwrap();
    db.execute(
        "UPDATE index_metadata SET root_spelling='wrong' || char(0) || 'root',index_revision=CAST(1.5 AS REAL)",
        [],
    )
    .unwrap();
    drop(db);
    let error = store.source_at("flow.js", Some(pin)).unwrap_err();
    assert!(
        error.to_string().starts_with("root_key_collision:"),
        "{error:#}"
    );

    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE index_metadata SET root_spelling=?1,index_revision=?2",
        rusqlite::params![original_root, pin.index_revision as i64],
    )
    .unwrap();
    drop(db);
    assert_eq!(
        clone.status().unwrap().revision,
        pin,
        "root failure must not latch"
    );
}

#[test]
fn live_stats_recovery_cancel_then_success_preserves_inode_and_rotates_pin() {
    use baleyg::{
        index_coordinator::IndexJobCoordinator, indexer::IndexOptions, model::CancelFlag,
    };
    use std::{
        os::unix::fs::MetadataExt,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    let (state, workspace, store, old) = projection_fixture();
    let clone = store.clone();
    let path = index_dir(state.path()).join("index.db");
    let inode = fs::metadata(&path).unwrap().ino();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE index_metadata SET stats='not-json'", [])
        .unwrap();
    drop(db);
    assert!(
        store
            .source_at("flow.js", Some(old))
            .unwrap_err()
            .to_string()
            .starts_with("incompatible_index:")
    );

    let cancel: CancelFlag = Arc::new(AtomicBool::new(true));
    let error = IndexJobCoordinator::prepare(&store, Some(old))
        .unwrap()
        .run(
            &IndexOptions::new(workspace.path().to_owned()),
            &cancel,
            |_| {},
        )
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error:#}");
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("SELECT stats FROM index_metadata", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "not-json"
    );
    drop(db);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert!(
        clone
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready")
    );

    cancel.store(false, Ordering::Release);
    let recovered = IndexJobCoordinator::prepare(&store, Some(old))
        .unwrap()
        .run(
            &IndexOptions::new(workspace.path().to_owned()),
            &cancel,
            |_| {},
        )
        .unwrap();
    assert_eq!(recovered.index_revision, 1);
    assert_ne!(recovered.index_generation, old.index_generation);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(store.status().unwrap().revision, recovered);
    assert_eq!(clone.status().unwrap().revision, recovered);
    assert!(
        IndexJobCoordinator::prepare(&store, Some(old))
            .err()
            .expect("old pin must conflict")
            .to_string()
            .contains("revision conflict")
    );
}

#[test]
fn live_control_corruption_status_first_is_typed_and_clone_shared() {
    for sql in [
        "UPDATE index_metadata SET stats='not-json'",
        "UPDATE index_metadata SET index_revision=CAST(1.5 AS REAL)",
        "UPDATE capture_inputs SET payload='not-json' WHERE input_key='root:.'",
    ] {
        let (state, _workspace, store, pin) = projection_fixture();
        let clone = store.clone();
        let path = index_dir(state.path()).join("index.db");
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute(sql, []).unwrap();
        drop(db);
        let error = store.status().unwrap_err();
        assert!(
            error.to_string().starts_with("incompatible_index:"),
            "{sql}: {error:#}"
        );
        assert!(
            clone
                .source_at("flow.js", Some(pin))
                .unwrap_err()
                .to_string()
                .contains("index_not_ready"),
            "{sql}"
        );
    }
}
