mod common;
use baleyg::{
    indexer::{self, IndexOptions},
    model::CancelFlag,
    store::{EvidenceFencePolicy, Store},
};
use std::sync::{Arc, atomic::AtomicBool};

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
fn no_published_head_remains_not_ready() {
    let (_state, _work, store) = fixture();
    assert!(store.evidence_response().is_err());
}

#[test]
fn committed_head_reads_during_fresh_successor_metadata_pause_without_prior_runtime_permit() {
    use std::{
        sync::{Mutex, mpsc},
        time::Duration,
    };
    let (_state, work, store) = fixture();
    std::fs::write(work.path().join("a.js"), "function prior() {}\n").unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let (graph, native, capture) = indexer::index_workspace_bundle(
        &IndexOptions::new(work.path().to_owned()),
        store.root_id(),
        &cancel,
        |_| {},
    )
    .unwrap();
    let prior_owner = store.leader_session().unwrap();
    let first = store
        .publish_native(
            &graph,
            &capture,
            &native,
            prior_owner.leader_guard().unwrap(),
            store.index_baseline().unwrap(),
            &cancel,
        )
        .unwrap();
    drop(prior_owner);
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = mpsc::sync_channel(1);
    let resume_rx = Mutex::new(resume_rx);
    store.set_leader_before_metadata_hook_for_tests(move || {
        entered_tx.send(()).unwrap();
        resume_rx.lock().unwrap().recv().unwrap();
    });
    let takeover_store = store.clone();
    let takeover = std::thread::spawn(move || takeover_store.leader_session());
    entered_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("successor did not acquire EX");
    let result = store.evidence_response();
    let finish = result.as_ref().map(|response| {
        assert_eq!(response.status().unwrap().revision, first);
        response.finish(()).unwrap();
    });
    let admission_ok = finish.is_ok();
    let admission_error = result.as_ref().err().map(|error| format!("{error:#}"));
    drop(result);
    resume_tx.send(()).unwrap();
    let successor = takeover.join().unwrap().unwrap();
    assert!(
        admission_ok,
        "validated published A must remain readable before H: {admission_error:?}"
    );
    assert!(
        store.claim_request(&successor).is_err(),
        "pre-H EX must never claim FIFO"
    );
}

#[test]
fn sidecar_writes_refuse_successor_pre_marker_while_prior_head_reads() {
    use baleyg::{
        index_coordinator::reconcile_workspace,
        model::{Annotation, SavedView, ViewQuery},
        store::topology::{TopologyRoots, WorkspaceIdentity},
    };
    use std::{
        collections::BTreeMap,
        sync::{Mutex, mpsc},
        time::Duration,
    };
    let (state, work, store) = fixture();
    std::fs::write(work.path().join("a.js"), "function current() {}\n").unwrap();
    let options = IndexOptions::new(work.path().to_owned());
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let (graph, native, capture) =
        indexer::index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap();
    let view = SavedView {
        id: "gate-view".into(),
        title: "Gate".into(),
        query: ViewQuery {
            seed: native.declarations[0].syntax_id.clone(),
            depth: 1,
            max_nodes: 40,
            max_calls: 200,
            include_callbacks: false,
            exclude_paths: vec![],
        },
        pins: BTreeMap::new(),
        hidden: vec![],
    };
    let annotation = Annotation {
        id: "gate-note".into(),
        node_id: "node".into(),
        body: "note".into(),
    };
    let owner = store.leader_session().unwrap();
    let first = store
        .publish_native(
            &graph,
            &capture,
            &native,
            owner.leader_guard().unwrap(),
            store.index_baseline().unwrap(),
            &cancel,
        )
        .unwrap();
    drop(owner);
    let roots =
        TopologyRoots::isolated_for_tests(state.path().join("cache"), state.path().join("data"));
    let identity = WorkspaceIdentity::discover(Some(work.path()), work.path())
        .unwrap()
        .attach_marker()
        .unwrap();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = mpsc::sync_channel(1);
    let resume_rx = Mutex::new(resume_rx);
    let takeover = std::thread::spawn(move || {
        roots.leader_with_hooks(
            &identity,
            || {
                entered_tx.send(()).unwrap();
                resume_rx.lock().unwrap().recv().unwrap();
                Ok(())
            },
            || Ok(()),
        )
    });
    entered_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("successor never acquired EX");
    let observed = store.evidence_response().unwrap();
    assert_eq!(observed.status().unwrap().revision, first);
    assert!(
        observed.require_mutation_ready().is_err(),
        "read A never authorizes premarker write"
    );
    assert!(
        store.put_annotation(&annotation).is_err(),
        "raw sidecar put must respect gate"
    );
    assert!(
        store.delete_annotation(&annotation.id).is_err(),
        "sidecar delete must respect gate"
    );
    drop(observed);
    resume_tx.send(()).unwrap();
    drop(takeover.join().unwrap().unwrap());
    let (second, reconciled) = reconcile_workspace(&store, &options, &cancel, |_| {}).unwrap();
    assert!(second.index_revision > first.index_revision);
    assert!(
        store.save_view_at(second, &view).is_ok(),
        "fresh current H allows sidecar save"
    );
    store.put_annotation(&annotation).unwrap();
    assert!(store.delete_annotation(&annotation.id).unwrap());
    drop(reconciled);
}

#[test]
fn admitted_exact_a_finishes_after_same_owner_b_publication_without_sqlite_reopen() {
    let (_state, work, store) = fixture();
    let source = work.path().join("a.js");
    std::fs::write(&source, "function versionA() {}\n").unwrap();
    let options = IndexOptions::new(work.path().to_owned());
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let leader = store.leader_session().unwrap();
    let (graph, native, capture) =
        indexer::index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap();
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
    assert_eq!(response.status().unwrap().revision, first);
    response.validate_pin(first).unwrap();
    let fence = response.into_fence(EvidenceFencePolicy::ExactPin(first));
    std::fs::write(&source, "function versionB() {}\n").unwrap();
    let (graph, native, capture) =
        indexer::index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap();
    let second = store
        .publish_native(
            &graph,
            &capture,
            &native,
            leader.leader_guard().unwrap(),
            first,
            &cancel,
        )
        .unwrap();
    assert!(second.index_revision > first.index_revision);
    fence
        .finish(())
        .expect("immutable admitted A remains valid after B");
}

#[test]
fn admitted_head_fails_closed_when_captured_root_path_changes() {
    let (state, work, store) = fixture();
    let source = work.path().join("a.js");
    std::fs::write(&source, "function oldRoot() {}\n").unwrap();
    let options = IndexOptions::new(work.path().to_owned());
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let (graph, native, capture) =
        indexer::index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap();
    let owner = store.leader_session().unwrap();
    let pin = store
        .publish_native(
            &graph,
            &capture,
            &native,
            owner.leader_guard().unwrap(),
            store.index_baseline().unwrap(),
            &cancel,
        )
        .unwrap();
    let admitted = store.evidence_response().unwrap();
    assert_eq!(admitted.status().unwrap().revision, pin);
    let moved = state.path().join("old-root");
    std::fs::rename(work.path(), &moved).unwrap();
    std::fs::create_dir(work.path()).unwrap();
    assert!(
        admitted
            .finish(())
            .unwrap_err()
            .to_string()
            .contains("root_changed")
    );
    assert!(
        store.evidence_response().is_err(),
        "replacement root cannot borrow old head"
    );
}
