mod common;
use baleyg::{
    indexer::{self, IndexOptions},
    model::CancelFlag,
    store::{EvidenceFencePolicy, PreHReadPermit, Store},
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
    let leader = store.leader_session().unwrap();
    let first = store
        .publish_native(
            &graph,
            &capture,
            &native,
            leader.leader_guard().unwrap(),
            store.index_baseline().unwrap(),
            &cancel,
        )
        .unwrap();
    let ordinary = store.evidence_response().unwrap();
    let epoch = Arc::new(AtomicU64::new(1));
    let permit = store
        .pre_h_read_permit(&ordinary, &leader, epoch.clone())
        .unwrap();
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
    let leader = store.leader_session().unwrap();
    let first = store
        .publish_native(
            &graph,
            &capture,
            &native,
            leader.leader_guard().unwrap(),
            store.index_baseline().unwrap(),
            &cancel,
        )
        .unwrap();
    let response = store.evidence_response().unwrap();
    let permit = store
        .pre_h_read_permit(&response, &leader, Arc::new(AtomicU64::new(5)))
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
    let leader = store.leader_session().unwrap();
    store
        .publish_native(
            &graph,
            &capture,
            &native,
            leader.leader_guard().unwrap(),
            store.index_baseline().unwrap(),
            &cancel,
        )
        .unwrap();
    let response = store.evidence_response().unwrap();
    let permit = store
        .pre_h_read_permit(&response, &leader, Arc::new(AtomicU64::new(1)))
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

fn released_head() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    Store,
    PreHReadPermit,
    baleyg::model::IndexPin,
) {
    let (state, work, store) = fixture();
    std::fs::write(work.path().join("a.js"), "function oldHead() {}\n").unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let (graph, native, capture) = indexer::index_workspace_bundle(
        &IndexOptions::new(work.path().to_owned()),
        store.root_id(),
        &cancel,
        |_| {},
    )
    .unwrap();
    let leader = store.leader_session().unwrap();
    let pin = store
        .publish_native(
            &graph,
            &capture,
            &native,
            leader.leader_guard().unwrap(),
            store.index_baseline().unwrap(),
            &cancel,
        )
        .unwrap();
    let response = store.evidence_response().unwrap();
    let permit = store
        .pre_h_read_permit(&response, &leader, Arc::new(AtomicU64::new(2)))
        .unwrap();
    drop(response);
    drop(leader);
    (state, work, store, permit, pin)
}

