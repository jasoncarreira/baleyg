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
    assert_eq!(store.status().unwrap().revision.index_revision, 0);
    assert_eq!(staged_files(&dir), Vec::<std::path::PathBuf>::new());
    drop(store);
    let reopened = Store::open_for_tests(state.path(), workspace.path()).unwrap();
    assert_eq!(reopened.status().unwrap().revision.index_revision, 0);
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
    assert_eq!(store.status().unwrap().revision.index_revision, 0);
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
            .status()
            .unwrap()
            .revision
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
    assert_eq!(store.status().unwrap().revision.index_revision, 0);
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
