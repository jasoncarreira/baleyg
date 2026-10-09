use baleyg::{
    daemon::registry::{CheckoutOptions, CheckoutRegistry},
    store::{
        Store,
        topology::{TopologyRoots, WorkspaceIdentity},
    },
};
use std::{fs, path::Path, time::Duration};

fn identity(root: &Path) -> WorkspaceIdentity {
    WorkspaceIdentity::discover(Some(root), root)
        .unwrap()
        .attach_marker()
        .unwrap()
}
fn roots(state: &Path) -> TopologyRoots {
    TopologyRoots::isolated_for_tests(state.join("cache"), state.join("data"))
}
async fn ready(runtime: &baleyg::daemon::registry::CheckoutRuntime) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok((response, catching_up)) = runtime.evidence_response() {
                let finished = response.finish(()).is_ok();
                if finished && !catching_up {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("H did not finish: {:?}", runtime.reconciliation_error()));
}

#[tokio::test]
async fn registration_does_not_open_a_store_and_active_checkouts_are_isolated() {
    let base = tempfile::tempdir().unwrap();
    let a = base.path().join("a");
    let b = base.path().join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    fs::write(
        a.join("a.js"),
        "function checkoutA() {}
",
    )
    .unwrap();
    fs::write(
        b.join("b.js"),
        "function checkoutB() {}
",
    )
    .unwrap();
    let first = identity(&a);
    let second = identity(&b);
    let topology = roots(base.path());
    let mut registry = CheckoutRegistry::with_roots(topology.clone());
    registry
        .register(&first, CheckoutOptions(serde_json::json!({})))
        .unwrap();
    registry
        .register(&second, CheckoutOptions(serde_json::json!({})))
        .unwrap();
    assert!(!topology.index_dir(&first).exists());
    assert!(!topology.index_dir(&second).exists());
    registry.attach_launch(1, &first).unwrap();
    let active = registry.activate(&first.root_key).unwrap();
    assert!(topology.index_dir(&first).exists());
    assert!(!topology.index_dir(&second).exists());
    ready(&active).await;
    registry.attach_launch(2, &second).unwrap();
    let other = registry.activate(&second.root_key).unwrap();
    ready(&other).await;
    let (response, _) = active.evidence_response().unwrap();
    assert_eq!(
        response.status().unwrap().workspace_root,
        fs::canonicalize(&a).unwrap().to_string_lossy()
    );
    response.finish(()).unwrap();
    let (response, _) = other.evidence_response().unwrap();
    assert_eq!(
        response.status().unwrap().workspace_root,
        fs::canonicalize(&b).unwrap().to_string_lossy()
    );
    response.finish(()).unwrap();
}

#[tokio::test]
async fn external_standalone_leader_keeps_daemon_a_follower() {
    let base = tempfile::tempdir().unwrap();
    let checkout = base.path().join("work");
    fs::create_dir(&checkout).unwrap();
    fs::write(
        checkout.join("a.js"),
        "function oldHead() {}
",
    )
    .unwrap();
    let id = identity(&checkout);
    let topology = roots(base.path());
    let standalone = Store::open(topology.clone(), identity(&checkout)).unwrap();
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let leader = baleyg::index_coordinator::establish_serving_session(
        &standalone,
        Some(&baleyg::indexer::IndexOptions::new(checkout.clone())),
        &cancel,
    )
    .unwrap();
    assert!(leader.is_leader());
    let mut registry = CheckoutRegistry::with_roots(topology);
    registry.attach_launch(1, &id).unwrap();
    let runtime = registry.activate(&id.root_key).unwrap();
    ready(&runtime).await;
    assert!(leader.verify().is_ok());
    assert!(runtime.is_follower());
}