fn index_db(state: &std::path::Path) -> std::path::PathBuf {
    std::fs::read_dir(state.join("cache/indexes"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
        .join("index.db")
}

#[test]
fn standalone_follower_cannot_mint_despite_a_valid_strict_read() {
    let (state, work, store) = fixture();
    std::fs::write(work.path().join("a.js"), "function oldHead() {}\n").unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let (graph, native, capture) = indexer::index_workspace_bundle(
        &IndexOptions::new(work.path().to_owned()),
        store.root_id(),
        &cancel,
        |_| {},
    )
    .unwrap();
    let standalone = store.leader_session().unwrap();
    let pin = store
        .publish_native(
            &graph,
            &capture,
            &native,
            standalone.leader_guard().unwrap(),
            store.index_baseline().unwrap(),
            &cancel,
        )
        .unwrap();
    let daemon_follower = Store::open_for_tests(state.path(), work.path()).unwrap();
    let response = daemon_follower.evidence_response().unwrap();
    assert_eq!(response.status().unwrap().revision, pin);
    response.finish(()).unwrap();
    let follower_session = daemon_follower.follower_session().unwrap();
    let refusal = daemon_follower
        .pre_h_read_permit(&response, &follower_session, Arc::new(AtomicU64::new(1)))
        .err()
        .expect("a follower cannot mint a daemon idle permit");
    assert!(
        refusal
            .to_string()
            .contains("storage_busy: follower cannot publish"),
        "{refusal:#}"
    );
}

#[test]
fn pre_h_denies_claim_and_recovery_closes_admitted_fence() {
    let (state, _work, store, permit, pin) = released_head();
    let successor = store.leader_for_idle_reattach(&permit).unwrap();
    let denied = store.claim_request(&successor).unwrap_err();
    assert!(
        denied
            .to_string()
            .contains("index_not_ready: mandatory leader reconciliation not committed"),
        "{denied:#}"
    );
    let response = store.evidence_response_pre_h(&permit, successor).unwrap();
    assert_eq!(response.status().unwrap().revision, pin);
    let fence = response.into_fence(EvidenceFencePolicy::T03);
    let db = rusqlite::Connection::open(index_db(state.path())).unwrap();
    db.pragma_update(None, "user_version", 7).unwrap();
    drop(db);
    let recovery = store.status().unwrap_err();
    assert!(
        recovery.to_string().contains("incompatible_index"),
        "{recovery:#}"
    );
    let refusal = fence.finish(pin).unwrap_err();
    assert!(
        refusal
            .to_string()
            .contains("store_unavailable: pre-H index recovery pending"),
        "{refusal:#}"
    );
}

#[test]
fn pre_h_root_schema_and_revision_mismatch_are_refused() {
    // Each case starts with a separately validated predecessor, not a fabricated permit.
    let (state, _work, store, permit, _) = released_head();
    let db = rusqlite::Connection::open(index_db(state.path())).unwrap();
    db.pragma_update(None, "user_version", 7).unwrap();
    drop(db);
    let successor = store.leader_for_idle_reattach(&permit).unwrap();
    let error = store
        .evidence_response_pre_h(&permit, successor)
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains("store_unavailable: pre-H index recovery pending"),
        "{error:#}"
    );

    let (state, _work, store, permit, _) = released_head();
    let db = rusqlite::Connection::open(index_db(state.path())).unwrap();
    db.execute(
        "UPDATE index_metadata SET index_revision=index_revision+1 WHERE singleton=1",
        [],
    )
    .unwrap();
    drop(db);
    let successor = store.leader_for_idle_reattach(&permit).unwrap();
    let error = store
        .evidence_response_pre_h(&permit, successor)
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains("store_unavailable: pre-H index recovery pending"),
        "{error:#}"
    );

    let (state, _work, store, permit, _) = released_head();
    let db = rusqlite::Connection::open(index_db(state.path())).unwrap();
    db.execute(
        "UPDATE index_metadata SET index_generation=?1 WHERE singleton=1",
        [uuid::Uuid::new_v4().to_string()],
    )
    .unwrap();
    drop(db);
    let successor = store.leader_for_idle_reattach(&permit).unwrap();
    let error = store
        .evidence_response_pre_h(&permit, successor)
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains("store_unavailable: pre-H index recovery pending"),
        "{error:#}"
    );

    let (_state, work, store, permit, _) = released_head();
    let moved = work.path().with_extension("moved");
    std::fs::rename(work.path(), &moved).unwrap();
    let error = store.leader_for_idle_reattach(&permit).err().unwrap();
    assert!(error.to_string().contains("root_changed"), "{error:#}");
}

#[test]
fn pre_h_lock_path_loss_refuses_final_result() {
    let (state, _work, store, permit, _) = released_head();
    let successor = store.leader_for_idle_reattach(&permit).unwrap();
    let admitted = store.evidence_response_pre_h(&permit, successor).unwrap();
    let index_dir = index_db(state.path()).parent().unwrap().to_path_buf();
    std::fs::rename(
        index_dir.join("leader.lock"),
        index_dir.join("leader.displaced"),
    )
    .unwrap();
    let error = admitted.finish(()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("store_unavailable: pre-H leader lock unavailable"),
        "{error:#}"
    );
}

#[test]
fn admitted_old_basis_finishes_after_h_publishes() {
    let (_state, work, store, permit, first) = released_head();
    let successor = store.leader_for_idle_reattach(&permit).unwrap();
    let response = store
        .evidence_response_pre_h(&permit, successor.clone())
        .unwrap();
    let selected = response.status().unwrap().revision;
    assert_eq!(selected, first);
    let (selected_source_pin, old_source) = response.source_at("a.js", None).unwrap().unwrap();
    assert_eq!(selected_source_pin, first);
    assert!(old_source.text.contains("oldHead"));
    let fence = response.into_fence(EvidenceFencePolicy::T03);
    std::fs::write(work.path().join("a.js"), "function newHead() {}\n").unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let (graph, native, capture) = indexer::index_workspace_bundle(
        &IndexOptions::new(work.path().to_owned()),
        store.root_id(),
        &cancel,
        |_| {},
    )
    .unwrap();
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
    assert_eq!(
        fence.finish(selected).unwrap(),
        first,
        "coherent predecessor result remains finishable"
    );
    let current = store.evidence_response().unwrap();
    assert_eq!(current.status().unwrap().revision, second);
    let (_, new_source) = current.source_at("a.js", None).unwrap().unwrap();
    assert!(new_source.text.contains("newHead"));
    current.finish(()).unwrap();
}
