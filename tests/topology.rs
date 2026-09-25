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
