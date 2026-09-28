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
