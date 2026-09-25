mod common;
use baleyg::store::topology::{UseGuard, WorkspaceIdentity};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
};

fn root(base: &Path) -> std::path::PathBuf {
    let work = base.join("work");
    fs::create_dir(&work).unwrap();
    work
}

#[test]
fn discovery_matrix() {
    let (temp, _) = common::fixture();
    let work = root(temp.path());
    fs::create_dir(work.join("nested")).unwrap();
    let plain = WorkspaceIdentity::discover(None, &work.join("nested")).unwrap();
    assert_eq!(plain.root, fs::canonicalize(work.join("nested")).unwrap());
    assert!(plain.record_id.starts_with("path-"));
    common::private(&work.join(".git"));
    let git = WorkspaceIdentity::discover(None, &work.join("nested")).unwrap();
    assert_eq!(git.root, fs::canonicalize(&work).unwrap());
    assert_eq!(git.record_id.len(), 36);
    assert_ne!(git.record_id, plain.record_id);
    git.verify().unwrap();
    let other = WorkspaceIdentity::discover(Some(&work.join("nested")), &work).unwrap();
    assert_eq!(other.root, fs::canonicalize(work.join("nested")).unwrap());
    let old = fs::rename(&work, temp.path().join("moved"));
    old.unwrap();
    assert!(
        git.verify()
            .unwrap_err()
            .to_string()
            .contains("root_changed")
    );
}

#[test]
fn fixed_paths_and_unsafe_components() {
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let identity = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    assert_eq!(
        roots.index_db(&identity),
        roots
            .cache
            .join("indexes")
            .join(&identity.root_key)
            .join("index.db")
    );
    assert_eq!(
        roots.record_db(&identity),
        roots
            .data
            .join("workspaces")
            .join(&identity.record_id)
            .join("workspace.db")
    );
    roots.prepare_index(&identity).unwrap();
    use std::os::unix::fs::symlink;
    fs::remove_dir(roots.index_dir(&identity)).unwrap();
    symlink(&work, roots.index_dir(&identity)).unwrap();
    assert!(roots.index_use(&identity).is_err());
}

#[test]
fn git_marker_matrix() {
    let (temp, _) = common::fixture();
    let work = root(temp.path());
    common::private(&work.join(".git"));
    let identity = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    let marker = work.join(".git/baleyg/workspace-id");
    assert_eq!(fs::read(&marker).unwrap().len(), 36);
    assert_eq!(
        WorkspaceIdentity::discover(Some(&work), &work)
            .unwrap()
            .record_id,
        identity.record_id
    );
    fs::write(&marker, b"aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa").unwrap();
    assert!(
        identity
            .verify()
            .unwrap_err()
            .to_string()
            .contains("workspace_id_changed")
    );
    fs::write(&marker, b"invalid").unwrap();
    assert!(WorkspaceIdentity::discover(Some(&work), &work).is_err());
    let linked = temp.path().join("linked");
    fs::create_dir(&linked).unwrap();
    fs::write(linked.join(".git"), "gitdir: ../work/.git\n").unwrap();
    // A valid pointer selects the same Git directory, hence the marker must be valid.
    assert!(WorkspaceIdentity::discover(Some(&linked), &linked).is_err());
    fs::write(&marker, identity.record_id.as_bytes()).unwrap();
    let pointed = WorkspaceIdentity::discover(Some(&linked), &linked).unwrap();
    assert_eq!(pointed.record_id, identity.record_id);
    fs::write(linked.join(".git"), "gitdir: ../work/.git\nextra\n").unwrap();
    assert!(WorkspaceIdentity::discover(Some(&linked), &linked).is_err());
}

