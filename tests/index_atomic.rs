use baleyg::store::Store;
use std::{fs, os::unix::fs::PermissionsExt, path::Path, sync::mpsc};
use tempfile::TempDir;

fn fixture() -> (TempDir, TempDir) {
    let state = tempfile::tempdir().unwrap();
    fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
    (state, tempfile::tempdir().unwrap())
}
fn projection_fixture() -> (
    TempDir,
    TempDir,
    Store,
    baleyg::model::IndexPin,
    std::sync::Arc<baleyg::store::topology::LeaderSession>,
) {
    use baleyg::{index_coordinator::IndexJobCoordinator, indexer::IndexOptions};
    use std::sync::{Arc, atomic::AtomicBool};
    let (state, workspace) = fixture();
    fs::write(
        workspace.path().join("flow.js"),
        "function go() { measured(); }\n",
    )
    .unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let coordinator = IndexJobCoordinator::prepare(&store, None).unwrap();
    let session = coordinator.session();
    let pin = coordinator
        .run(
            &IndexOptions::new(workspace.path().to_owned()),
            &Arc::new(AtomicBool::new(false)),
            |_| {},
        )
        .unwrap();
    (state, workspace, store, pin, session)
}
#[test]
fn v8_bootstrap_admits_only_empty_unpublished_evidence() {
    let (state, workspace) = fixture();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    assert_eq!(store.index_baseline().unwrap().index_revision, 0);
    assert!(store.recorded_index_options().unwrap().is_none());
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("index_not_ready"),
        "an empty bootstrap must not serve evidence"
    );
    let path = index_dir(state.path()).join("index.db");
    drop(store);
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute(
        "INSERT INTO native_source_sets(id,root_id) VALUES('forged-bootstrap','forged-root')",
        [],
    )
    .unwrap();
    drop(db);
    let reopened = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    assert!(
        reopened
            .status()
            .unwrap_err()
            .to_string()
            .contains("incompatible_index"),
        "partial v8 bootstrap must fail closed"
    );
    assert!(
        reopened
            .recorded_index_options()
            .expect_err("partial bootstrap cannot become an absent-option fallback")
            .to_string()
            .contains("partial v8 bootstrap")
    );
}

