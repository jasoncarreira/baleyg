use baleyg::store::Store;
use std::fs;
use tempfile::TempDir;

#[cfg(unix)]
#[test]
fn state_and_databases_are_private_and_symlink_safe() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let state = temp.path().join("private-state");
    let store = Store::open(&state, &source).unwrap();
    assert_eq!(
        fs::metadata(&state).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for name in ["cache.db", "workspace.db"] {
        assert_eq!(
            fs::metadata(state.join(name)).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(store.status().unwrap().revision, 0);
    let public = temp.path().join("public");
    fs::create_dir(&public).unwrap();
    fs::set_permissions(&public, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Store::open(&public, &source).is_err());
    let alias = temp.path().join("alias");
    symlink(&state, &alias).unwrap();
    assert!(Store::open(&alias, &source).is_err());
    fs::remove_file(state.join("cache.db")).unwrap();
    let other = temp.path().join("not-a-cache");
    fs::write(&other, b"sensitive").unwrap();
    symlink(&other, state.join("cache.db")).unwrap();
    assert!(store.status().is_err());
    assert_eq!(fs::read(&other).unwrap(), b"sensitive");
}
