use baleyg::store::Store;
use std::{fs, os::unix::fs::PermissionsExt, path::Path, sync::mpsc};
use tempfile::TempDir;

fn fixture() -> (TempDir, TempDir) {
    let state = tempfile::tempdir().unwrap();
    fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
    (state, tempfile::tempdir().unwrap())
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
        error.to_string().contains("incompatible_index:"),
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
    let (state, workspace) = fixture();
    drop(Store::open_for_tests(state.path(), workspace.path()).unwrap());
    let index = index_dir(state.path()).join("index.db");
    fs::OpenOptions::new()
        .write(true)
        .open(&index)
        .unwrap()
        .set_len(0)
        .unwrap();
    let error = Store::open_for_tests(state.path(), workspace.path()).unwrap_err();
    assert!(
        error.to_string().contains("incompatible_index:"),
        "{error:#}"
    );
    assert_eq!(fs::metadata(index).unwrap().len(), 0);
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
    assert_eq!(store.status().unwrap().revision, first);

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
    assert_eq!(store.status().unwrap().revision, first);
    assert_eq!(
        store
            .source_at("main.js", Some(first))
            .unwrap()
            .unwrap()
            .1
            .text,
        original
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
    assert_eq!(store.status().unwrap().revision, first);
    assert_eq!(
        store
            .source_at("main.js", Some(first))
            .unwrap()
            .unwrap()
            .1
            .text,
        original
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
    assert_eq!(store.status().unwrap().revision, first);
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