#[test]
fn published_v8_missing_reconcile_options_never_looks_like_a_bootstrap() {
    let (state, _workspace, store, pin, _session) = projection_fixture();
    assert_eq!(store.status().unwrap().revision, pin);
    let db = rusqlite::Connection::open(index_dir(state.path()).join("index.db")).unwrap();
    db.execute("UPDATE index_metadata SET reconcile_options=NULL", [])
        .unwrap();
    assert!(
        store
            .recorded_index_options()
            .expect_err("published v8 cannot omit recorded options")
            .to_string()
            .contains("missing reconcile options")
    );
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
fn published_sqlite_header_with_missing_pages_refuses_reads_until_explicit_recovery() {
    use baleyg::{index_coordinator::reconcile_workspace, indexer::IndexOptions};
    use std::{
        os::unix::fs::MetadataExt,
        sync::{Arc, atomic::AtomicBool},
    };

    let (state, workspace, store, old_pin, session) = projection_fixture();
    drop(session);
    drop(store);
    let dir = index_dir(state.path());
    let index = dir.join("index.db");
    let file = fs::OpenOptions::new().write(true).open(&index).unwrap();
    file.set_len(128).unwrap();
    drop(file);
    let old_bytes = fs::read(&index).unwrap();
    assert_eq!(&old_bytes[..16], b"SQLite format 3\0");
    assert!(old_bytes.len() > 20);
    let old_inode = fs::metadata(&index).unwrap().ino();

    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    assert!(
        store.status().is_err(),
        "a damaged published index must not be served"
    );
    assert!(
        store.source_at("flow.js", Some(old_pin)).is_err(),
        "a damaged source must not be served"
    );
    assert_eq!(fs::read(&index).unwrap(), old_bytes);
    assert_eq!(fs::metadata(&index).unwrap().ino(), old_inode);
    assert!(staged_files(&dir).is_empty());

    let options = IndexOptions::new(workspace.path().to_owned());
    let cancelled = Arc::new(AtomicBool::new(true));
    let failure = reconcile_workspace(&store, &options, &cancelled, |_| {}).unwrap_err();
    assert!(failure.to_string().contains("cancelled"), "{failure:#}");
    assert!(store.status().is_err());
    assert_eq!(fs::read(&index).unwrap(), old_bytes);
    assert_eq!(fs::metadata(&index).unwrap().ino(), old_inode);
    assert!(staged_files(&dir).is_empty());

    let ready = Arc::new(AtomicBool::new(false));
    let (recovered, _session) = reconcile_workspace(&store, &options, &ready, |_| {}).unwrap();
    assert_eq!(recovered.index_revision, 1);
    assert_ne!(recovered.index_generation, old_pin.index_generation);
    assert!(store.source_at("flow.js", Some(old_pin)).is_err());
    assert!(
        store
            .source_at("flow.js", Some(recovered))
            .unwrap()
            .is_some()
    );
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
    let leader = store.leader().unwrap();
    let before = publish_bundle(
        &store,
        &graph,
        root,
        &leader,
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
    let job = IndexJobCoordinator::prepare(&store, None).unwrap();
    let session = job.session();
    let first = job.run(&options, &cancel, |_| {}).unwrap();
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
        let (native_bytes, native_hash): (Vec<u8>, String) = db
            .query_row(
                "SELECT v.source_bytes,v.content_hash FROM document_versions v JOIN revision_documents m ON m.document_version_id=v.id WHERE m.path='main.js'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        (
            String::from_utf8(native_bytes.clone()).unwrap(),
            native_bytes,
            native_hash,
        )
    };
    let cancelled =
        IndexJobCoordinator::prepare_with_session(&store, Some(first), session.clone()).unwrap();
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
    assert_eq!(store.status().unwrap().revision, first);
    let (served_pin, served_source) = store.source_at("main.js", Some(first)).unwrap().unwrap();
    assert_eq!(served_pin, first);
    assert_eq!(served_source.text, original);
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
    let late =
        IndexJobCoordinator::prepare_with_session(&store, Some(first), session.clone()).unwrap();
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

    let drift =
        IndexJobCoordinator::prepare_with_session(&store, Some(first), session.clone()).unwrap();
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
    let stale = IndexJobCoordinator::prepare_with_session(
        &store,
        Some(baleyg::model::IndexPin {
            index_generation: first.index_generation,
            index_revision: first.index_revision - 1,
        }),
        session.clone(),
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
    let next = IndexJobCoordinator::prepare_with_session(&store, Some(first), session.clone())
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
        8
    );
    let files = db
        .prepare("SELECT path FROM revision_documents WHERE revision_id=?1 ORDER BY path")
        .unwrap()
        .query_map(
            [format!(
                "pin:v1:{}:{}",
                second.index_generation, second.index_revision
            )],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(files, vec!["two.js"]);
    let capture_stat: String = db
        .query_row(
            "SELECT capture_stat FROM revision_documents WHERE revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND path='two.js'",
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
            "SELECT payload FROM revision_capture_inputs WHERE revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND input_key='config:package.json'",
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
            "SELECT count(*) FROM revision_capture_inputs WHERE revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND payload='{\"state\":\"absent\"}'",
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

// Frozen v4 objects match Store's exact closed-world legacy recognition.
// This is a physical old-format fixture, not a marker downgrade of a v8 DB.
const FROZEN_LEGACY4_GRAPH_SQL: &str = "
CREATE TABLE index_metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1), schema_version INTEGER NOT NULL, extractor_version TEXT NOT NULL, root_spelling TEXT NOT NULL, root_device TEXT NOT NULL, root_inode TEXT NOT NULL, index_generation TEXT NOT NULL, index_revision INTEGER NOT NULL CHECK(index_revision BETWEEN 0 AND 9007199254740991), last_opened_at INTEGER NOT NULL CHECK(last_opened_at BETWEEN 0 AND 9007199254740991), indexed_at TEXT NOT NULL, stats TEXT NOT NULL, diagnostics TEXT NOT NULL);
CREATE TABLE files(path TEXT PRIMARY KEY, hash TEXT NOT NULL, payload TEXT NOT NULL);
CREATE TABLE nodes(id TEXT PRIMARY KEY, name TEXT NOT NULL, path TEXT NOT NULL REFERENCES files(path) DEFERRABLE INITIALLY DEFERRED, payload TEXT NOT NULL);
CREATE INDEX nodes_name ON nodes(name);
CREATE TABLE calls(id TEXT PRIMARY KEY, caller TEXT NOT NULL REFERENCES nodes(id) DEFERRABLE INITIALLY DEFERRED, target TEXT, path TEXT NOT NULL REFERENCES files(path) DEFERRABLE INITIALLY DEFERRED, payload TEXT NOT NULL);
CREATE INDEX calls_caller ON calls(caller);
CREATE TABLE regions(id TEXT PRIMARY KEY, owner TEXT NOT NULL REFERENCES nodes(id) DEFERRABLE INITIALLY DEFERRED, path TEXT NOT NULL REFERENCES files(path) DEFERRABLE INITIALLY DEFERRED, payload TEXT NOT NULL);
";
const FROZEN_LEGACY4_CLASS_SQL: &str = "
CREATE TABLE class_catalog(singleton INTEGER PRIMARY KEY CHECK(singleton=1), warnings TEXT NOT NULL, truncated INTEGER NOT NULL);
CREATE TABLE classes(id TEXT PRIMARY KEY REFERENCES nodes(id) DEFERRABLE INITIALLY DEFERRED, name TEXT NOT NULL, qualified_name TEXT NOT NULL, path TEXT NOT NULL, payload TEXT NOT NULL);
CREATE INDEX classes_path ON classes(path,id);
CREATE TABLE class_relations(id TEXT PRIMARY KEY, owner TEXT NOT NULL REFERENCES classes(id) DEFERRABLE INITIALLY DEFERRED, target TEXT REFERENCES classes(id) DEFERRABLE INITIALLY DEFERRED, payload TEXT NOT NULL);
CREATE INDEX class_relations_owner ON class_relations(owner,id);
CREATE INDEX class_relations_target ON class_relations(target,id);
";
fn rewrite_as_physical_v4(db: &rusqlite::Connection) {
    let metadata: (String,String,String,String,i64,i64,String,String,String) = db.query_row(
        "SELECT root_spelling,root_device,root_inode,index_generation,index_revision,last_opened_at,indexed_at,stats,diagnostics FROM index_metadata",
        [],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?)),
    ).unwrap();
    db.pragma_update(None, "foreign_keys", "OFF").unwrap();
    let names: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for name in names {
        db.execute_batch(&format!("DROP TABLE \"{}\"", name.replace('"', "\"\"")))
            .unwrap();
    }
    db.execute_batch(FROZEN_LEGACY4_GRAPH_SQL).unwrap();
    db.execute_batch(FROZEN_LEGACY4_CLASS_SQL).unwrap();
    db.execute(
        "INSERT INTO index_metadata VALUES(1,4,'native-v1',?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        rusqlite::params![
            metadata.0, metadata.1, metadata.2, metadata.3, metadata.4, metadata.5, metadata.6,
            metadata.7, metadata.8
        ],
    )
    .unwrap();
    db.execute("INSERT INTO class_catalog VALUES(1,'[]',0)", [])
        .unwrap();
    db.pragma_update(None, "user_version", 4).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='revision_documents'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn physical_schema_four_rebuild_is_same_file_with_fresh_generation_and_revision_one() {
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
    let job = IndexJobCoordinator::prepare(&store, None).unwrap();
    let session = job.session();
    let current = job.run(&options, &cancel, |_| {}).unwrap();
    let path = index_dir(state.path()).join("index.db");
    let inode = fs::metadata(&path).unwrap().ino();
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    rewrite_as_physical_v4(&db);
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
    let rebuilt = IndexJobCoordinator::prepare_with_session(&store, Some(current), session.clone())
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();
    assert_ne!(rebuilt.index_generation, current.index_generation);
    assert_eq!(rebuilt.index_revision, 1);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(store.status().unwrap().revision, rebuilt);
}

