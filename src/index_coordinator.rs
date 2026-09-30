//! Shared admission and publication boundary for CLI and authenticated daemon index jobs.
//! A job has one expected pair, one captured source set, and one complete paired commit.
use crate::{
    capture::Capture,
    indexer::{self, IndexOptions},
    model::{CancelFlag, IndexPin, IndexProgress},
    store::{RecoveryBaseline, Store, topology::LeaderSession},
};
use anyhow::{Result, ensure};
use std::sync::{Arc, atomic::Ordering};

pub struct IndexJobCoordinator {
    store: Store,
    expected: RecoveryBaseline,
    session: Arc<LeaderSession>,
}

impl IndexJobCoordinator {
    /// The control baseline admits known old indexes without exposing their evidence to readers.
    /// A supplied HTTP pair is checked before any source admission or worker is started.
    pub fn prepare(store: &Store, requested: Option<IndexPin>) -> Result<Self> {
        let expected = store.recovery_index_baseline()?;
        ensure!(
            requested.is_none_or(|pin| expected.pin() == Some(pin)),
            "revision conflict: prior index pin is not decodable or changed"
        );
        let session = store.leader_session()?;
        Self::prepare_with_session_and_baseline(store, requested, expected, session)
    }

    pub fn prepare_with_session(
        store: &Store,
        requested: Option<IndexPin>,
        session: Arc<LeaderSession>,
    ) -> Result<Self> {
        let expected = store.recovery_index_baseline()?;
        Self::prepare_with_session_and_baseline(store, requested, expected, session)
    }

    fn prepare_with_session_and_baseline(
        store: &Store,
        requested: Option<IndexPin>,
        expected: RecoveryBaseline,
        session: Arc<LeaderSession>,
    ) -> Result<Self> {
        ensure!(session.is_leader(), "storage_busy: follower cannot publish");
        store.verify_leader_session(&session)?;
        ensure!(
            requested.is_none_or(|pin| expected.pin() == Some(pin)),
            "revision conflict: prior index pin is not decodable or changed"
        );
        Ok(Self {
            store: store.clone(),
            expected,
            session,
        })
    }

    pub fn session(&self) -> Arc<LeaderSession> {
        self.session.clone()
    }

    /// Projection uses the admitted bytes; publication checks drift, cancellation and the
    /// whole expected pair under the writer lock before making graph and native rows visible.
    pub fn run(
        self,
        options: &IndexOptions,
        cancel: &CancelFlag,
        progress: impl Fn(IndexProgress) + Sync,
    ) -> Result<IndexPin> {
        self.run_observed(options, cancel, progress, |_| {})
    }

    /// `observe` sees the job's single capture after admission and before publication.
    pub(crate) fn run_observed(
        self,
        options: &IndexOptions,
        cancel: &CancelFlag,
        progress: impl Fn(IndexProgress) + Sync,
        observe: impl FnOnce(&Capture),
    ) -> Result<IndexPin> {
        ensure!(!cancel.load(Ordering::Acquire), "index cancelled");
        self.store.begin_leader_publication(&self.session)?;
        let (graph, native, capture) =
            indexer::index_workspace_bundle(options, self.store.root_id(), cancel, progress)?;
        observe(&capture);
        ensure!(!cancel.load(Ordering::Acquire), "index cancelled");
        self.store.verify_leader_session(&self.session)?;
        self.store.publish_native_recovery(
            &graph,
            &capture,
            &native,
            self.session.leader_guard()?,
            self.expected,
            cancel,
        )
    }
}

/// One explicit index command returns the owner that published its one full capture.
/// Exceptional recovery cannot take the live-baseline path or upgrade shared use.
pub fn reconcile_workspace(
    store: &Store,
    options: &IndexOptions,
    cancel: &CancelFlag,
    progress: impl Fn(IndexProgress) + Sync,
) -> Result<(IndexPin, Arc<LeaderSession>)> {
    if store.is_recreate_pending() {
        return store.recreate_pending_leader_session(options, cancel);
    }
    let coordinator = IndexJobCoordinator::prepare(store, None)?;
    let session = coordinator.session();
    let pin = coordinator.run(options, cancel, progress)?;
    session.verify()?;
    Ok((pin, session))
}