#[test]
fn marker_race_and_durability() {
    let (temp, _) = common::fixture();
    let work = root(temp.path());
    common::private(&work.join(".git"));
    let children: Vec<_> = (0..4)
        .map(|_| {
            Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("marker_child")
                .arg("--nocapture")
                .env("TOPOLOGY_MARKER_CHILD", &work)
                .stdout(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut ids = vec![];
    for child in children {
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
        let output = String::from_utf8_lossy(&result.stdout);
        ids.push(
            output
                .lines()
                .find_map(|line| line.strip_prefix("MARKER_ID="))
                .unwrap()
                .to_owned(),
        );
    }
    assert!(ids.iter().all(|id| id == &ids[0]));
}
#[test]
fn marker_child() {
    if let Some(path) = std::env::var_os("TOPOLOGY_MARKER_CHILD") {
        let id = WorkspaceIdentity::discover(Some(Path::new(&path)), Path::new(&path)).unwrap();
        println!("MARKER_ID={}", id.record_id);
    }
}

#[test]
fn paused_creator_and_adopter_share_the_single_marker() {
    use baleyg::store::topology::MarkerStage;
    use std::sync::mpsc;
    for initial in [b"".as_slice(), b"partial".as_slice()] {
        let (temp, _) = common::fixture();
        let work = root(temp.path());
        common::private(&work.join(".git"));
        let marker = work.join(".git/baleyg/workspace-id");
        let (created_tx, created_rx) = mpsc::channel();
        let (write_tx, write_rx) = mpsc::channel();
        let (short_tx, short_rx) = mpsc::channel();
        let (retry_tx, retry_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let work_ref = &work;
            let creator = scope.spawn(move || {
                WorkspaceIdentity::discover_with_marker_hook(Some(work_ref), work_ref, |stage| {
                    if stage == MarkerStage::CreatedBeforeWrite {
                        created_tx.send(()).unwrap();
                        write_rx.recv().unwrap();
                    }
                    Ok(())
                })
                .unwrap()
            });
            created_rx.recv().unwrap();
            assert_eq!(fs::read(&marker).unwrap(), b"");
            fs::write(&marker, initial).unwrap();
            let adopter = scope.spawn(move || {
                WorkspaceIdentity::discover_with_marker_hook(Some(work_ref), work_ref, |stage| {
                    if stage == MarkerStage::ShortRead {
                        short_tx.send(()).unwrap();
                        retry_rx.recv().unwrap();
                    }
                    Ok(())
                })
                .unwrap()
            });
            short_rx.recv().unwrap();
            write_tx.send(()).unwrap();
            let winner = creator.join().unwrap();
            assert_eq!(fs::read(&marker).unwrap().len(), 36);
            retry_tx.send(()).unwrap();
            let loser = adopter.join().unwrap();
            assert_eq!(winner.record_id, loser.record_id);
            winner.verify().unwrap();
            loser.verify().unwrap();
        });
    }
}

#[test]
fn every_marker_sync_stage_is_required_for_creator_and_adopter() {
    use baleyg::store::topology::MarkerStage;
    for stage in [
        MarkerStage::MarkerSync,
        MarkerStage::PrivateDirSync,
        MarkerStage::GitDirSync,
    ] {
        let (temp, _) = common::fixture();
        let work = root(temp.path());
        common::private(&work.join(".git"));
        let marker = work.join(".git/baleyg/workspace-id");
        let mut reached = vec![];
        let error = WorkspaceIdentity::discover_with_marker_hook(Some(&work), &work, |at| {
            reached.push(at);
            if at == stage {
                anyhow::bail!("injected {stage:?}")
            }
            Ok(())
        })
        .unwrap_err();
        assert!(error.to_string().contains("workspace_id_not_durable"));
        assert_eq!(fs::read(&marker).unwrap().len(), 36);
        let mut adopted = vec![];
        let id = WorkspaceIdentity::discover_with_marker_hook(Some(&work), &work, |at| {
            adopted.push(at);
            Ok(())
        })
        .unwrap();
        assert_eq!(id.record_id, fs::read_to_string(&marker).unwrap());
        assert_eq!(
            adopted,
            [
                MarkerStage::MarkerSync,
                MarkerStage::PrivateDirSync,
                MarkerStage::GitDirSync
            ]
        );
        id.verify().unwrap();
    }
}

#[test]
fn persistent_short_and_malformed_markers_remain_unchanged() {
    let (temp, _) = common::fixture();
    let work = root(temp.path());
    common::private(&work.join(".git"));
    let marker = work.join(".git/baleyg/workspace-id");
    let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    for bytes in [
        b"partial".as_slice(),
        b"gaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa",
        b"aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaaextra",
    ] {
        fs::write(&marker, bytes).unwrap();
        assert!(WorkspaceIdentity::discover(Some(&work), &work).is_err());
        assert_eq!(fs::read(&marker).unwrap(), bytes);
    }
    fs::write(&marker, id.record_id.as_bytes()).unwrap();
    let original = id.record_id;
    assert_eq!(
        WorkspaceIdentity::discover(Some(&work), &work)
            .unwrap()
            .record_id,
        original
    );
    fs::remove_file(&marker).unwrap();
    std::os::unix::fs::symlink(work.join(".git"), &marker).unwrap();
    assert!(WorkspaceIdentity::discover(Some(&work), &work).is_err());
}

#[test]
fn multiprocess_lock_protocol() {
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    let leader = roots.leader(&id).unwrap();
    leader.verify().unwrap();
    assert_eq!(
        fs::read(roots.leader_lock(&id)).unwrap(),
        leader.incarnation.to_string().as_bytes()
    );
    assert!(roots.leader(&id).is_err());
    assert!(UseGuard::acquire(&roots.index_use_lock(&id), true, true).is_err());
    drop(leader);
    let next = roots.leader(&id).unwrap();
    assert_ne!(
        next.incarnation.to_string(),
        "00000000-0000-0000-0000-000000000000"
    );
    drop(next);
    let exclusive = UseGuard::acquire(&roots.index_use_lock(&id), true, true).unwrap();
    exclusive.remove_last().unwrap();
    assert!(!roots.index_use_lock(&id).exists());
}

#[test]
fn leader_child() {
    if let Some(path) = std::env::var_os("TOPOLOGY_LEADER_CHILD") {
        let base = Path::new(&path);
        let roots = baleyg::store::topology::TopologyRoots::isolated_for_tests(
            base.join("cache"),
            base.join("data"),
        );
        let work = base.join("work");
        let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
        let guard = if std::env::var_os("TOPOLOGY_PAUSE_BEFORE_WRITE").is_some() {
            roots
                .leader_with_hooks(
                    &id,
                    || {
                        println!("BEFORE_WRITE");
                        std::io::stdout().flush().unwrap();
                        let mut byte = [0];
                        std::io::stdin().read_exact(&mut byte).unwrap();
                        Ok(())
                    },
                    || Ok(()),
                )
                .unwrap()
        } else {
            roots.leader(&id).unwrap()
        };
        println!("READY={}", guard.incarnation);
        std::io::stdout().flush().unwrap();
        let mut byte = [0];
        std::io::stdin().read_exact(&mut byte).unwrap();
        guard.verify().unwrap();
    }
}
#[test]
fn suspended_leader_is_not_displaced() {
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("leader_child")
        .arg("--nocapture")
        .env("TOPOLOGY_LEADER_CHILD", temp.path())
        .stdout(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = String::new();
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        output.push_str(&line);
        if line.contains("READY=") {
            break;
        }
    }
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGSTOP) }, 0);
    assert!(
        roots
            .leader(&id)
            .unwrap_err()
            .to_string()
            .contains("storage_busy")
    );
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGCONT) }, 0);
    child.stdin.take().unwrap().write_all(&[1]).unwrap();
    assert!(child.wait().unwrap().success());
    let next = roots.leader(&id).unwrap();
    assert!(!output.contains(&format!("READY={}", next.incarnation)));
}
#[test]
fn pause_before_incarnation_and_sync_fault() {
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("leader_child")
        .arg("--nocapture")
        .env("TOPOLOGY_LEADER_CHILD", temp.path())
        .env("TOPOLOGY_PAUSE_BEFORE_WRITE", "1")
        .stdout(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line.contains("BEFORE_WRITE") {
            break;
        }
    }
    assert_eq!(fs::read(roots.leader_lock(&id)).unwrap(), b"");
    assert!(
        roots
            .leader(&id)
            .unwrap_err()
            .to_string()
            .contains("storage_busy")
    );
    child.stdin.take().unwrap().write_all(&[1, 1]).unwrap();
    assert!(child.wait().unwrap().success());
    let error = roots
        .leader_with_hooks(&id, || Ok(()), || anyhow::bail!("injected sync failure"))
        .unwrap_err();
    assert!(error.to_string().contains("incarnation_not_durable"));
    roots.leader(&id).unwrap().verify().unwrap();
}

