use baleyg::{
    daemon::registry::{
        BROWSER_IDLE_DELAY, CHECKOUT_RELEASE_DELAY, CheckoutOptions, CheckoutRegistry,
        DAEMON_IDLE_DELAY,
    },
    store::topology::{TopologyRoots, WorkspaceIdentity},
};
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

fn identity(root: &Path) -> WorkspaceIdentity {
    WorkspaceIdentity::discover(Some(root), root)
        .unwrap()
        .attach_marker()
        .unwrap()
}
fn fixture() -> (
    tempfile::TempDir,
    WorkspaceIdentity,
    CheckoutRegistry,
    Instant,
) {
    let base = tempfile::tempdir().unwrap();
    let checkout = base.path().join("work");
    fs::create_dir(&checkout).unwrap();
    fs::write(checkout.join("a.js"), "function original() {}\n").unwrap();
    let id = identity(&checkout);
    let now = Instant::now();
    let roots =
        TopologyRoots::isolated_for_tests(base.path().join("cache"), base.path().join("data"));
    let mut registry = CheckoutRegistry::with_roots_at(roots, now);
    registry
        .register(&id, CheckoutOptions(serde_json::json!({})))
        .unwrap();
    (base, id, registry, now)
}
async fn ready(runtime: &baleyg::daemon::registry::CheckoutRuntime) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Ok((response, catching_up)) = runtime.evidence_response() {
                let finished = response.finish(()).is_ok();
                if finished && !catching_up {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn lifecycle_driver_exits_when_startup_deadline_is_due() {
    let registry = std::sync::Arc::new(tokio::sync::Mutex::new(CheckoutRegistry::starting_at(
        Instant::now() - DAEMON_IDLE_DELAY,
    )));
    tokio::time::timeout(
        Duration::from_secs(1),
        baleyg::daemon::run_idle_lifecycle(registry),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn browser_expiry_starts_a_separate_release_grace_and_exit_clock() {
    let (_base, id, mut registry, now) = fixture();
    registry.browser_request_at(&id, now).unwrap();
    let runtime = registry.activate(&id.root_key).unwrap();
    ready(&runtime).await;
    let epsilon = Duration::from_nanos(1);
    assert!(
        !registry
            .advance(now + BROWSER_IDLE_DELAY - epsilon)
            .unwrap()
            .exit
    );
    assert_eq!(registry.idle_exit_deadline(), None);
    assert!(runtime.has_active_resources());
    registry.advance(now + BROWSER_IDLE_DELAY).unwrap();
    assert_eq!(
        registry.idle_exit_deadline(),
        Some(now + BROWSER_IDLE_DELAY + DAEMON_IDLE_DELAY)
    );
    assert!(runtime.has_active_resources());
    assert!(
        registry
            .advance(now + BROWSER_IDLE_DELAY + CHECKOUT_RELEASE_DELAY - epsilon)
            .unwrap()
            .released
            .is_empty()
    );
    assert!(
        registry
            .advance(now + BROWSER_IDLE_DELAY + CHECKOUT_RELEASE_DELAY)
            .unwrap()
            .released
            .contains(&id.root_key)
    );
    assert!(!runtime.has_active_resources());
    assert!(
        !registry
            .advance(now + BROWSER_IDLE_DELAY + DAEMON_IDLE_DELAY - epsilon)
            .unwrap()
            .exit
    );
    assert!(
        registry
            .advance(now + BROWSER_IDLE_DELAY + DAEMON_IDLE_DELAY)
            .unwrap()
            .exit
    );
}

#[tokio::test]
async fn browser_client_cannot_collide_with_session_and_late_tick_keeps_deadlines() {
    let (_base, id, mut registry, now) = fixture();
    registry.attach_launch(u64::MAX, &id).unwrap();
    registry.browser_request_at(&id, now).unwrap();
    let runtime = registry.activate(&id.root_key).unwrap();
    ready(&runtime).await;
    registry.disconnect_at(u64::MAX, now);
    registry
        .advance(now + BROWSER_IDLE_DELAY - Duration::from_nanos(1))
        .unwrap();
    assert_eq!(registry.idle_exit_deadline(), None);
    let late = now + BROWSER_IDLE_DELAY + DAEMON_IDLE_DELAY + Duration::from_secs(1);
    let tick = registry.advance(late).unwrap();
    assert_eq!(tick.released, vec![id.root_key.clone()]);
    assert!(tick.exit);
    assert_eq!(
        registry.idle_exit_deadline(),
        Some(now + BROWSER_IDLE_DELAY + DAEMON_IDLE_DELAY)
    );
}

#[tokio::test]
async fn final_mcp_disconnect_after_unpolled_browser_expiry_starts_its_own_grace() {
    let (_base, id, mut registry, now) = fixture();
    registry.attach_launch(1, &id).unwrap();
    registry.browser_request_at(&id, now).unwrap();
    let runtime = registry.activate(&id.root_key).unwrap();
    ready(&runtime).await;
    let disconnected = now + BROWSER_IDLE_DELAY + Duration::from_secs(5);
    registry.disconnect_at(1, disconnected);
    assert_eq!(
        registry.idle_exit_deadline(),
        Some(disconnected + DAEMON_IDLE_DELAY)
    );
    registry.disconnect_at(99, disconnected + Duration::from_secs(3));
    assert_eq!(
        registry.idle_exit_deadline(),
        Some(disconnected + DAEMON_IDLE_DELAY)
    );
    assert!(
        registry
            .advance(disconnected + CHECKOUT_RELEASE_DELAY - Duration::from_nanos(1))
            .unwrap()
            .released
            .is_empty()
    );
    assert_eq!(
        registry
            .advance(disconnected + CHECKOUT_RELEASE_DELAY)
            .unwrap()
            .released,
        vec![id.root_key.clone()]
    );
}

#[tokio::test]
async fn selected_browser_request_renews_virtual_client() {
    let (_base, id, mut registry, now) = fixture();
    registry.browser_request_at(&id, now).unwrap();
    let runtime = registry.activate(&id.root_key).unwrap();
    ready(&runtime).await;
    let next = now + BROWSER_IDLE_DELAY - Duration::from_nanos(1);
    registry.browser_request_at(&id, next).unwrap();
    registry.advance(now + BROWSER_IDLE_DELAY).unwrap();
    assert_eq!(registry.idle_exit_deadline(), None);
    assert!(runtime.has_active_resources());
    registry.advance(next + BROWSER_IDLE_DELAY).unwrap();
    assert_eq!(
        registry.idle_exit_deadline(),
        Some(next + BROWSER_IDLE_DELAY + DAEMON_IDLE_DELAY)
    );
}

#[tokio::test]
async fn final_session_disconnect_and_later_attach_reset_deadlines() {
    let (_base, id, mut registry, now) = fixture();
    registry.attach_launch(1, &id).unwrap();
    registry.attach_launch(2, &id).unwrap();
    let runtime = registry.activate(&id.root_key).unwrap();
    ready(&runtime).await;
    registry.disconnect_at(1, now);
    assert_eq!(registry.idle_exit_deadline(), None);
    registry.disconnect_at(2, now + Duration::from_secs(4));
    assert_eq!(
        registry.idle_exit_deadline(),
        Some(now + Duration::from_secs(4) + DAEMON_IDLE_DELAY)
    );
    let before = now + Duration::from_secs(4) + CHECKOUT_RELEASE_DELAY;
    assert!(
        registry
            .advance(before - Duration::from_nanos(1))
            .unwrap()
            .released
            .is_empty()
    );
    registry.attach_launch(3, &id).unwrap();
    assert_eq!(registry.idle_exit_deadline(), None);
    assert!(registry.advance(before).unwrap().released.is_empty());
    registry.disconnect_at(3, before);
    assert!(
        registry
            .advance(before + CHECKOUT_RELEASE_DELAY)
            .unwrap()
            .released
            .contains(&id.root_key)
    );
}

#[test]
fn registration_and_selector_free_traffic_do_not_postpone_startup_exit() {
    let (_base, _id, mut registry, now) = fixture();
    assert!(
        !registry
            .advance(now + DAEMON_IDLE_DELAY - Duration::from_nanos(1))
            .unwrap()
            .exit
    );
    assert!(registry.advance(now + DAEMON_IDLE_DELAY).unwrap().exit);
}

#[tokio::test]
async fn queued_or_inflight_work_defers_expired_deadlines_until_drain() {
    let (_base, id, mut registry, now) = fixture();
    registry.attach_launch(1, &id).unwrap();
    let runtime = registry.activate(&id.root_key).unwrap();
    ready(&runtime).await;
    registry.disconnect_at(1, now);
    registry.set_pending_work(&id.root_key, true).unwrap();
    let elapsed = now + DAEMON_IDLE_DELAY;
    // A real in-flight response keeps the owner alive even after both clocks expire.
    let (response, _) = runtime.evidence_response().unwrap();
    assert!(!registry.advance(elapsed).unwrap().exit);
    assert!(runtime.has_active_resources());
    response.finish(()).unwrap();
    drop(response);
    registry.set_pending_work(&id.root_key, false).unwrap();
    let tick = registry.advance(elapsed).unwrap();
    assert_eq!(tick.released, vec![id.root_key.clone()]);
    assert!(tick.exit);
}

#[test]
fn startup_orphan_queue_blocks_idle_exit_without_any_client() {
    let base = tempfile::tempdir().unwrap();
    let checkout = base.path().join("work");
    fs::create_dir(&checkout).unwrap();
    fs::write(checkout.join("a.js"), "function queued() {}\n").unwrap();
    let id = identity(&checkout);
    let roots =
        TopologyRoots::isolated_for_tests(base.path().join("cache"), base.path().join("data"));
    let store = baleyg::store::Store::open(roots.clone(), id).unwrap();
    store
        .enqueue_request(&baleyg::indexer::IndexOptions::new(checkout), None)
        .unwrap();
    drop(store);
    let now = Instant::now();
    let mut restarted = CheckoutRegistry::with_roots_at(roots, now);
    assert!(!restarted.advance(now + DAEMON_IDLE_DELAY).unwrap().exit);
}

#[cfg(unix)]
fn open_descriptors(path: &Path) -> usize {
    use std::os::unix::fs::MetadataExt;
    let expected = fs::metadata(path).unwrap();
    (0..1024)
        .filter(|fd| {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe { libc::fstat(*fd, stat.as_mut_ptr()) } != 0 {
                return false;
            }
            let stat = unsafe { stat.assume_init() };
            #[cfg(target_os = "macos")]
            let dev = stat.st_dev as u64;
            #[cfg(not(target_os = "macos"))]
            let dev = stat.st_dev;
            (dev, stat.st_ino) == (expected.dev(), expected.ino())
        })
        .count()
}

#[tokio::test]
async fn timed_release_closes_checkout_handles_and_reattach_runs_h() {
    let (base, id, mut registry, now) = fixture();
    let root_baseline = open_descriptors(&id.root);
    let lock = base
        .path()
        .join("cache/indexes")
        .join(&id.root_key)
        .join("leader.lock");
    registry.attach_launch(1, &id).unwrap();
    let old = registry.activate(&id.root_key).unwrap();
    ready(&old).await;
    assert!(open_descriptors(&id.root) > root_baseline);
    assert!(open_descriptors(&lock) > 0);
    registry.disconnect_at(1, now);
    assert_eq!(
        registry
            .advance(now + CHECKOUT_RELEASE_DELAY)
            .unwrap()
            .released,
        vec![id.root_key.clone()]
    );
    assert!(!old.has_active_resources());
    tokio::time::timeout(Duration::from_secs(2), async {
        while open_descriptors(&id.root) != root_baseline || open_descriptors(&lock) != 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    fs::write(id.root.join("a.js"), "function changed() {}\n").unwrap();
    registry.attach_launch(2, &id).unwrap();
    let resumed = registry.activate(&id.root_key).unwrap();
    assert!(resumed.catching_up());
    ready(&resumed).await;
    assert!(open_descriptors(&lock) > 0);
    let (response, stale) = resumed.evidence_response().unwrap();
    assert!(!stale);
    let (_, source) = response.source_at("a.js", None).unwrap().unwrap();
    assert!(source.text.contains("changed"));
    response.finish(()).unwrap();
}
