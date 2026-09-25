use baleyg::store::topology::TopologyRoots;
use std::{fs, path::Path};
use tempfile::TempDir;

pub fn fixture() -> (TempDir, TopologyRoots) {
    let dir = tempfile::tempdir().unwrap();
    let roots =
        TopologyRoots::isolated_for_tests(dir.path().join("cache"), dir.path().join("data"));
    (dir, roots)
}
pub fn private(path: &Path) {
    fs::create_dir(path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
}