#[tokio::test]
async fn reattach_reports_catching_up_and_keeps_complete_head_until_h() {
    let base = tempfile::tempdir().unwrap();
    let checkout = base.path().join("work");
    fs::create_dir(&checkout).unwrap();
    fs::write(
        checkout.join("a.js"),
        "function oldHead() {}
",
    )
    .unwrap();
    let id = identity(&checkout);
    let mut registry = CheckoutRegistry::with_roots(roots(base.path()));
    registry.attach_launch(1, &id).unwrap();
    let runtime = registry.activate(&id.root_key).unwrap();
    assert!(runtime.catching_up(), "mandatory H has not run yet");
    ready(&runtime).await;
    let (old, stale) = runtime.evidence_response().unwrap();
    assert!(!stale);
    let first = old.status().unwrap().revision;
    old.finish(()).unwrap();
    drop(old);
    registry.disconnect(1);
    assert!(registry.release(&id.root_key).unwrap());
    fs::write(
        checkout.join("a.js"),
        "function newHead() {}
",
    )
    .unwrap();
    registry.attach_launch(2, &id).unwrap();
    let resumed = registry.activate(&id.root_key).unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
    let resume_rx = std::sync::Mutex::new(resume_rx);
    resumed.set_pre_h_hook_for_tests(std::sync::Arc::new(move || {
        entered_tx.send(()).unwrap();
        resume_rx.lock().unwrap().recv().unwrap();
    }));
    tokio::time::timeout(Duration::from_secs(5), async {
        while entered_rx.try_recv().is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("pre-H worker did not reach pause");
    assert!(resumed.catching_up());
    let (prior, catching_up) = resumed
        .evidence_response()
        .expect("validated old head while H paused");
    assert!(catching_up);
    assert_eq!(prior.status().unwrap().revision, first);
    let (_, old_source) = prior.source_at("a.js", None).unwrap().unwrap();
    assert!(old_source.text.contains("oldHead"));
    prior.finish(()).unwrap();
    drop(prior);
    resume_tx.send(()).unwrap();
    ready(&resumed).await;
    let (current, catching_up) = resumed.evidence_response().unwrap();
    assert!(!catching_up);
    let new_revision = current.status().unwrap().revision;
    assert!(new_revision.index_revision > first.index_revision);
    current.finish(()).unwrap();
}

#[tokio::test]
async fn committed_queue_survives_runtime_restart() {
    let base = tempfile::tempdir().unwrap();
    let checkout = base.path().join("work");
    fs::create_dir(&checkout).unwrap();
    fs::write(
        checkout.join("a.js"),
        "function queued() {}
",
    )
    .unwrap();
    let id = identity(&checkout);
    let topology = roots(base.path());
    let store = Store::open(topology.clone(), identity(&checkout)).unwrap();
    let options = baleyg::indexer::IndexOptions::new(checkout.clone());
    let accepted = store.enqueue_request(&options, None).unwrap();
    drop(store);
    let mut restarted = CheckoutRegistry::with_roots(topology.clone());
    restarted.attach_launch(1, &id).unwrap();
    let _runtime = restarted.activate(&id.root_key).unwrap();
    let observed = Store::open(topology, identity(&checkout)).unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if observed
                .request_by_id(&accepted.id)
                .unwrap()
                .unwrap()
                .finished_at
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        observed.request_by_id(&accepted.id).unwrap().unwrap().state,
        "done"
    );
}

#[cfg(unix)]
fn file_descriptors(path: &Path) -> usize {
    use std::os::unix::fs::MetadataExt;
    let expected = fs::metadata(path).unwrap();
    (0..1024)
        .filter(|fd| {
            let mut held = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe { libc::fstat(*fd, held.as_mut_ptr()) } != 0 {
                return false;
            }
            let held = unsafe { held.assume_init() };
            #[cfg(target_os = "macos")]
            let device = held.st_dev as u64;
            #[cfg(not(target_os = "macos"))]
            let device = held.st_dev;
            (device, held.st_ino) == (expected.dev(), expected.ino())
        })
        .count()
}

#[tokio::test]
async fn retained_registration_and_old_runtime_handle_hold_no_checkout_descriptors() {
    let base = tempfile::tempdir().unwrap();
    let checkout = base.path().join("work");
    fs::create_dir(&checkout).unwrap();
    fs::write(checkout.join("a.js"), "function beforeRelease() {}\n").unwrap();
    let baseline = file_descriptors(&checkout);
    let id = identity(&checkout);
    let key = id.root_key.clone();
    let topology = roots(base.path());
    let lock_path = topology.leader_lock(&id);
    let index_db = topology.index_db(&id);
    let requests_db = topology.requests_db(&id);
    let mut registry = CheckoutRegistry::with_roots(topology);
    registry
        .register(&id, CheckoutOptions(serde_json::json!({})))
        .unwrap();
    drop(id);
    assert_eq!(
        file_descriptors(&checkout),
        baseline,
        "registration must retain metadata only"
    );
    let id = identity(&checkout);
    registry.attach_launch(1, &id).unwrap();
    drop(id);
    let old = registry.activate(&key).unwrap();
    ready(&old).await;
    assert!(old.has_active_resources());
    assert!(file_descriptors(&checkout) > baseline);
    assert!(
        file_descriptors(&lock_path) > 0,
        "active leader holds checkout lock"
    );
    assert!(file_descriptors(&index_db) > 0);
    assert!(file_descriptors(&requests_db) > 0);
    registry.disconnect(1);
    assert!(registry.release(&key).unwrap());
    assert!(!old.has_active_resources());
    assert!(
        old.evidence_response().is_err(),
        "released handles cannot answer"
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while file_descriptors(&checkout) != baseline
            || file_descriptors(&lock_path) != 0
            || file_descriptors(&index_db) != 0
            || file_descriptors(&requests_db) != 0
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("released runtime, permit and registration must close root handles");
    fs::write(checkout.join("a.js"), "function afterRelease() {}\n").unwrap();
    let id = identity(&checkout);
    registry.attach_launch(2, &id).unwrap();
    drop(id);
    let resumed = registry.activate(&key).unwrap();
    ready(&resumed).await;
    assert!(!old.has_active_resources());
    let (response, catching_up) = resumed.evidence_response().unwrap();
    assert!(!catching_up);
    response.finish(()).unwrap();
}

#[tokio::test]
async fn queued_follower_is_pending_even_after_client_disconnect() {
    let base = tempfile::tempdir().unwrap();
    let checkout = base.path().join("work");
    fs::create_dir(&checkout).unwrap();
    fs::write(checkout.join("a.js"), "function pendingWork() {}\n").unwrap();
    let id = identity(&checkout);
    let topology = roots(base.path());
    let standalone = Store::open(topology.clone(), identity(&checkout)).unwrap();
    let options = baleyg::indexer::IndexOptions::new(checkout.clone());
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let leader =
        baleyg::index_coordinator::establish_serving_session(&standalone, Some(&options), &cancel)
            .unwrap();
    let mut registry = CheckoutRegistry::with_roots(topology);
    registry.attach_launch(1, &id).unwrap();
    let runtime = registry.activate(&id.root_key).unwrap();
    ready(&runtime).await;
    assert!(runtime.is_follower());
    let row = standalone.enqueue_request(&options, None).unwrap();
    assert_eq!(
        standalone.request_by_id(&row.id).unwrap().unwrap().state,
        "queued"
    );
    assert!(registry.refresh_pending_work(&id.root_key).unwrap());
    registry.disconnect(1);
    assert!(!registry.release(&id.root_key).unwrap());
    assert!(leader.verify().is_ok());
}

#[tokio::test]
async fn follower_takeover_marks_h_pending_and_next_reattach_admits_old_head() {
    let base = tempfile::tempdir().unwrap();
    let checkout = base.path().join("work");
    fs::create_dir(&checkout).unwrap();
    fs::write(checkout.join("a.js"), "function firstHead() {}\n").unwrap();
    let id = identity(&checkout);
    let topology = roots(base.path());
    let standalone = Store::open(topology.clone(), identity(&checkout)).unwrap();
    let options = baleyg::indexer::IndexOptions::new(checkout.clone());
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let leader =
        baleyg::index_coordinator::establish_serving_session(&standalone, Some(&options), &cancel)
            .unwrap();
    let mut registry = CheckoutRegistry::with_roots(topology);
    registry.attach_launch(1, &id).unwrap();
    let runtime = registry.activate(&id.root_key).unwrap();
    ready(&runtime).await;
    assert!(runtime.is_follower());
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
    let resume_rx = std::sync::Mutex::new(resume_rx);
    runtime.set_takeover_h_hook_for_tests(std::sync::Arc::new(move || {
        entered_tx.send(()).unwrap();
        resume_rx.lock().unwrap().recv().unwrap();
    }));
    fs::write(checkout.join("a.js"), "function takeoverHead() {}\n").unwrap();
    let accepted = standalone.enqueue_request(&options, None).unwrap();
    drop(leader);
    tokio::time::timeout(Duration::from_secs(10), async {
        while entered_rx.try_recv().is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("daemon did not elect after standalone released");
    assert!(
        runtime.catching_up(),
        "the newly elected owner has not committed H"
    );
    assert!(
        !runtime.is_follower(),
        "invalid predecessor follower must not remain ready"
    );
    assert_eq!(
        standalone
            .request_by_id(&accepted.id)
            .unwrap()
            .unwrap()
            .state,
        "queued"
    );
    resume_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while runtime.is_follower()
            || runtime.catching_up()
            || standalone
                .request_by_id(&accepted.id)
                .unwrap()
                .unwrap()
                .finished_at
                .is_none()
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("new daemon leader did not finish H and FIFO");
    let (current, flag) = runtime.evidence_response().unwrap();
    assert!(!flag);
    let established = current.status().unwrap().revision;
    current.finish(()).unwrap();
    drop(current);
    registry.disconnect(1);
    assert!(registry.release(&id.root_key).unwrap());
    fs::write(checkout.join("a.js"), "function nextHead() {}\n").unwrap();
    registry.attach_launch(2, &id).unwrap();
    let next = registry.activate(&id.root_key).unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
    let resume_rx = std::sync::Mutex::new(resume_rx);
    next.set_pre_h_hook_for_tests(std::sync::Arc::new(move || {
        entered_tx.send(()).unwrap();
        resume_rx.lock().unwrap().recv().unwrap();
    }));
    tokio::time::timeout(Duration::from_secs(5), async {
        while entered_rx.try_recv().is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("new daemon leader did not mint a reattach permit");
    let (prior, flag) = next
        .evidence_response()
        .expect("takeover leader must mint pre-H permit");
    assert!(flag);
    assert_eq!(prior.status().unwrap().revision, established);
    prior.finish(()).unwrap();
    drop(prior);
    resume_tx.send(()).unwrap();
    ready(&next).await;
    let (updated, flag) = next.evidence_response().unwrap();
    assert!(!flag);
    assert!(updated.status().unwrap().revision.index_revision > established.index_revision);
    updated.finish(()).unwrap();
}

#[tokio::test]
async fn root_loss_fails_accepted_work_then_releases_old_runtime_handles() {
    let base = tempfile::tempdir().unwrap();
    let checkout = base.path().join("work");
    let moved = base.path().join("moved");
    fs::create_dir(&checkout).unwrap();
    fs::write(checkout.join("a.js"), "function oldRoot() {}\n").unwrap();
    let id = identity(&checkout);
    let key = id.root_key.clone();
    let topology = roots(base.path());
    let db_path = topology.requests_db(&id);
    let leader_path = topology.leader_lock(&id);
    let mut registry = CheckoutRegistry::with_roots(topology.clone());
    registry.attach_launch(1, &id).unwrap();
    drop(id);
    let old = registry.activate(&key).unwrap();
    ready(&old).await;
    let store = Store::open(topology, identity(&checkout)).unwrap();
    let row = store
        .enqueue_request(&baleyg::indexer::IndexOptions::new(checkout.clone()), None)
        .unwrap();
    drop(store);
    fs::rename(&checkout, &moved).unwrap();
    fs::create_dir(&checkout).unwrap();
    fs::write(checkout.join("a.js"), "function newRoot() {}\n").unwrap();
    registry.disconnect(1);
    assert!(
        !registry.release(&key).unwrap(),
        "committed queued row cannot release before terminal root-loss result"
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while registry.refresh_pending_work(&key).unwrap() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("old-root queue did not reach a terminal result");
    let db = rusqlite::Connection::open(db_path).unwrap();
    let (state, error): (String, String) = db
        .query_row(
            "SELECT state,error_code FROM requests WHERE id=?1",
            [&row.id],
            |record| Ok((record.get(0)?, record.get(1)?)),
        )
        .unwrap();
    assert_eq!((state.as_str(), error.as_str()), ("failed", "root_changed"));
    drop(db);
    assert!(registry.release(&key).unwrap());
    assert!(!old.has_active_resources());
    assert!(old.evidence_response().is_err());
    tokio::time::timeout(Duration::from_secs(2), async {
        while file_descriptors(&moved) != 0 || file_descriptors(&leader_path) != 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("old runtime retained root/leader descriptors after release");
}

#[tokio::test]
async fn root_loss_during_paused_reattach_h_waits_for_worker_then_releases() {
    let base = tempfile::tempdir().unwrap();
    let checkout = base.path().join("work");
    let moved = base.path().join("moved");
    fs::create_dir(&checkout).unwrap();
    fs::write(checkout.join("a.js"), "function beforeRelease() {}\n").unwrap();
    let id = identity(&checkout);
    let key = id.root_key.clone();
    let topology = roots(base.path());
    let leader_path = topology.leader_lock(&id);
    let mut registry = CheckoutRegistry::with_roots(topology);
    registry.attach_launch(1, &id).unwrap();
    let original = registry.activate(&key).unwrap();
    ready(&original).await;
    registry.disconnect(1);
    assert!(registry.release(&key).unwrap());
    registry.attach_launch(2, &id).unwrap();
    drop(id);
    let paused = registry.activate(&key).unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
    let resume_rx = std::sync::Mutex::new(resume_rx);
    paused.set_pre_h_hook_for_tests(std::sync::Arc::new(move || {
        entered_tx.send(()).unwrap();
        resume_rx.lock().unwrap().recv().unwrap();
    }));
    tokio::time::timeout(Duration::from_secs(5), async {
        while entered_rx.try_recv().is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("idle reattach did not elect before H pause");
    fs::rename(&checkout, &moved).unwrap();
    fs::create_dir(&checkout).unwrap();
    fs::write(checkout.join("a.js"), "function replacement() {}\n").unwrap();
    registry.disconnect(2);
    assert!(
        paused.catching_up(),
        "paused H is pending despite an empty old-root queue"
    );
    assert!(
        !registry.release(&key).unwrap(),
        "live H owner must block idle release"
    );
    assert!(paused.has_active_resources());
    assert!(file_descriptors(&moved) > 0);
    assert!(file_descriptors(&leader_path) > 0);
    resume_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !registry.release(&key).unwrap() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("root-lost H did not retire and release");
    assert!(!paused.has_active_resources());
    assert!(paused.evidence_response().is_err());
    tokio::time::timeout(Duration::from_secs(2), async {
        while file_descriptors(&moved) != 0 || file_descriptors(&leader_path) != 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("paused H leaked old-root or leader descriptors");
}

#[tokio::test]
async fn cold_activation_preserves_recorded_scip_option() {
    let base = tempfile::tempdir().unwrap();
    let checkout = base.path().join("work");
    fs::create_dir(&checkout).unwrap();
    fs::write(
        checkout.join("a.js"),
        "function recordedScip() {}
",
    )
    .unwrap();
    let id = identity(&checkout);
    let topology = roots(base.path());
    let store = Store::open(topology.clone(), identity(&checkout)).unwrap();
    let mut options = baleyg::indexer::IndexOptions::new(checkout.clone());
    options.scip_path = Some(checkout.join("recorded.scip"));
    options.max_file_bytes = 8192;
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let owner =
        baleyg::index_coordinator::establish_serving_session(&store, Some(&options), &cancel)
            .unwrap();
    assert!(owner.is_leader());
    drop(owner);
    drop(store);
    let mut registry = CheckoutRegistry::with_roots(topology.clone());
    registry.attach_launch(1, &id).unwrap();
    let runtime = registry.activate(&id.root_key).unwrap();
    ready(&runtime).await;
    let observed = Store::open(topology.clone(), identity(&checkout))
        .unwrap()
        .recorded_index_options()
        .unwrap()
        .unwrap();
    assert_eq!(observed.scip_path, options.scip_path);
    assert_eq!(observed.max_file_bytes, options.max_file_bytes);

    registry.disconnect(1);
    assert!(registry.release(&id.root_key).unwrap());
    registry
        .register(
            &id,
            CheckoutOptions(serde_json::json!({"maxFileBytes":4096})),
        )
        .unwrap();
    registry.attach_launch(2, &id).unwrap();
    let explicit = registry.activate(&id.root_key).unwrap();
    ready(&explicit).await;
    let selected = Store::open(topology, identity(&checkout))
        .unwrap()
        .recorded_index_options()
        .unwrap()
        .unwrap();
    assert_eq!(selected.max_file_bytes, 4096);
    assert!(
        selected.scip_path.is_none(),
        "explicit browser options override recorded inputs"
    );
}

#[tokio::test]
async fn failed_h_backs_off_and_empty_idle_checkout_can_release() {
    let base = tempfile::tempdir().unwrap();
    let checkout = base.path().join("work");
    fs::create_dir(&checkout).unwrap();
    let id = identity(&checkout);
    let mut registry = CheckoutRegistry::with_roots(roots(base.path()));
    registry.attach_launch(1, &id).unwrap();
    let runtime = registry.activate(&id.root_key).unwrap();
    runtime.set_pre_h_hook_for_tests(std::sync::Arc::new(|| panic!("injected H failure")));
    tokio::time::timeout(Duration::from_secs(5), async {
        while runtime.reconciliation_error().is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    registry.disconnect(1);
    tokio::time::timeout(Duration::from_millis(180), async {
        while !registry.release(&id.root_key).unwrap() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("idle failed H may release during backoff");
    assert!(!runtime.has_active_resources());
}

#[tokio::test]
async fn failed_h_backoff_never_releases_accepted_fifo_work() {
    let base = tempfile::tempdir().unwrap();
    let checkout = base.path().join("work");
    fs::create_dir(&checkout).unwrap();
    let id = identity(&checkout);
    let topology = roots(base.path());
    let store = Store::open(topology.clone(), identity(&checkout)).unwrap();
    let queued = store
        .enqueue_request(&baleyg::indexer::IndexOptions::new(checkout), None)
        .unwrap();
    let mut registry = CheckoutRegistry::with_roots(topology);
    registry.attach_launch(1, &id).unwrap();
    let runtime = registry.activate(&id.root_key).unwrap();
    runtime.set_pre_h_hook_for_tests(std::sync::Arc::new(|| panic!("injected queued H failure")));
    tokio::time::timeout(Duration::from_secs(5), async {
        while runtime.reconciliation_error().is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    registry.disconnect(1);
    assert!(!registry.release(&id.root_key).unwrap());
    assert!(runtime.has_active_resources());
    assert_eq!(
        store.request_by_id(&queued.id).unwrap().unwrap().state,
        "queued"
    );
}

#[tokio::test]
async fn replaced_path_cold_runtime_publishes_new_identity_not_old_head() {
    let base = tempfile::tempdir().unwrap();
    let checkout = base.path().join("work");
    let moved = base.path().join("old");
    fs::create_dir(&checkout).unwrap();
    fs::write(
        checkout.join("a.js"),
        "function priorIdentity() {}
",
    )
    .unwrap();
    let old_id = identity(&checkout);
    let topology = roots(base.path());
    let mut registry = CheckoutRegistry::with_roots(topology);
    registry.attach_launch(1, &old_id).unwrap();
    let prior = registry.activate(&old_id.root_key).unwrap();
    ready(&prior).await;
    registry.disconnect(1);
    assert!(registry.release(&old_id.root_key).unwrap());
    fs::rename(&checkout, &moved).unwrap();
    fs::create_dir(&checkout).unwrap();
    fs::write(
        checkout.join("a.js"),
        "function newIdentity() {}
",
    )
    .unwrap();
    let next = identity(&checkout);
    registry.attach_launch(2, &next).unwrap();
    let current = registry.activate(&next.root_key).unwrap();
    ready(&current).await;
    let (answer, _) = current.evidence_response().unwrap();
    let (_, source) = answer.source_at("a.js", None).unwrap().unwrap();
    assert!(source.text.contains("newIdentity"));
    assert!(!source.text.contains("priorIdentity"));
    answer.finish(()).unwrap();
    assert!(prior.evidence_response().is_err());
}
