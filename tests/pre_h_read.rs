mod common;
use baleyg::{
    indexer::{self, IndexOptions},
    model::CancelFlag,
    store::Store,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

fn fixture() -> (tempfile::TempDir, tempfile::TempDir, Store) {
    let state = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let work = tempfile::tempdir().unwrap();
    let store = Store::open_for_tests(state.path(), work.path()).unwrap();
    (state, work, store)
}

#[test]
fn idle_reattach_admits_only_validated_predecessor_and_fences_epoch() {
    let (_state, work, store) = fixture();
    std::fs::write(work.path().join("a.js"), "function oldHead() {}\n").unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let (graph, native, capture) = indexer::index_workspace_bundle(
        &IndexOptions::new(work.path().to_owned()),
        store.root_id(),
        &cancel,
        |_| {},
    )
    .unwrap();
    let leader = store.leader().unwrap();
    let first = store
        .publish_native(
            &graph,
            &capture,
            &native,
            &leader,
            store.index_baseline().unwrap(),
            &cancel,
        )
        .unwrap();
    let ordinary = store.evidence_response().unwrap();
    let epoch = Arc::new(AtomicU64::new(1));
    let permit = store.pre_h_read_permit(&ordinary, epoch.clone()).unwrap();
    ordinary.finish(()).unwrap();
    drop(ordinary);
    drop(leader);

    let successor = store.leader_for_idle_reattach(&permit).unwrap();
    assert!(
        store.evidence_response().is_err(),
        "ordinary admission stays strict"
    );
    let admission = store.evidence_response_pre_h(&permit, successor.clone());
    assert!(
        admission.is_ok(),
        "matching validated predecessor must admit a read: {:?}",
        admission.as_ref().err().map(ToString::to_string)
    );
    let admitted = admission.ok().unwrap();
    assert_eq!(admitted.status().unwrap().revision, first);
    admitted.validate_pin(first).unwrap();
    admitted.finish(()).unwrap();
    epoch.fetch_add(1, Ordering::AcqRel);
    assert!(
        admitted
            .finish(())
            .unwrap_err()
            .to_string()
            .contains("checkout epoch changed")
    );
    assert!(
        store
            .evidence_response_pre_h(&permit, successor)
            .err()
            .unwrap()
            .to_string()
            .contains("checkout epoch changed")
    );
}

#[test]
fn cold_head_cannot_mint_a_permit() {
    let (_state, _work, store) = fixture();
    assert!(store.evidence_response().is_err());
}

#[test]
fn publication_ends_new_pre_h_admissions_and_resumes_strict_reads() {
    let (_state, work, store) = fixture();
    std::fs::write(work.path().join("a.js"), "function oldHead() {}\n").unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let (graph, native, capture) = indexer::index_workspace_bundle(
        &IndexOptions::new(work.path().to_owned()),
        store.root_id(),
        &cancel,
        |_| {},
    )
    .unwrap();
    let leader = store.leader().unwrap();
    let first = store
        .publish_native(
            &graph,
            &capture,
            &native,
            &leader,
            store.index_baseline().unwrap(),
            &cancel,
        )
        .unwrap();
    let response = store.evidence_response().unwrap();
    let permit = store
        .pre_h_read_permit(&response, Arc::new(AtomicU64::new(5)))
        .unwrap();
    drop(response);
    drop(leader);
    let successor = store.leader_for_idle_reattach(&permit).unwrap();
    let admitted = store
        .evidence_response_pre_h(&permit, successor.clone())
        .unwrap();
    admitted.finish(()).unwrap();
    drop(admitted);
    let second = store
        .publish_native(
            &graph,
            &capture,
            &native,
            successor.leader_guard().unwrap(),
            first,
            &cancel,
        )
        .unwrap();
    assert!(second.index_revision > first.index_revision);
    assert!(
        store
            .evidence_response_pre_h(&permit, successor.clone())
            .is_err(),
        "published H must close predecessor admission"
    );
    let strict = store.evidence_response().unwrap();
    assert_eq!(strict.status().unwrap().revision, second);
    strict.finish(()).unwrap();
}

#[test]
fn intervening_leader_incarnation_cannot_reuse_an_idle_permit() {
    let (_state, work, store) = fixture();
    std::fs::write(work.path().join("a.js"), "function oldHead() {}\n").unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let (graph, native, capture) = indexer::index_workspace_bundle(
        &IndexOptions::new(work.path().to_owned()),
        store.root_id(),
        &cancel,
        |_| {},
    )
    .unwrap();
    let leader = store.leader().unwrap();
    store
        .publish_native(
            &graph,
            &capture,
            &native,
            &leader,
            store.index_baseline().unwrap(),
            &cancel,
        )
        .unwrap();
    let response = store.evidence_response().unwrap();
    let permit = store
        .pre_h_read_permit(&response, Arc::new(AtomicU64::new(1)))
        .unwrap();
    drop(response);
    drop(leader);
    let intervening = store.leader().unwrap();
    drop(intervening);
    assert!(
        store
            .leader_for_idle_reattach(&permit)
            .err()
            .unwrap()
            .to_string()
            .contains("intervening leader")
    );
}