#[test]
fn external_write_destinations() {
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    assert!(roots.validate_external(&id, &[work.join("token")]).is_err());
    assert!(
        roots
            .validate_external(&id, &[temp.path().join("absent/../work/token")])
            .is_err()
    );
    assert!(
        roots
            .validate_external(&id, &[roots.data.join("token")])
            .is_err()
    );
    assert!(
        roots
            .validate_external(&id, &[temp.path().join("outside")])
            .is_ok()
    );
    assert!(
        roots
            .validate_external(
                &id,
                &[
                    temp.path().join("outside"),
                    temp.path().join("outside/child")
                ]
            )
            .is_err()
    );
    let another = temp.path().join("another");
    fs::create_dir(&another).unwrap();
    common::private(&another.join(".git"));
    assert!(
        roots
            .validate_external(&id, &[another.join("token")])
            .is_err()
    );
}

#[test]
fn marker_sync_faults_refuse_creator_and_adopter() {
    let (temp, _) = common::fixture();
    let work = root(temp.path());
    common::private(&work.join(".git"));
    let fail = || anyhow::bail!("injected marker descriptor sync failure");
    let error =
        WorkspaceIdentity::discover_with_marker_sync_hook(Some(&work), &work, fail).unwrap_err();
    assert!(error.to_string().contains("workspace_id_not_durable"));
    let marker = work.join(".git/baleyg/workspace-id");
    assert_eq!(fs::read(&marker).unwrap().len(), 36);
    let error =
        WorkspaceIdentity::discover_with_marker_sync_hook(Some(&work), &work, fail).unwrap_err();
    assert!(error.to_string().contains("workspace_id_not_durable"));
    WorkspaceIdentity::discover(Some(&work), &work)
        .unwrap()
        .verify()
        .unwrap();
}