fn sqlite_snapshot(
    path: &Path,
    pin: baleyg::model::IndexPin,
    normalize_current_publication: bool,
) -> Vec<(String, Vec<Vec<String>>)> {
    use rusqlite::types::Value;
    let db = rusqlite::Connection::open(path).unwrap();
    let (generation, published_revision, incarnation): (String, i64, String) = db
        .query_row(
            "SELECT index_generation,index_revision,reconciled_incarnation FROM index_metadata",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert!(uuid::Uuid::parse_str(&generation).is_ok());
    assert!(uuid::Uuid::parse_str(&incarnation).is_ok());
    assert_eq!(pin.index_generation.to_string(), generation);
    if normalize_current_publication {
        assert_eq!(pin.index_revision as i64, published_revision);
    }
    let revision_id = format!("pin:v1:{}:{}", pin.index_generation, pin.index_revision);
    let (header_incarnation, header_revision): (String, i64) = db
        .query_row(
            "SELECT reconciled_incarnation,published_index_revision FROM native_revisions WHERE id=?1",
            [&revision_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(header_revision, pin.index_revision as i64);
    if normalize_current_publication {
        assert_eq!(header_incarnation, incarnation);
    }
    let violations: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0, "all retained revision FKs must remain valid");
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name!='index_metadata' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(tables.len(), 28, "compare all v8 evidence tables");
    tables
        .into_iter()
        .map(|table| {
            // Select the complete revision projection, not all archived rows.
            // Every table remains in the independent cold-source comparison.
            let (where_clause, alias) = match table.as_str() {
                "native_revisions" => ("id=?1", ""),
                "revision_capture_inputs" | "revision_documents" => ("revision_id=?1", ""),
                "native_source_sets" => ("id=(SELECT source_set_id FROM native_revisions WHERE id=?1)", ""),
                "native_source_set_languages" | "native_source_set_dependencies" =>
                    ("source_set_id=(SELECT source_set_id FROM native_revisions WHERE id=?1)", ""),
                "native_producers" | "native_producer_languages" | "native_producer_inputs" =>
                    ("EXISTS(SELECT 1 FROM document_versions v JOIN revision_documents d ON d.document_version_id=v.id WHERE d.revision_id=?1 AND v.producer_id=selected.producer_id AND v.producer_version=selected.producer_version)", "producer"),
                "document_versions" => ("id IN (SELECT document_version_id FROM revision_documents WHERE revision_id=?1)", ""),
                "graph_projections" => ("id IN (SELECT graph_projection_id FROM revision_documents WHERE revision_id=?1)", ""),
                "graph_nodes" | "graph_calls" | "graph_regions" =>
                    ("projection_id IN (SELECT graph_projection_id FROM revision_documents WHERE revision_id=?1)", ""),
                "class_projections" => ("id IN (SELECT class_projection_id FROM revision_documents WHERE revision_id=?1)", ""),
                "classes" | "class_relations" =>
                    ("projection_id IN (SELECT class_projection_id FROM revision_documents WHERE revision_id=?1)", ""),
                name if name.starts_with("native_version_") =>
                    ("version_id IN (SELECT document_version_id FROM revision_documents WHERE revision_id=?1)", ""),
                _ => panic!("unhandled v8 evidence table: {table}"),
            };
            // Producer descriptor columns differ: only the parent table uses id/version.
            let where_clause = if table == "native_producers" {
                "EXISTS(SELECT 1 FROM document_versions v JOIN revision_documents d ON d.document_version_id=v.id WHERE d.revision_id=?1 AND v.producer_id=selected.id AND v.producer_version=selected.version)"
            } else {
                where_clause
            };
            let alias = if alias.is_empty() { "" } else { " selected" };
            let sql = format!("SELECT * FROM \"{table}\"{alias} WHERE {where_clause}");
            let mut statement = db.prepare(&sql).unwrap();
            let columns = statement.column_count();
            let mut rows = statement
                .query_map([&revision_id], |row| {
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
            if table == "native_revisions" {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0][0], format!("t:{revision_id}"));
                assert_eq!(rows[0][8], format!("t:{header_incarnation}"));
                assert_eq!(rows[0][12], format!("i:{}", pin.index_revision));
                if normalize_current_publication {
                    rows[0][0] = "t:<current-pin>".into();
                    rows[0][8] = "t:<leader-incarnation>".into();
                    rows[0][12] = "i:<current-revision>".into();
                }
            } else if matches!(table.as_str(), "revision_capture_inputs" | "revision_documents") {
                for row in &mut rows {
                    assert_eq!(row[0], format!("t:{revision_id}"));
                    if normalize_current_publication {
                        row[0] = "t:<current-pin>".into();
                    }
                }
            }
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
        (
            "Witness.java",
            "class Witness { void retained() { measured(); } }\n",
        ),
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
    let first_job = IndexJobCoordinator::prepare(&store, None).unwrap();
    let leader_session = first_job.session();
    let first = first_job.run(&options, &cancel, |_| {}).unwrap();
    let reconciled_path = index_dir(state.path()).join("index.db");
    let retained_first = sqlite_snapshot(&reconciled_path, first, false);
    for table in [
        "document_versions",
        "graph_projections",
        "class_projections",
        "native_version_declarations",
        "graph_nodes",
        "classes",
    ] {
        assert!(
            !retained_first
                .iter()
                .find(|(name, _)| name == table)
                .unwrap()
                .1
                .is_empty(),
            "{table} fixture must be nonempty"
        );
    }

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
    let second =
        IndexJobCoordinator::prepare_with_session(&store, Some(first), leader_session.clone())
            .unwrap()
            .run(&options, &cancel, |_| {})
            .unwrap();
    assert_eq!(second.index_generation, first.index_generation);
    assert_eq!(second.index_revision, first.index_revision + 1);
    assert_eq!(store.status().unwrap().revision, second);
    assert_eq!(
        sqlite_snapshot(&reconciled_path, first, false),
        retained_first,
        "r1 full evidence must remain immutable after r2"
    );
    let r2 = sqlite_snapshot(&reconciled_path, second, true);
    let paths = |snapshot: &Vec<(String, Vec<Vec<String>>)>| -> std::collections::BTreeSet<String> {
        snapshot
            .iter()
            .find(|(table, _)| table == "revision_documents")
            .unwrap()
            .1
            .iter()
            .map(|row| row[3].clone())
            .collect()
    };
    let first_paths = paths(&retained_first);
    let second_paths = paths(&r2);
    assert!(first_paths.contains("t:delete.js") && first_paths.contains("t:rename-old.js"));
    assert!(
        second_paths.contains("t:added.rs")
            && second_paths.contains("t:rename-new.js")
            && second_paths.contains("t:ignored.js")
            && !second_paths.contains("t:delete.js")
            && !second_paths.contains("t:rename-old.js")
    );
    let db = rusqlite::Connection::open(&reconciled_path).unwrap();
    let headers: i64 = db
        .query_row("SELECT count(*) FROM native_revisions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(headers, 2, "r1 and r2 headers survive");
    for pin in [first, second] {
        let id = format!("pin:v1:{}:{}", pin.index_generation, pin.index_revision);
        for table in ["revision_documents", "revision_capture_inputs"] {
            let count: i64 = db
                .query_row(
                    &format!("SELECT count(*) FROM {table} WHERE revision_id=?1"),
                    [&id],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(count > 0, "{table}: complete {id} evidence must remain");
        }
    }
    let stale = store.source_at("keep.js", Some(first)).unwrap_err();
    assert!(stale.to_string().contains("revision conflict"), "{stale:#}");
    drop(db);

    let fresh_state = tempfile::tempdir().unwrap();
    fs::set_permissions(fresh_state.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let fresh = Store::open_for_tests(fresh_state.path(), workspace.path()).unwrap();
    let cold_pin = IndexJobCoordinator::prepare(&fresh, None)
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();
    let fresh_path = index_dir(fresh_state.path()).join("index.db");
    assert_eq!(
        r2,
        sqlite_snapshot(&fresh_path, cold_pin, true),
        "current r2 must equal a full independent cold build, including every selected v8 table"
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
    let job = IndexJobCoordinator::prepare(&store, None).unwrap();
    let session = job.session();
    let first = job.run(&options, &cancel, |_| {}).unwrap();
    let path = index_dir(state.path()).join("index.db");
    let read_stat = || {
        let db = rusqlite::Connection::open(&path).unwrap();
        let payload: String = db
            .query_row(
                "SELECT capture_stat FROM revision_documents WHERE revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND path='same.js'",
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
    let second = IndexJobCoordinator::prepare_with_session(&store, Some(first), session.clone())
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
    let job = IndexJobCoordinator::prepare(&store, None).unwrap();
    let session = job.session();
    let first = job.run(&options, &cancel, |_| {}).unwrap();
    let path = index_dir(state.path()).join("index.db");
    let inode = fs::metadata(&path).unwrap().ino();
    drop(store);

    let db = rusqlite::Connection::open(&path).unwrap();
    assert!(
        db.execute(
            "UPDATE index_metadata SET extractor_version='obsolete-extractor'",
            []
        )
        .is_err(),
        "v8 metadata must physically forbid unsupported extractor markers"
    );
    db.execute("UPDATE index_metadata SET stats='not-json'", [])
        .unwrap();
    drop(db);
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    assert!(
        store
            .status()
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );
    let rebuilt = IndexJobCoordinator::prepare_with_session(&store, Some(first), session.clone())
        .unwrap()
        .run(&options, &cancel, |_| {})
        .unwrap();
    assert_eq!(rebuilt.index_revision, 1);
    assert_ne!(rebuilt.index_generation, first.index_generation);
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    drop(store);

    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE document_versions SET source_bytes=x'FF',byte_length=1 WHERE id=(SELECT document_version_id FROM revision_documents WHERE path='one.js')",
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
    let failure = IndexJobCoordinator::prepare_with_session(&store, Some(rebuilt), session.clone())
        .unwrap()
        .run(&options, &failed_cancel, |_| {})
        .unwrap_err();
    assert!(failure.to_string().contains("cancelled"));
    let db = rusqlite::Connection::open(&path).unwrap();
    let malformed: String = db
        .query_row("SELECT hex(source_bytes) FROM document_versions WHERE id=(SELECT document_version_id FROM revision_documents WHERE path='one.js')", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(malformed, "FF");
    drop(db);
    assert!(store.status().is_err());
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);
    failed_cancel.store(false, Ordering::Release);
    let mut recovered =
        IndexJobCoordinator::prepare_with_session(&store, Some(rebuilt), session.clone())
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
            "SELECT d.syntax_id,d.lookup_key FROM native_version_declarations d JOIN document_versions v ON v.id=d.version_id WHERE d.lookup_key IS NOT NULL AND v.language='javascript' ORDER BY d.syntax_id LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        db.execute(
            "UPDATE native_version_declarations SET key_ordinal=CAST(key_ordinal + 0.5 AS REAL) WHERE syntax_id=?1",
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
    let failure =
        IndexJobCoordinator::prepare_with_session(&store, Some(recovered), session.clone())
            .unwrap()
            .run(&options, &failed_cancel, |_| {})
            .unwrap_err();
    assert!(failure.to_string().contains("cancelled"), "{failure:#}");
    let db = rusqlite::Connection::open(&path).unwrap();
    let stored_type: String = db
        .query_row(
            "SELECT typeof(key_ordinal) FROM native_version_declarations WHERE syntax_id=?1",
            [&syntax_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_type, "real");
    drop(db);
    assert!(store.status().is_err());
    failed_cancel.store(false, Ordering::Release);
    let typed_recovered =
        IndexJobCoordinator::prepare_with_session(&store, Some(recovered), session.clone())
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
    drop(session);
    drop(store.leader().unwrap());
    assert!(store.status().is_err());
    assert!(clone.status().is_err());
    // A separately opened Store performs its own full recovery admission.
    let separate = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    assert_eq!(separate.index_baseline().unwrap(), recovered);
    drop(separate);
    let job = IndexJobCoordinator::prepare(&clone, Some(recovered)).unwrap();
    let _next_session = job.session();
    let next = job.run(&options, &failed_cancel, |_| {}).unwrap();
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
    let job = IndexJobCoordinator::prepare(&store, None).unwrap();
    let session = job.session();
    let pin = job.run(&options, &cancel, |_| {}).unwrap();
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
            .contains("incompatible_index")
    );
    cancel.store(true, Ordering::Release);
    let failure = IndexJobCoordinator::prepare_with_session(&store, Some(pin), session.clone())
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
    let repaired = IndexJobCoordinator::prepare_with_session(&store, Some(pin), session.clone())
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
        "DELETE FROM revision_capture_inputs WHERE input_key='config:package.json'",
        "INSERT INTO revision_capture_inputs(revision_id,input_key,payload) SELECT id,'unknown:slot','{\"state\":\"absent\"}' FROM native_revisions WHERE published_index_revision=(SELECT index_revision FROM index_metadata)",
        "UPDATE index_metadata SET reconcile_options=json_set(reconcile_options,'$.version',2)",
        "UPDATE revision_capture_inputs SET payload=json_set(payload,'$.hash','invalid-digest') WHERE input_key='config:package.json'",
    ] {
        let (state, workspace) = fixture();
        fs::write(workspace.path().join("one.js"), "function one() {}\n").unwrap();
        fs::write(
            workspace.path().join("package.json"),
            b"{\"name\":\"capture-fixture\"}",
        )
        .unwrap();
        let scip = workspace.path().join("labels.scip");
        let manifest = workspace.path().join("labels.json");
        fs::write(&scip, b"captured presentation bytes").unwrap();
        fs::write(&manifest, b"{}").unwrap();
        let mut options = IndexOptions::new(workspace.path().to_owned());
        options.scip_path = Some(scip.clone());
        options.manifest_path = Some(manifest.clone());
        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
        let job = IndexJobCoordinator::prepare(&store, None).unwrap();
        let session = job.session();
        let pin = job.run(&options, &cancel, |_| {}).unwrap();
        let path = index_dir(state.path()).join("index.db");
        drop(store);
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(mutation).unwrap();
        assert!(
            db.changes() == 1,
            "inventory mutation must touch exactly one row: {mutation}"
        );
        drop(db);
        let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
        assert!(
            store.status().is_err(),
            "invalid inventory was publicly readable: {mutation}"
        );
        let repaired =
            IndexJobCoordinator::prepare_with_session(&store, Some(pin), session.clone())
                .unwrap()
                .run(&options, &cancel, |_| {})
                .unwrap();
        assert_eq!(repaired.index_revision, 1);
        assert_ne!(repaired.index_generation, pin.index_generation);
        assert_eq!(store.status().unwrap().revision, repaired);
        let db = rusqlite::Connection::open(&path).unwrap();
        let keys = db
            .prepare("SELECT input_key FROM revision_capture_inputs ORDER BY input_key")
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
    let (state, _workspace, store, pin, _session) = projection_fixture();
    let clone = store.clone();
    let db = rusqlite::Connection::open(index_dir(state.path()).join("index.db")).unwrap();
    db.execute(
        "UPDATE document_versions SET source_bytes=x'FF',byte_length=1 WHERE id=(SELECT document_version_id FROM revision_documents WHERE path='flow.js')",
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
            .contains("incompatible_index")
    );

    // A matching invalid node is selected by the page even when file JSON is valid.
    let (state, _workspace, store, pin, _session) = projection_fixture();
    let clone = store.clone();
    let db = rusqlite::Connection::open(index_dir(state.path()).join("index.db")).unwrap();
    db.execute(
        "UPDATE graph_nodes SET payload='not-json' WHERE path='flow.js'",
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
            .contains("incompatible_index")
    );

    // The methods route types both invalid JSON and valid JSON with bad Symbol shape.
    for payload in ["not-json", r#"{"kind":"function"}"#] {
        let (state, _workspace, store, pin, _session) = projection_fixture();
        let clone = store.clone();
        let db = rusqlite::Connection::open(index_dir(state.path()).join("index.db")).unwrap();
        db.execute(
            "UPDATE graph_nodes SET payload=?1 WHERE path='flow.js'",
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
                .contains("incompatible_index")
        );
    }

    // The selected source's bytes remain the same, but storing them as TEXT
    // instead of BLOB must produce a typed conversion refusal. No FK or CHECK
    // is disabled: the source hash and declared byte length still match.
    let (state, _workspace, store, pin, _session) = projection_fixture();
    let clone = store.clone();
    let db = rusqlite::Connection::open(index_dir(state.path()).join("index.db")).unwrap();
    assert_eq!(
        db.execute(
            "UPDATE document_versions SET source_bytes=CAST(source_bytes AS TEXT) WHERE id=(SELECT document_version_id FROM revision_documents WHERE path='flow.js')",
            [],
        )
        .unwrap(),
        1
    );
    let storage_class: String = db.query_row(
        "SELECT typeof(source_bytes) FROM document_versions WHERE id=(SELECT document_version_id FROM revision_documents WHERE path='flow.js')",
        [],
        |row| row.get(0),
    ).unwrap();
    assert_eq!(storage_class, "text");
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
            .contains("incompatible_index")
    );

    // Tree enrichment evaluates only its visible file and latches its invalid node.
    let (state, workspace, store, _pin, _session) = projection_fixture();
    let clone = store.clone();
    let db = rusqlite::Connection::open(index_dir(state.path()).join("index.db")).unwrap();
    db.execute(
        "UPDATE graph_nodes SET payload='not-json' WHERE path='flow.js'",
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
            .contains("incompatible_index")
    );

    // Benign absence and a bad pin remain non-latching request outcomes.
    let (_state, _workspace, store, pin, _session) = projection_fixture();
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

    let (state, workspace, store, pin, session) = projection_fixture();
    let clone = store.clone();
    let path = index_dir(state.path()).join("index.db");
    let inode = fs::metadata(&path).unwrap().ino();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE document_versions SET source_bytes=x'FF',byte_length=1 WHERE id=(SELECT document_version_id FROM revision_documents WHERE path='flow.js')",
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
            .contains("incompatible_index")
    );

    let cancel: CancelFlag = Arc::new(AtomicBool::new(true));
    let failed = IndexJobCoordinator::prepare_with_session(&store, Some(pin), session.clone())
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
            .contains("incompatible_index")
    );
    assert!(
        clone
            .status()
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );
    assert_eq!(fs::metadata(&path).unwrap().ino(), inode);

    cancel.store(false, Ordering::Release);
    let recovered = IndexJobCoordinator::prepare_with_session(&store, Some(pin), session.clone())
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

    let (state, workspace, initial, old, session) = projection_fixture();
    let path = index_dir(state.path()).join("index.db");
    let inode = fs::metadata(&path).unwrap().ino();
    drop(initial);
    let db = rusqlite::Connection::open(&path).unwrap();
    // Simulate an out-of-band damaged v8 metadata marker without pretending
    // this is a physical legacy index. The ordinary SQL CHECK rejects it.
    db.pragma_update(None, "ignore_check_constraints", true)
        .unwrap();
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
            .contains("incompatible_index")
    );
    assert!(
        clone
            .status()
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );
    assert_eq!(store.index_baseline().unwrap(), old);

    let stale =
        IndexJobCoordinator::prepare_with_session(&store, Some(old), session.clone()).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.pragma_update(None, "ignore_check_constraints", true)
        .unwrap();
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
        "UPDATE index_metadata SET extractor_version='native-v4-class-compose-v1'",
        [],
    )
    .unwrap();
    drop(db);

    let cancel: CancelFlag = Arc::new(AtomicBool::new(true));
    let error = IndexJobCoordinator::prepare_with_session(&store, Some(old), session.clone())
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
            .contains("incompatible_index")
    );

    cancel.store(false, Ordering::Release);
    let recovered = IndexJobCoordinator::prepare_with_session(&store, Some(old), session.clone())
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
        IndexJobCoordinator::prepare_with_session(&store, Some(old), session.clone())
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

    let (state, workspace, initial, old, session) = projection_fixture();
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
            .contains("incompatible_index")
    );
    assert!(
        store
            .index_baseline()
            .unwrap_err()
            .to_string()
            .contains("not decodable")
    );
    assert!(
        IndexJobCoordinator::prepare_with_session(&store, Some(old), session.clone())
            .err()
            .expect("undecodable pin must conflict")
            .to_string()
            .contains("revision conflict")
    );

    let cancel: CancelFlag = Arc::new(AtomicBool::new(true));
    let error = IndexJobCoordinator::prepare_with_session(&store, None, session.clone())
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
            .contains("incompatible_index")
    );

    cancel.store(false, Ordering::Release);
    let recovered = IndexJobCoordinator::prepare_with_session(&store, None, session.clone())
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
        let (state, workspace, initial, _old, session) = projection_fixture();
        let path = index_dir(state.path()).join("index.db");
        drop(initial);
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute(first, []).unwrap();
        drop(db);
        let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
        let coordinator =
            IndexJobCoordinator::prepare_with_session(&store, None, session.clone()).unwrap();
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
                .contains("incompatible_index")
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

    let (state, workspace, initial, _old, session) = projection_fixture();
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
    let coordinator =
        IndexJobCoordinator::prepare_with_session(&store, None, session.clone()).unwrap();
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
            .contains("incompatible_index")
    );
    let inode = fs::metadata(&path).unwrap().ino();
    let cancel = Arc::new(AtomicBool::new(true));
    let error = IndexJobCoordinator::prepare_with_session(&store, None, session.clone())
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
    let recovered = IndexJobCoordinator::prepare_with_session(&store, None, session.clone())
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

    let (state, workspace, initial, _old, _session) = projection_fixture();
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

    let (state, workspace, initial, _old, _session) = projection_fixture();
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

    let (state, workspace, initial, _old, _session) = projection_fixture();
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
    let (state, _workspace, store, pin, _session) = projection_fixture();
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
            .contains("incompatible_index")
    );
    assert!(
        store
            .views()
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );
    assert!(
        store
            .view("missing")
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );
    assert!(
        store
            .annotations()
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );

    // REAL revision must fail before pin comparison and close every clone.
    let (state, _workspace, store, pin, _session) = projection_fixture();
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
            .contains("incompatible_index")
    );

    // Malformed persisted input is reached by the existing full-read inventory pass.
    let (state, _workspace, store, pin, _session) = projection_fixture();
    let clone = store.clone();
    let path = index_dir(state.path()).join("index.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE revision_capture_inputs SET payload='not-json' WHERE input_key='root:.'",
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
            .contains("incompatible_index")
    );
}