/// Establish one bounded serving owner. A free lock performs one complete
/// reconciliation; contention is admitted only as a verified follower.
pub fn establish_serving_session(
    store: &Store,
    explicit_options: Option<&IndexOptions>,
    cancel: &CancelFlag,
) -> Result<Arc<LeaderSession>> {
    if store.is_recreate_pending() {
        let options = explicit_options.ok_or_else(|| {
            anyhow::anyhow!(
                "recovery_required: index options unavailable; run explicit baleyg index"
            )
        })?;
        let (_, session) = store.recreate_pending_leader_session(options, cancel)?;
        session.verify()?;
        return Ok(session);
    }
    match store.leader_session() {
        Ok(session) => {
            let expected = store.recovery_index_baseline()?;
            let options = match explicit_options {
                Some(options) => options.clone(),
                None => store.recorded_index_options()?.unwrap_or_else(|| {
                    IndexOptions::new(std::path::PathBuf::from(store.workspace_root()))
                }),
            };
            let coordinator = IndexJobCoordinator::prepare_with_session_and_baseline(
                store,
                None,
                expected,
                session.clone(),
            )?;
            coordinator.run(&options, cancel, |_| {})?;
            session.verify()?;
            Ok(session)
        }
        Err(error) if format!("{error:#}").contains("storage_busy") => store.follower_session(),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::SourceOperations;
    use std::{
        collections::BTreeMap,
        fs,
        sync::{Arc, atomic::AtomicBool},
    };

    /// Per-source (opens, complete reads, hashes), keyed by path.
    type Counts = BTreeMap<String, (usize, usize, usize)>;

    fn counts(capture: &Capture) -> (Vec<String>, Counts) {
        let files = capture.files.iter().map(|f| f.path.clone()).collect();
        let ops = capture
            .source_operations
            .iter()
            .map(
                |(
                    path,
                    SourceOperations {
                        opens,
                        complete_reads,
                        hashes,
                    },
                )| { (path.clone(), (*opens, *complete_reads, *hashes)) },
            )
            .collect();
        (files, ops)
    }

    #[test]
    fn explicit_reconcile_replaces_corruption_without_upgrading_a_reader() {
        use crate::store::topology::{TopologyRoots, WorkspaceIdentity};
        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        fs::write(work.path().join("a.js"), "function a() {}\n").unwrap();
        let roots = TopologyRoots::isolated_for_tests(
            state.path().join("cache"),
            state.path().join("data"),
        );
        let identity = WorkspaceIdentity::discover(Some(work.path()), work.path()).unwrap();
        let path = roots.index_db(&identity);
        let store = Store::open_for_tests(state.path(), work.path()).unwrap();
        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        let options = IndexOptions::new(work.path().to_owned());
        let (old, first_owner) = reconcile_workspace(&store, &options, &cancel, |_| {}).unwrap();
        assert_eq!(store.status().unwrap().revision, old);
        drop(first_owner);
        drop(store);
        fs::write(&path, b"short").unwrap();
        let corrupt = fs::read(&path).unwrap();
        let pending = Store::open_for_tests(state.path(), work.path()).unwrap();
        assert!(pending.is_recreate_pending());
        let shared = roots.index_use_existing(&identity).unwrap();
        let busy = reconcile_workspace(&pending, &options, &cancel, |_| {}).unwrap_err();
        assert!(busy.to_string().contains("storage_busy"), "{busy:#}");
        assert_eq!(fs::read(&path).unwrap(), corrupt);
        assert!(pending.is_recreate_pending());
        drop(shared);
        let mut configured = options.clone();
        configured.max_file_bytes = 2_097_152;
        let (new, owner) = reconcile_workspace(&pending, &configured, &cancel, |_| {}).unwrap();
        assert_ne!(new.index_generation, old.index_generation);
        assert_eq!(new.index_revision, 1);
        assert_eq!(pending.status().unwrap().revision, new);
        assert_eq!(
            pending
                .recorded_index_options()
                .unwrap()
                .unwrap()
                .max_file_bytes,
            2_097_152
        );
        assert!(
            roots
                .leader(&identity)
                .unwrap_err()
                .to_string()
                .contains("storage_busy")
        );
        drop(owner);
        roots.leader(&identity).unwrap().verify().unwrap();
    }

    #[test]
    fn pending_read_refuses_unknown_options_and_daemon_config_does_not_fallback() {
        use crate::store::topology::{TopologyRoots, WorkspaceIdentity};
        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        fs::write(work.path().join("a.js"), "function a() {}\n").unwrap();
        let roots = TopologyRoots::isolated_for_tests(
            state.path().join("cache"),
            state.path().join("data"),
        );
        let identity = WorkspaceIdentity::discover(Some(work.path()), work.path()).unwrap();
        let path = roots.index_db(&identity);
        let old = Store::open_for_tests(state.path(), work.path()).unwrap();
        drop(old);
        fs::write(&path, b"not sqlite format header").unwrap();
        let original = fs::read(&path).unwrap();
        let pending = Store::open_for_tests(state.path(), work.path()).unwrap();
        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        let missing = establish_serving_session(&pending, None, &cancel).unwrap_err();
        assert!(
            missing.to_string().contains("explicit baleyg index"),
            "{missing:#}"
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        let mut configured = IndexOptions::new(work.path().to_owned());
        configured.max_file_bytes = 2_097_152;
        let shared = roots.index_use_existing(&identity).unwrap();
        let busy = establish_serving_session(&pending, Some(&configured), &cancel).unwrap_err();
        assert!(busy.to_string().contains("storage_busy"), "{busy:#}");
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(pending.is_recreate_pending());
        drop(shared);
        let owner = establish_serving_session(&pending, Some(&configured), &cancel).unwrap();
        assert!(owner.is_leader());
        assert_eq!(pending.status().unwrap().revision.index_revision, 1);
        assert_eq!(
            pending
                .recorded_index_options()
                .unwrap()
                .unwrap()
                .max_file_bytes,
            2_097_152
        );
        assert!(
            roots
                .leader(&identity)
                .unwrap_err()
                .to_string()
                .contains("storage_busy")
        );
    }

    #[test]
    fn coordinator_capture_opens_reads_and_hashes_each_source_once() {
        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        fs::write(
            work.path().join("main.js"),
            "function a() { b(); } function b() {}\n",
        )
        .unwrap();
        fs::write(
            work.path().join("Lib.java"),
            "class Lib { void run() {} }\n",
        )
        .unwrap();
        let store = Store::open_for_tests(state.path(), work.path()).unwrap();
        let options = IndexOptions::new(work.path().to_owned());
        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        let expected = |files: &[String]| {
            files
                .iter()
                .map(|path| (path.clone(), (1, 1, 1)))
                .collect::<BTreeMap<_, _>>()
        };

        let mut published = None;
        let first_job = IndexJobCoordinator::prepare(&store, None).unwrap();
        let first_session = first_job.session();
        let first = first_job
            .run_observed(
                &options,
                &cancel,
                |_| {},
                |capture| published = Some(counts(capture)),
            )
            .unwrap();
        let (files, ops) = published.expect("capture observed");
        assert_eq!(files.len(), 2);
        assert_eq!(ops, expected(&files));
        assert_eq!(store.status().unwrap().revision, first);

        // A cancel after the single capture leaves the prior pair and the same counters.
        fs::write(work.path().join("main.js"), "function late() {}\n").unwrap();
        let mut cancelled = None;
        drop(first_session);
        let second_job = IndexJobCoordinator::prepare(&store, Some(first)).unwrap();
        let _second_session = second_job.session();
        let error = second_job
            .run_observed(
                &options,
                &cancel,
                |_| {},
                |capture| {
                    cancelled = Some(counts(capture));
                    cancel.store(true, Ordering::Release);
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"), "{error:#}");
        let (files, ops) = cancelled.expect("capture observed before cancel");
        assert_eq!(ops, expected(&files));
        assert_eq!(store.index_baseline().unwrap(), first);
        let closed = store.status().unwrap_err();
        assert!(closed.to_string().contains("index_not_ready"), "{closed:#}");
    }
    #[test]
    fn reused_leader_session_is_bound_to_its_store_before_capture() {
        let state = tempfile::tempdir().unwrap();
        let first_root = tempfile::tempdir().unwrap();
        let second_root = tempfile::tempdir().unwrap();
        std::fs::write(
            first_root.path().join("a.js"),
            "function a() {}
",
        )
        .unwrap();
        std::fs::write(
            second_root.path().join("b.js"),
            "function b() {}
",
        )
        .unwrap();
        let first = Store::open_for_tests(&state.path().join("first"), first_root.path()).unwrap();
        let second =
            Store::open_for_tests(&state.path().join("second"), second_root.path()).unwrap();
        let session = first.leader_session().unwrap();
        let error = IndexJobCoordinator::prepare_with_session(&second, None, session)
            .err()
            .unwrap();
        assert!(
            error.to_string().contains("another workspace")
                || error.to_string().contains("wrong leader guard"),
            "{error:#}"
        );
    }

    #[test]
    fn reused_leader_is_reverified_after_capture_before_publish() {
        use crate::store::topology::{TopologyRoots, WorkspaceIdentity};
        use std::os::unix::fs::PermissionsExt;
        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        std::fs::write(
            work.path().join("a.js"),
            "function a() {}
",
        )
        .unwrap();
        let roots = TopologyRoots::isolated_for_tests(
            state.path().join("cache"),
            state.path().join("data"),
        );
        let identity = WorkspaceIdentity::discover(Some(work.path()), work.path()).unwrap();
        let leader_path = roots.leader_lock(&identity);
        let store = Store::open(roots, identity).unwrap();
        let job = IndexJobCoordinator::prepare(&store, None).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let error = job
            .run_observed(
                &IndexOptions::new(work.path().to_owned()),
                &cancel,
                |_| {},
                |_| {
                    std::fs::remove_file(&leader_path).unwrap();
                    std::fs::write(&leader_path, uuid::Uuid::new_v4().to_string()).unwrap();
                    std::fs::set_permissions(&leader_path, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                },
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("leader") || error.to_string().contains("managed file"),
            "{error:#}"
        );
        assert_eq!(store.index_baseline().unwrap().index_revision, 0);
    }
}