#[test]
fn roots_overlapping_fixed_locations_are_refused_before_creation() {
    let (temp, roots) = common::fixture();
    fs::create_dir(&roots.cache).unwrap();
    fs::create_dir(&roots.data).unwrap();
    let alias = temp.path().join("cache-alias");
    std::os::unix::fs::symlink(&roots.cache, &alias).unwrap();
    let nested = roots.cache.join("nested");
    fs::create_dir(&nested).unwrap();
    let alias_nested = alias.join("nested");
    for candidate in [&roots.cache, &roots.data, temp.path(), &alias_nested] {
        let identity = WorkspaceIdentity::discover(Some(candidate), candidate).unwrap();
        assert!(
            roots
                .prepare_index(&identity)
                .unwrap_err()
                .to_string()
                .contains("overlaps")
        );
        assert!(
            roots
                .prepare_records(&identity)
                .unwrap_err()
                .to_string()
                .contains("overlaps")
        );
        assert!(!roots.cache.join("indexes").exists());
        assert!(!roots.data.join("workspaces").exists());
    }
    let id = WorkspaceIdentity::discover(Some(&nested), &nested).unwrap();
    assert!(roots.prepare_index(&id).is_err());
    assert!(!roots.cache.join("indexes").exists());
}

#[test]
fn shared_use_cannot_remove_last_and_live_holder_blocks_exclusive() {
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let identity = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    let shared = roots.index_use(&identity).unwrap();
    let path = roots.index_use_lock(&identity);
    assert!(
        UseGuard::acquire(&path, false, false)
            .unwrap()
            .remove_last()
            .unwrap_err()
            .to_string()
            .contains("exclusive")
    );
    assert!(
        UseGuard::acquire(&path, true, true)
            .unwrap_err()
            .to_string()
            .contains("storage_busy")
    );
    assert!(path.exists());
    shared.verify().unwrap();
    drop(shared);
    // Parallel process fixtures may briefly inherit a test lock during spawn.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let exclusive = loop {
        match UseGuard::acquire(&path, true, true) {
            Ok(guard) => break guard,
            Err(error)
                if error.to_string().starts_with("storage_busy")
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::yield_now()
            }
            Err(error) => panic!("exclusive lock remained busy after releasing holders: {error}"),
        }
    };
    exclusive.remove_last().unwrap();
}

#[test]
fn stale_inode_waiters_reopen_before_success() {
    use std::io::{BufRead, BufReader};
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let identity = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    roots.prepare_index(&identity).unwrap();
    let path = roots.index_use_lock(&identity);
    let original = UseGuard::acquire(&path, true, true).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("stale_use_child")
        .arg("--nocapture")
        .env("TOPOLOGY_STALE_USE", &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line.contains("OLD_OPEN") {
            break;
        }
    }
    original.remove_last().unwrap();
    let replacement = UseGuard::acquire(&path, true, true).unwrap();
    child.stdin.take().unwrap().write_all(&[1]).unwrap();
    // The child first locks the unlinked inode, then must wait for the new one.
    assert!(child.try_wait().unwrap().is_none());
    drop(replacement);
    assert!(child.wait().unwrap().success());
    let mut rest = String::new();
    reader.read_to_string(&mut rest).unwrap();
    assert!(rest.contains("NEW_INODE_VERIFIED"), "{rest}");

    // The leader opens its old inode while paused. Replace it before flock.
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("stale_leader_child")
        .arg("--nocapture")
        .env("TOPOLOGY_STALE_LEADER", temp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line.contains("LEADER_OLD_OPEN") {
            break;
        }
    }
    let leader_path = roots.leader_lock(&identity);
    fs::remove_file(&leader_path).unwrap();
    fs::write(&leader_path, "").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&leader_path, fs::Permissions::from_mode(0o600)).unwrap();
    child.stdin.take().unwrap().write_all(&[1]).unwrap();
    assert!(child.wait().unwrap().success());
    let mut rest = String::new();
    reader.read_to_string(&mut rest).unwrap();
    assert!(rest.contains("LEADER_NEW_VERIFIED"), "{rest}");
}