#[test]
fn live_structural_inventory_and_control_first_reads_fail_closed() {
    // This is an already executed structural check, not a new derived invariant.
    let (state, _workspace, store, pin, _session) = projection_fixture();
    let clone = store.clone();
    let path = index_dir(state.path()).join("index.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE revision_capture_inputs SET input_key='unexpected:role' WHERE input_key='root:.'",
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
            .contains("incompatible_index")
    );
    assert!(
        clone
            .view("missing")
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );
    assert!(
        clone
            .annotations()
            .unwrap_err()
            .to_string()
            .contains("incompatible_index")
    );

    // A control-only read can also be the first observer of typed control corruption.
    let (state, _workspace, store, _pin, _session) = projection_fixture();
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
            .contains("incompatible_index")
    );
}

#[test]
fn live_root_mismatch_precedes_typed_control_corruption_without_latching() {
    let (state, _workspace, store, pin, _session) = projection_fixture();
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

    let (state, workspace, store, old, session) = projection_fixture();
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
    let error = IndexJobCoordinator::prepare_with_session(&store, Some(old), session.clone())
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
            .contains("incompatible_index")
    );

    cancel.store(false, Ordering::Release);
    let recovered = IndexJobCoordinator::prepare_with_session(&store, Some(old), session.clone())
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
        IndexJobCoordinator::prepare_with_session(&store, Some(old), session.clone())
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
        "UPDATE revision_capture_inputs SET payload='not-json' WHERE input_key='root:.'",
    ] {
        let (state, _workspace, store, pin, _session) = projection_fixture();
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
                .contains("incompatible_index"),
            "{sql}"
        );
    }
}

