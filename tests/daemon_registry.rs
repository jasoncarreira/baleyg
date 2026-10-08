use baleyg::daemon::registry::{
    CheckoutOptions, CheckoutRegistry, MAX_ACTIVE_CHECKOUTS, SelectionError,
};
use baleyg::store::topology::WorkspaceIdentity;
use std::{fs, os::unix::fs::symlink, path::Path, process::Command};

fn git(args: &[&str], dir: &Path) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );
}
fn identity(root: &Path) -> WorkspaceIdentity {
    WorkspaceIdentity::discover(Some(root), root)
        .unwrap()
        .attach_marker()
        .unwrap()
}

#[test]
fn selected_worktree_is_attributed_and_retained_until_disconnect() {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    let launch = base.join("launch");
    let selected = base.join("selected");
    fs::create_dir(&launch).unwrap();
    git(&["init", "-q"], &launch);
    git(
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.org",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ],
        &launch,
    );
    git(
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "selected",
            selected.to_str().unwrap(),
        ],
        &launch,
    );
    let launch_id = identity(&launch);
    let mut registry = CheckoutRegistry::new();
    registry.attach_launch(1, &launch_id).unwrap();
    fs::create_dir(selected.join("nested")).unwrap();
    let chosen = registry
        .select(1, &launch_id, &selected.join("nested"))
        .unwrap();
    assert_eq!(chosen.root, selected);
    assert_ne!(chosen.root_key, launch_id.root_key);
    assert_eq!(registry.active_count(), 2);
    registry.disconnect(1);
    assert_eq!(registry.active_count(), 0);
    assert_eq!(registry.known_roots().len(), 2);
}

#[test]
fn selection_does_not_follow_symlinks_or_admit_other_repositories() {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    let launch = base.join("launch");
    let other = base.join("other");
    fs::create_dir(&launch).unwrap();
    fs::create_dir(&other).unwrap();
    git(&["init", "-q"], &launch);
    git(&["init", "-q"], &other);
    let launch_id = identity(&launch);
    let mut registry = CheckoutRegistry::new();
    assert_eq!(
        registry
            .select(1, &launch_id, Path::new("other"))
            .unwrap_err(),
        SelectionError::NotAbsolute
    );
    assert_eq!(
        registry.select(1, &launch_id, &other).unwrap_err(),
        SelectionError::DifferentRepository
    );
    let alias = base.join("alias");
    symlink(&other, &alias).unwrap();
    assert_eq!(
        registry.select(1, &launch_id, &alias).unwrap_err(),
        SelectionError::NotCheckout
    );
    assert_eq!(registry.active_count(), 0);
}

#[test]
fn inert_registration_and_capacity() {
    let temp = tempfile::tempdir().unwrap();
    let mut registry = CheckoutRegistry::new();
    let base = fs::canonicalize(temp.path()).unwrap();
    let mut identities = Vec::new();
    for index in 0..=MAX_ACTIVE_CHECKOUTS {
        let root = base.join(format!("root-{index}"));
        fs::create_dir(&root).unwrap();
        identities.push(identity(&root));
    }
    let options = CheckoutOptions(serde_json::json!({"provider":"first"}));
    registry.register(&identities[0], options.clone()).unwrap();
    registry.register(&identities[0], options.clone()).unwrap();
    assert_eq!(registry.active_count(), 0);
    assert_eq!(
        registry.registration(&identities[0].root_key),
        Some(&options)
    );
    for (index, identity) in identities.iter().take(MAX_ACTIVE_CHECKOUTS).enumerate() {
        registry.attach_launch(index as u64, identity).unwrap();
    }
    assert_eq!(registry.active_count(), MAX_ACTIVE_CHECKOUTS);
    assert_eq!(
        registry
            .attach_launch(999, &identities[MAX_ACTIVE_CHECKOUTS])
            .unwrap_err(),
        SelectionError::CheckoutCapacity
    );
    assert!(SelectionError::CheckoutCapacity.retryable());
    assert_eq!(
        registry
            .register(
                &identities[0],
                CheckoutOptions(serde_json::json!({"provider":"other"}))
            )
            .unwrap_err(),
        SelectionError::RegistrationConflict
    );
    registry.disconnect(0);
    registry
        .attach_launch(999, &identities[MAX_ACTIVE_CHECKOUTS])
        .unwrap();
    assert_eq!(registry.active_count(), MAX_ACTIVE_CHECKOUTS);
    registry
        .register(
            &identities[0],
            CheckoutOptions(serde_json::json!({"provider":"other"})),
        )
        .unwrap();
}