#[test]
fn stale_use_child() {
    if let Some(path) = std::env::var_os("TOPOLOGY_STALE_USE") {
        let path = Path::new(&path);
        let guard = UseGuard::acquire_with_hook(path, false, false, || {
            println!("OLD_OPEN");
            std::io::stdout().flush()?;
            let mut byte = [0];
            std::io::stdin().read_exact(&mut byte)?;
            Ok(())
        })
        .unwrap();
        guard.verify().unwrap();
        println!("NEW_INODE_VERIFIED");
    }
}
#[test]
fn stale_leader_child() {
    if let Some(path) = std::env::var_os("TOPOLOGY_STALE_LEADER") {
        let base = Path::new(&path);
        let roots = baleyg::store::topology::TopologyRoots::isolated_for_tests(
            base.join("cache"),
            base.join("data"),
        );
        let work = base.join("work");
        let identity = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
        let guard = roots
            .leader_with_lock_hook(
                &identity,
                || {
                    println!("LEADER_OLD_OPEN");
                    std::io::stdout().flush()?;
                    let mut byte = [0];
                    std::io::stdin().read_exact(&mut byte)?;
                    Ok(())
                },
                || Ok(()),
                || Ok(()),
            )
            .unwrap();
        guard.verify().unwrap();
        println!("LEADER_NEW_VERIFIED");
    }
}

#[test]
fn exclusive_deletion_waits_for_shared_process() {
    use std::io::{BufRead, BufReader};
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    roots.prepare_index(&id).unwrap();
    let path = roots.index_use_lock(&id);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("shared_holder_child")
        .arg("--nocapture")
        .env("TOPOLOGY_SHARED_HOLDER", &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line.contains("SHARED_HELD") {
            break;
        }
    }
    assert!(
        UseGuard::acquire(&path, true, true)
            .unwrap_err()
            .to_string()
            .contains("storage_busy")
    );
    assert!(path.exists());
    child.stdin.take().unwrap().write_all(&[1]).unwrap();
    assert!(child.wait().unwrap().success());
    UseGuard::acquire(&path, true, true)
        .unwrap()
        .remove_last()
        .unwrap();
    assert!(!path.exists());
}
#[test]
fn shared_holder_child() {
    if let Some(path) = std::env::var_os("TOPOLOGY_SHARED_HOLDER") {
        let guard = UseGuard::acquire(Path::new(&path), false, false).unwrap();
        println!("SHARED_HELD");
        std::io::stdout().flush().unwrap();
        let mut byte = [0];
        std::io::stdin().read_exact(&mut byte).unwrap();
        guard.verify().unwrap();
    }
}
use std::os::unix::fs::PermissionsExt;