#[test]
fn captured_java_python_scip_labels_require_unique_measured_names_and_coordinates() {
    use baleyg::{
        indexer::{IndexOptions, index_workspace_bundle},
        model::{CancelFlag, SymbolKind},
    };
    use protobuf::Message;
    use sha2::{Digest, Sha256};
    use std::sync::{Arc, atomic::AtomicBool};
    let (state, workspace) = fixture();
    let java = "/*é*/ class A {}\n";
    let python = "class P: pass\n";
    fs::write(workspace.path().join("A.java"), java).unwrap();
    fs::write(workspace.path().join("a.py"), python).unwrap();
    fs::write(workspace.path().join("a.js"), "class J {}\n").unwrap();
    fs::write(workspace.path().join("a.rs"), "struct R {}\n").unwrap();
    let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    let scip = state.path().join("labels.scip");
    let manifest = state.path().join("labels.json");
    let mut options = IndexOptions::new(workspace.path().to_owned());
    options.scip_path = Some(scip.clone());
    options.manifest_path = Some(manifest.clone());
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let occurrences = |version: &str| {
        let mut index = scip::types::Index::new();
        for (path, range, name) in [
            ("A.java", vec![0, 12, 13], "A"), // default UTF-16; UTF-8 byte columns are 13..14.
            ("a.py", vec![0, 6, 7], "P"),
            ("a.js", vec![0, 6, 7], "J"),
            ("a.rs", vec![0, 7, 8], "R"),
        ] {
            let mut doc = scip::types::Document::new();
            doc.relative_path = path.into();
            let mut occurrence = scip::types::Occurrence::new();
            occurrence.range = range;
            occurrence.symbol_roles = 1;
            occurrence.symbol = format!("scip {version} {name}");
            doc.occurrences.push(occurrence);
            index.documents.push(doc);
        }
        index
    };
    let correct_hashes = serde_json::json!({
        "A.java":hex::encode(Sha256::digest(java.as_bytes())),
        "a.py":hex::encode(Sha256::digest(python.as_bytes())),
        "a.js":hex::encode(Sha256::digest(b"class J {}\n")),
        "a.rs":hex::encode(Sha256::digest(b"struct R {}\n")),
    });
    let run = |index: &scip::types::Index, hashes: &serde_json::Value| {
        fs::write(&scip, index.write_to_bytes().unwrap()).unwrap();
        fs::write(&manifest, serde_json::to_vec(hashes).unwrap()).unwrap();
        index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap()
    };
    let valid = occurrences("valid");
    let (baseline, base_native, _) = run(&valid, &correct_hashes);
    let node = |graph: &baleyg::model::Graph, path: &str, name: &str| {
        graph
            .nodes
            .iter()
            .find(|n| n.path == path && n.name == name && n.kind == SymbolKind::Class)
            .unwrap()
            .clone()
    };
    for (path, name) in [("A.java", "A"), ("a.py", "P"), ("a.js", "J")] {
        assert_eq!(
            node(&baseline, path, name).display_label.as_deref(),
            Some(format!("scip valid {name}").as_str())
        );
    }
    assert!(
        baseline
            .nodes
            .iter()
            .filter(|n| n.path == "a.rs")
            .all(|n| n.display_label.is_none())
    );
    let baseline_ids: Vec<_> = baseline
        .nodes
        .iter()
        .map(|n| (&n.id, &n.provenance))
        .collect();
    for scenario in [
        "stale-manifest",
        "missing-manifest",
        "wrong-range",
        "wrong-name",
        "reference-role",
        "duplicate",
        "ambiguous-range",
        "unicode-byte-column",
    ] {
        let mut index = valid.clone();
        let mut hashes = correct_hashes.clone();
        let java_doc = index
            .documents
            .iter_mut()
            .find(|d| d.relative_path == "A.java")
            .unwrap();
        match scenario {
            "stale-manifest" => hashes["A.java"] = serde_json::json!("deadbeef"),
            "missing-manifest" => {
                hashes.as_object_mut().unwrap().remove("A.java");
            }
            "wrong-range" => java_doc.occurrences[0].range = vec![0, 5, 6],
            "wrong-name" => java_doc.occurrences[0].symbol = "scip wrong Q".into(),
            "reference-role" => java_doc.occurrences[0].symbol_roles = 0,
            "duplicate" => java_doc.occurrences.push(java_doc.occurrences[0].clone()),
            "ambiguous-range" => {
                // A second, same-coordinate presentation cannot choose a label.
                let mut other = java_doc.occurrences[0].clone();
                other.symbol = "scip second A".into();
                java_doc.occurrences.push(other);
            }
            "unicode-byte-column" => java_doc.occurrences[0].range = vec![0, 13, 14],
            _ => unreachable!(),
        }
        let (graph, native, _) = run(&index, &hashes);
        assert_eq!(
            node(&graph, "A.java", "A").display_label,
            None,
            "{scenario}"
        );
        assert_eq!(
            node(&graph, "a.py", "P").display_label,
            node(&baseline, "a.py", "P").display_label
        );
        assert_eq!(
            node(&graph, "a.js", "J").display_label,
            node(&baseline, "a.js", "J").display_label
        );
        assert!(
            graph
                .nodes
                .iter()
                .filter(|n| n.path == "a.rs")
                .all(|n| n.display_label.is_none())
        );
        assert_eq!(
            graph
                .nodes
                .iter()
                .map(|n| (&n.id, &n.provenance))
                .collect::<Vec<_>>(),
            baseline_ids,
            "native IDs and provenance must not change with SCIP presentation: {scenario}"
        );
        assert_eq!(native.producer, base_native.producer);
        assert_eq!(
            native
                .declarations
                .iter()
                .map(|d| &d.syntax_id)
                .collect::<Vec<_>>(),
            base_native
                .declarations
                .iter()
                .map(|d| &d.syntax_id)
                .collect::<Vec<_>>()
        );
    }
}