#[test]
fn durable_first_save_empty_record_and_payloads() {
    use baleyg::{
        model::{Annotation, SavedView},
        store::topology::DurableRecords,
    };
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    let records = DurableRecords::new(&roots, &id);
    assert!(records.views().unwrap().is_empty());
    assert!(records.annotations().unwrap().is_empty());
    assert!(!records.delete_view("missing").unwrap());
    assert!(!roots.data.exists(), "read-only access created data root");
    let view: SavedView = serde_json::from_str(r#"{"id":"view1","title":"A view","query":{"seed":"symbol"},"pins":{"symbol":{"x":12.5,"y":9.25}}}"#).unwrap();
    records.put_view(&view).unwrap();
    let annotation = Annotation {
        id: "note1".into(),
        node_id: "symbol".into(),
        body: "Original text".into(),
    };
    records.put_annotation(&annotation).unwrap();
    assert_eq!(records.view("view1").unwrap(), Some(view.clone()));
    assert_eq!(records.annotations().unwrap(), vec![annotation.clone()]);
    assert!(records.delete_view("view1").unwrap());
    assert!(records.delete_annotation("note1").unwrap());
    assert!(records.views().unwrap().is_empty());
    assert!(roots.record_db(&id).exists());
    let db = rusqlite::Connection::open(roots.record_db(&id)).unwrap();
    assert_eq!(
        db.query_row("SELECT record_id FROM record_metadata", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        id.record_id
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM known_roots", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        tables,
        ["annotations", "known_roots", "record_metadata", "views"]
    );
    assert!(
        !roots
            .record_db(&id)
            .with_file_name("workspace.db-wal")
            .exists()
    );
}

#[test]
fn durable_git_move_and_copy_share_payload_without_rewriting() {
    use baleyg::{model::Annotation, store::topology::DurableRecords};
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    common::private(&work.join(".git"));
    let original = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    let note = Annotation {
        id: "note".into(),
        node_id: "gone".into(),
        body: "Persist verbatim".into(),
    };
    DurableRecords::new(&roots, &original)
        .put_annotation(&note)
        .unwrap();
    let copied = temp.path().join("copy");
    fs::create_dir(&copied).unwrap();
    common::private(&copied.join(".git"));
    fs::create_dir(copied.join(".git/baleyg")).unwrap();
    fs::set_permissions(
        copied.join(".git/baleyg"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    fs::copy(
        work.join(".git/baleyg/workspace-id"),
        copied.join(".git/baleyg/workspace-id"),
    )
    .unwrap();
    let copy = WorkspaceIdentity::discover(Some(&copied), &copied).unwrap();
    assert_eq!(copy.record_id, original.record_id);
    assert_eq!(
        DurableRecords::new(&roots, &copy).annotations().unwrap(),
        vec![note.clone()]
    );
    let moved = temp.path().join("moved");
    fs::rename(&work, &moved).unwrap();
    let moved_id = WorkspaceIdentity::discover(Some(&moved), &moved).unwrap();
    DurableRecords::new(&roots, &moved_id)
        .put_annotation(&note)
        .unwrap();
    assert_eq!(
        DurableRecords::new(&roots, &copy).annotations().unwrap(),
        vec![note.clone()]
    );
    assert!(
        DurableRecords::new(&roots, &original)
            .annotations()
            .is_err()
    );
    let db = rusqlite::Connection::open(roots.record_db(&copy)).unwrap();
    let rows: Vec<(String, String, String)> = db
        .prepare("SELECT path,device,inode FROM known_roots ORDER BY path")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut expected = vec![
        (
            original.root.to_str().unwrap().to_owned(),
            original.device.to_string(),
            original.inode.to_string(),
        ),
        (
            moved_id.root.to_str().unwrap().to_owned(),
            moved_id.device.to_string(),
            moved_id.inode.to_string(),
        ),
    ];
    expected.sort();
    assert_eq!(rows, expected);
    let payload: String = db
        .query_row("SELECT payload FROM annotations WHERE id='note'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(payload, serde_json::to_string(&note).unwrap());
}

#[test]
fn durable_incomplete_and_incompatible_refused() {
    use baleyg::{model::Annotation, store::topology::DurableRecords};
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    roots.prepare_records(&id).unwrap();
    common::private(&roots.record_dir(&id));
    let note = Annotation {
        id: "note".into(),
        node_id: "node".into(),
        body: "Body".into(),
    };
    assert!(
        DurableRecords::new(&roots, &id)
            .put_annotation(&note)
            .is_err()
    );
    assert!(!roots.record_db(&id).exists());
    let _lock = roots.record_use(&id, true).unwrap();
    let db = rusqlite::Connection::open(roots.record_db(&id)).unwrap();
    db.pragma_update(None, "user_version", 99).unwrap();
    drop(db);
    drop(_lock);
    assert!(DurableRecords::new(&roots, &id).annotations().is_err());
}

#[test]
fn durable_absent_parent_is_verified() {
    use baleyg::store::topology::DurableRecords;
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    fs::create_dir(&roots.data).unwrap();
    fs::set_permissions(&roots.data, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(DurableRecords::new(&roots, &id).views().is_err());
    fs::set_permissions(&roots.data, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(roots.data.join("workspaces")).unwrap();
    fs::set_permissions(
        roots.data.join("workspaces"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(
        DurableRecords::new(&roots, &id)
            .delete_view("absent")
            .is_err()
    );
    fs::remove_dir(roots.data.join("workspaces")).unwrap();
    std::os::unix::fs::symlink(temp.path(), roots.data.join("workspaces")).unwrap();
    assert!(DurableRecords::new(&roots, &id).annotations().is_err());
}

#[test]
fn durable_existing_lock_unlink_never_recreates() {
    use baleyg::store::topology::DurableRecords;
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    let note = baleyg::model::Annotation {
        id: "n".into(),
        node_id: "node".into(),
        body: "body".into(),
    };
    DurableRecords::new(&roots, &id)
        .put_annotation(&note)
        .unwrap();
    let lock = roots.record_use_lock(&id);
    assert!(
        UseGuard::acquire_existing_with_hook(&lock, false, false, || {
            fs::remove_file(&lock)?;
            Ok(())
        })
        .is_err()
    );
    assert!(!lock.exists(), "existing-only reopen created a lock");
    assert!(DurableRecords::new(&roots, &id).annotations().is_err());
    assert!(!lock.exists(), "durable read created a lock");
    assert!(
        DurableRecords::new(&roots, &id)
            .delete_annotation("n")
            .is_err()
    );
    assert!(!lock.exists(), "durable delete created a lock");
}

#[test]
fn durable_existing_lock_replacement_checks_new_inode_and_holder() {
    use std::io::{BufRead, BufReader};
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    roots.prepare_records(&id).unwrap();
    let lock = roots.record_use_lock(&id);
    drop(roots.record_use(&id, true).unwrap());
    let mut child = None;
    let mut reader = None;
    let result = UseGuard::acquire_existing_with_hook(&lock, false, true, || {
        fs::remove_file(&lock)?;
        drop(roots.record_use(&id, true)?);
        let mut process = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("shared_holder_child")
            .arg("--nocapture")
            .env("TOPOLOGY_SHARED_HOLDER", &lock)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut output = BufReader::new(process.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(output.read_line(&mut line)?, 0);
            if line.contains("SHARED_HELD") {
                break;
            }
        }
        reader = Some(output);
        child = Some(process);
        Ok(())
    });
    // The replacement is held shared. An exclusive acquisition must not use the old inode.
    assert!(result.is_ok());
    let process = child.as_mut().unwrap();
    assert!(
        UseGuard::acquire_existing(&lock, true, true)
            .unwrap_err()
            .to_string()
            .contains("storage_busy")
    );
    process.stdin.take().unwrap().write_all(&[1]).unwrap();
    assert!(process.wait().unwrap().success());
}

#[test]
fn durable_annotation_first_commit_fault_and_sync_fault() {
    use baleyg::{model::Annotation, store::topology::DurableRecords};
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    let note = Annotation {
        id: "first".into(),
        node_id: "node".into(),
        body: "original".into(),
    };
    let records = DurableRecords::new(&roots, &id);
    assert!(
        records
            .put_annotation_with_first_save_hook(&note, |phase| {
                if phase == "before_commit" {
                    anyhow::bail!("injected commit fault")
                }
                Ok(())
            })
            .unwrap_err()
            .to_string()
            .contains("injected commit fault")
    );
    assert!(
        records
            .annotations()
            .unwrap_err()
            .to_string()
            .contains("incomplete_record")
    );
    assert!(
        records.put_annotation(&note).is_err(),
        "must not heal incomplete record"
    );
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    let records = DurableRecords::new(&roots, &id);
    assert!(
        records
            .put_annotation_with_first_save_hook(&note, |phase| {
                if phase == "before_sync" {
                    anyhow::bail!("injected sync fault")
                }
                Ok(())
            })
            .unwrap_err()
            .to_string()
            .contains("injected sync fault")
    );
    // A committed record is not silently erased or described as incomplete after sync refusal.
    assert_eq!(records.annotations().unwrap(), vec![note]);
}

#[test]
fn durable_creator_child() {
    if let Some(base) = std::env::var_os("TOPOLOGY_DURABLE_CHILD") {
        use baleyg::{model::Annotation, store::topology::DurableRecords};
        let base = Path::new(&base);
        let roots = baleyg::store::topology::TopologyRoots::isolated_for_tests(
            base.join("cache"),
            base.join("data"),
        );
        let work = base.join("work");
        let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
        let note = Annotation {
            id: "loser".into(),
            node_id: "node".into(),
            body: "second".into(),
        };
        assert!(
            DurableRecords::new(&roots, &id)
                .put_annotation(&note)
                .unwrap_err()
                .to_string()
                .contains("storage_busy")
        );
        println!("CREATOR_BUSY");
        std::io::stdout().flush().unwrap();
        let mut byte = [0];
        std::io::stdin().read_exact(&mut byte).unwrap();
        DurableRecords::new(&roots, &id)
            .put_annotation(&note)
            .unwrap();
        println!("CREATOR_RETRIED");
    }
}

#[test]
fn durable_first_creator_process_contention_and_retry() {
    use baleyg::{model::Annotation, store::topology::DurableRecords};
    use std::io::{BufRead, BufReader};
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    let lock = roots.record_use(&id, true).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("durable_creator_child")
        .arg("--nocapture")
        .env("TOPOLOGY_DURABLE_CHILD", temp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(output.read_line(&mut line).unwrap(), 0);
        if line.contains("CREATOR_BUSY") {
            break;
        }
    }
    drop(lock);
    let first = Annotation {
        id: "winner".into(),
        node_id: "node".into(),
        body: "first".into(),
    };
    DurableRecords::new(&roots, &id)
        .put_annotation(&first)
        .unwrap();
    child.stdin.take().unwrap().write_all(&[1]).unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        DurableRecords::new(&roots, &id)
            .annotations()
            .unwrap()
            .len(),
        2
    );
    let db = rusqlite::Connection::open(roots.record_db(&id)).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM record_metadata", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn durable_non_git_move_retains_old_record_and_root_row() {
    use baleyg::{model::Annotation, store::topology::DurableRecords};
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let old = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    let note = Annotation {
        id: "n".into(),
        node_id: "node".into(),
        body: "unchanged".into(),
    };
    DurableRecords::new(&roots, &old)
        .put_annotation(&note)
        .unwrap();
    let old_db = roots.record_db(&old);
    fs::rename(&work, temp.path().join("moved")).unwrap();
    let moved_path = temp.path().join("moved");
    let moved = WorkspaceIdentity::discover(Some(&moved_path), &moved_path).unwrap();
    assert_ne!(old.record_id, moved.record_id);
    assert!(
        DurableRecords::new(&roots, &moved)
            .annotations()
            .unwrap()
            .is_empty()
    );
    assert!(old_db.exists());
    let db = rusqlite::Connection::open(&old_db).unwrap();
    let saved: (String, String, String) = db
        .query_row("SELECT path,device,inode FROM known_roots", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(
        saved,
        (
            old.root.to_str().unwrap().into(),
            old.device.to_string(),
            old.inode.to_string()
        )
    );
    let payload: String = db
        .query_row("SELECT payload FROM annotations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(serde_json::from_str::<Annotation>(&payload).unwrap(), note);
}

#[test]
fn durable_marker_change_refuses_real_operation() {
    use baleyg::{model::Annotation, store::topology::DurableRecords};
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    common::private(&work.join(".git"));
    let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    let note = Annotation {
        id: "n".into(),
        node_id: "node".into(),
        body: "body".into(),
    };
    DurableRecords::new(&roots, &id)
        .put_annotation(&note)
        .unwrap();
    fs::write(
        work.join(".git/baleyg/workspace-id"),
        uuid::Uuid::new_v4().to_string(),
    )
    .unwrap();
    assert!(DurableRecords::new(&roots, &id).annotations().is_err());
    assert!(
        DurableRecords::new(&roots, &id)
            .put_annotation(&note)
            .is_err()
    );
    assert!(
        DurableRecords::new(&roots, &id)
            .delete_annotation("n")
            .is_err()
    );
}

#[test]
fn index_open_and_generation() {
    let base = tempfile::tempdir().unwrap();
    let work = root(base.path());
    let state = base.path().join("state");
    let store = common::open_store(&state, &work).unwrap();
    let first = store.status().unwrap().revision;
    assert_eq!(first.index_revision, 0);
    assert_eq!(first.index_generation.get_version_num(), 4);
    drop(store);
    let reopened = common::open_store(&state, &work).unwrap();
    assert_eq!(reopened.status().unwrap().revision, first);
}
#[test]
fn index_delete_journal_no_wal() {
    let base = tempfile::tempdir().unwrap();
    let work = root(base.path());
    let state = base.path().join("state");
    let store = common::open_store(&state, &work).unwrap();
    let index = fs::read_dir(state.join("cache/indexes"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
        .join("index.db");
    let db =
        rusqlite::Connection::open_with_flags(&index, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let journal: String = db
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .unwrap();
    assert_eq!(journal, "delete");
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
    drop(db);
    let bytes = fs::read(&index).unwrap();
    assert_eq!((bytes[18], bytes[19]), (1, 1));
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(!index.with_file_name(format!("index.db{suffix}")).exists());
    }
    assert_eq!(store.status().unwrap().revision.index_revision, 0);
}
#[test]
fn leader_records_open_age_and_follower_preserves_it() {
    let base = tempfile::tempdir().unwrap();
    let work = root(base.path());
    let state = base.path().join("state");
    let store = common::open_store(&state, &work).unwrap();
    let index = fs::read_dir(state.join("cache/indexes"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
        .join("index.db");
    let read_age = || -> i64 {
        rusqlite::Connection::open(&index)
            .unwrap()
            .query_row("SELECT last_opened_at FROM index_metadata", [], |r| {
                r.get(0)
            })
            .unwrap()
    };
    assert!(read_age() > 0);
    let baseline = store.status().unwrap().revision;
    let leader = store.leader().unwrap();
    let age = read_age();
    assert!(age > 0);
    drop(leader);
    drop(store);
    let follower = common::open_store(&state, &work).unwrap();
    assert_eq!(read_age(), age);
    assert_eq!(follower.status().unwrap().revision, baseline);
    let leader = follower.leader().unwrap();
    assert!(read_age() >= age);
    assert_eq!(follower.status().unwrap().revision, baseline);
    drop(leader);
}

#[test]
fn guard_drop_unlocks_even_when_fork_child_keeps_descriptor() {
    let (temp, roots) = common::fixture();
    let work = root(temp.path());
    let id = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    let guard = roots.leader(&id).unwrap();
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        unsafe { libc::close(pipe[1]) };
        let mut byte = 0u8;
        unsafe { libc::read(pipe[0], (&mut byte as *mut u8).cast(), 1) };
        unsafe { libc::_exit(0) };
    }
    unsafe { libc::close(pipe[0]) };
    drop(guard);
    let exclusive = UseGuard::acquire_existing(&roots.index_use_lock(&id), true, true).unwrap();
    drop(exclusive);
    let next = roots.leader(&id).unwrap();
    drop(next);
    unsafe { libc::close(pipe[1]) };
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
    assert_eq!(status, 0);
}
