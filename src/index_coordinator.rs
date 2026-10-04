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

    /// A diagnostic decision for two immutable admissions. Publication classifies
    /// its persisted prior manifest in one private read snapshot instead.
    pub fn staged_capture_decision(
        previous: &Capture,
        current: &Capture,
    ) -> indexer::CapturedChange {
        indexer::measure_captured_change(previous, current)
    }

    /// This stand-alone measurement is nonpublishable without a pinned prior manifest,
    /// authenticated immutable versions and a writer-transaction CAS.
    pub fn staged_capture_measurement(
        previous: &Capture,
        current: &Capture,
        root: &std::path::Path,
        root_id: &str,
        cancel: &CancelFlag,
        on_extract: impl FnMut(&crate::native_evidence::DocumentKey),
    ) -> Result<indexer::StagedNativeMeasurement> {
        indexer::measure_captured_native_change(
            previous, current, root, root_id, cancel, on_extract,
        )
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
        self.run_with_capture(options, cancel, progress, observe, false)
    }

    /// Only unchanged leader Serve may reuse selected native versions without
    /// invoking extraction. Explicit index and accepted requests keep their
    /// existing fully validated publication path.
    pub fn run_serving(
        self,
        options: &IndexOptions,
        cancel: &CancelFlag,
        progress: impl Fn(IndexProgress) + Sync,
    ) -> Result<IndexPin> {
        self.run_with_capture(options, cancel, progress, |_| {}, true)
    }

    fn run_with_capture(
        self,
        options: &IndexOptions,
        cancel: &CancelFlag,
        progress: impl Fn(IndexProgress) + Sync,
        observe: impl FnOnce(&Capture),
        serving_fast: bool,
    ) -> Result<IndexPin> {
        ensure!(!cancel.load(Ordering::Acquire), "index cancelled");
        let mut phase_start = std::time::Instant::now();
        let report = |name: &str, elapsed: std::time::Duration| {
            progress(IndexProgress {
                phase: format!("timing:{name}"),
                completed: elapsed.as_micros() as usize,
                total: 1,
            });
        };
        self.store.begin_leader_publication(&self.session)?;
        let capture = Capture::admit(options, cancel, &progress)?;
        report("capture", phase_start.elapsed());
        phase_start = std::time::Instant::now();
        if serving_fast
            && let Some(pin) = self.store.publish_unchanged_native_recovery(
                &capture,
                self.session.leader_guard()?,
                &self.expected,
                cancel,
            )?
        {
            progress(IndexProgress {
                phase: "mode:unchanged".into(),
                completed: 0,
                total: 1,
            });
            report("publish", phase_start.elapsed());
            observe(&capture);
            return Ok(pin);
        }
        let root = std::fs::canonicalize(&options.workspace_root)?;
        if let Some(prepared) =
            self.store
                .prepare_local_revision(&capture, &self.expected, cancel)?
        {
            report("measure", phase_start.elapsed());
            phase_start = std::time::Instant::now();
            let changed = indexer::project_native_document(
                options,
                &capture,
                &prepared.native,
                &prepared.changed_path,
                cancel,
                &progress,
            )?;
            if let Some(graph) = self
                .store
                .compose_local_revision(&capture, &prepared, changed, cancel)?
            {
                progress(IndexProgress {
                    phase: "mode:local".into(),
                    completed: 0,
                    total: 1,
                });
                report("compose", phase_start.elapsed());
                phase_start = std::time::Instant::now();
                capture.verify(cancel)?;
                observe(&capture);
                ensure!(!cancel.load(Ordering::Acquire), "index cancelled");
                self.store.verify_leader_session(&self.session)?;
                report("attest", phase_start.elapsed());
                phase_start = std::time::Instant::now();
                let pin = self.store.publish_local_native_recovery(
                    &graph,
                    &capture,
                    prepared,
                    self.session.leader_guard()?,
                    self.expected,
                    cancel,
                )?;
                report("publish", phase_start.elapsed());
                return Ok(pin);
            }
            // A moved D4 class cut is unproved: recalculate every native fact
            // from the SAME authenticated capture, without a second admission.
            phase_start = std::time::Instant::now();
        }
        progress(IndexProgress {
            phase: "mode:full".into(),
            completed: 0,
            total: 1,
        });
        let native =
            crate::native_evidence::from_capture(&capture, &root, self.store.root_id(), cancel)?;
        report("measure", phase_start.elapsed());
        phase_start = std::time::Instant::now();
        let graph = indexer::project_native(options, &capture, &native, cancel, &progress)?;
        report("compose", phase_start.elapsed());
        phase_start = std::time::Instant::now();
        capture.verify(cancel)?;
        observe(&capture);
        ensure!(!cancel.load(Ordering::Acquire), "index cancelled");
        self.store.verify_leader_session(&self.session)?;
        report("attest", phase_start.elapsed());
        phase_start = std::time::Instant::now();
        let published = self.store.publish_native_recovery(
            &graph,
            &capture,
            &native,
            self.session.leader_guard()?,
            self.expected,
            cancel,
        )?;
        report("publish", phase_start.elapsed());
        Ok(published)
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
        let (pin, session) = store.recreate_pending_leader_session(options, cancel)?;
        store.fail_changed_root_requests(&session)?;
        return Ok((pin, session));
    }
    let coordinator = IndexJobCoordinator::prepare(store, None)?;
    let session = coordinator.session();
    let pin = coordinator.run(options, cancel, progress)?;
    session.verify()?;
    Ok((pin, session))
}

/// A verified leader owns the queue stream. Reclaimed running rows retain their FIFO position.
/// Callers serialize this function with startup and other local native work.
pub fn drain_requests(store: &Store, session: &Arc<LeaderSession>) -> Result<usize> {
    drain_requests_observed(store, session, |_, _| {})
}

/// Progress is advisory and local. The durable request row alone controls state.
pub fn drain_requests_observed(
    store: &Store,
    session: &Arc<LeaderSession>,
    progress: impl Fn(&str, IndexProgress) + Sync,
) -> Result<usize> {
    if store.root_path_replaced()? {
        store.fail_changed_root_requests(session)?;
        anyhow::bail!("root_changed: old leader stopped after queue failure transition");
    }
    store.verify_leader_session(session)?;
    store.fail_changed_root_requests(session)?;
    // A prior attempt may have published but failed its queue terminal write. Resolve
    // that exact cached result before any new claim or native publication.
    let mut completed = usize::from(store.retry_recorded_completion(session)?);
    while let Some(request) = store.claim_request(session)? {
        let outcome = (|| {
            let options = request.options(std::path::Path::new(store.workspace_root()))?;
            let coordinator = IndexJobCoordinator::prepare_with_session(
                store,
                request.expected,
                session.clone(),
            )?;
            coordinator.run(
                &options,
                &Arc::new(std::sync::atomic::AtomicBool::new(false)),
                |p| progress(&request.id, p),
            )
        })();
        // No unverified worker can mark a request terminal. On fencing loss leave it running
        // for the next incarnation to reclaim after its complete root reconciliation.
        store.record_and_finish_request(session, &request, outcome)?;
        completed += 1;
    }
    Ok(completed)
}

fn retryable_cli_completion_error(error: &anyhow::Error) -> bool {
    // Never treat invariant failures such as "storage_busy: cached claim changed"
    // as retryable merely because their text shares a prefix with lock contention.
    #[cfg(test)]
    if error.to_string() == "storage_busy: injected terminal write failure" {
        return true;
    }
    error.chain().any(|cause| {
        cause
            .downcast_ref::<crate::store::topology::StorageBusy>()
            .is_some()
            || matches!(cause.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::SqliteFailure(info, _))
                    if matches!(info.code,
                        rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
    })
}

/// The CLI is its own sole queue driver. A transient completion failure must be
/// resolved while this verified leader is still held, not handed to a nonexistent
/// daemon or a future command. Never repeat the native publication for that head.
fn retry_cli_recorded_completion(
    store: &Store,
    session: &LeaderSession,
    cancel: &CancelFlag,
    initial: anyhow::Error,
) -> Result<()> {
    if !store.has_recorded_completion(session)? || !retryable_cli_completion_error(&initial) {
        return Err(initial);
    }
    loop {
        ensure!(
            !cancel.load(Ordering::Acquire),
            "index wait interrupted; accepted request remains queued"
        );
        store.verify_leader_session(session)?;
        match store.retry_recorded_completion(session) {
            Ok(true) => return Ok(()),
            Ok(false) => anyhow::bail!("storage_busy: cached completion disappeared"),
            Err(error) if retryable_cli_completion_error(&error) => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    }
}

/// A recovery full capture can satisfy the FIFO head when it used exactly that
/// unpinned request's options. Never publish the same capture a second time.
pub(crate) fn finish_reconciled_head(
    store: &Store,
    session: &Arc<LeaderSession>,
    options: &IndexOptions,
    pin: IndexPin,
) -> Result<()> {
    let Some(head) = store.earliest_unfinished_request()? else {
        return Ok(());
    };
    let Ok(selected) = head.options(std::path::Path::new(store.workspace_root())) else {
        return Ok(());
    };
    // The request root was verified by device/inode on enqueue and again on
    // read/claim; only the persisted option fields need exact equality here.
    if head.expected.is_some()
        || selected.scip_path != options.scip_path
        || selected.manifest_path != options.manifest_path
        || selected.max_file_bytes != options.max_file_bytes
    {
        return Ok(());
    }
    if let Some(claimed) = store.claim_request(session)? {
        ensure!(
            claimed.id == head.id,
            "storage_busy: FIFO head changed during recovery"
        );
        store.record_and_finish_request(session, &claimed, Ok(pin))?;
    }
    Ok(())
}

/// One explicit CLI command commits before waiting. A free lock requires a complete
/// takeover reconciliation before any queued request is claimed.
pub fn enqueue_and_wait(
    store: &Store,
    options: &IndexOptions,
    cancel: &CancelFlag,
) -> Result<(crate::model::IndexPin, Arc<LeaderSession>)> {
    enqueue_and_wait_observed(store, options, cancel, |_| {})
}

/// CLI-only progress observer; the durable queue and publication route remain shared.
pub fn enqueue_and_wait_observed(
    store: &Store,
    options: &IndexOptions,
    cancel: &CancelFlag,
    progress: impl Fn(IndexProgress) + Sync,
) -> Result<(crate::model::IndexPin, Arc<LeaderSession>)> {
    let request = store.enqueue_request(options, None)?;
    let mut held: Option<Arc<LeaderSession>> = None;
    loop {
        if store.root_path_replaced()? {
            if let Some(session) = &held {
                store.fail_changed_root_requests(session)?;
            }
            anyhow::bail!("root_changed: captured workspace pathname changed");
        }
        store.verify_root()?;
        if let Some(row) = store.request_by_id(&request.id)? {
            match row.state.as_str() {
                "done" => {
                    let session = match held.take() {
                        Some(session) => session,
                        None => store.follower_session()?,
                    };
                    session.verify()?;
                    return Ok((row.revision.expect("done request has revision"), session));
                }
                "failed" => anyhow::bail!(
                    "{}: queued indexing failed",
                    row.error_code.unwrap_or_else(|| "index_failed".into())
                ),
                _ => {}
            }
        }
        ensure!(
            !cancel.load(Ordering::Acquire),
            "index wait interrupted; accepted request remains queued"
        );
        if held.is_none() && store.is_recreate_pending() {
            match store.recreate_pending_leader_session(options, cancel) {
                Ok((pin, session)) => {
                    store.fail_changed_root_requests(&session)?;
                    held = Some(session.clone());
                    if let Err(error) = finish_reconciled_head(store, &session, options, pin) {
                        retry_cli_recorded_completion(store, &session, cancel, error)?;
                    }
                }
                Err(error)
                    if error.chain().any(|cause| {
                        cause
                            .downcast_ref::<crate::store::topology::StorageBusy>()
                            .is_some()
                    }) => {}
                Err(error) => return Err(error),
            }
        }
        if held.is_none() {
            match store.leader_session() {
                Ok(session) => {
                    // The takeover reconciliation precedes claims and may itself satisfy
                    // the FIFO head: it captured after acceptance using that request's
                    // exact options. Claim only after the full root-checked publication.
                    store.fail_changed_root_requests(&session)?;
                    let earliest = store.earliest_unfinished_request()?;
                    let reconcile_options = earliest
                        .as_ref()
                        .and_then(|row| {
                            row.options(std::path::Path::new(store.workspace_root()))
                                .ok()
                        })
                        .unwrap_or_else(|| options.clone());
                    let startup =
                        IndexJobCoordinator::prepare_with_session(store, None, session.clone())?;
                    let takeover_pin = startup.run(&reconcile_options, cancel, &progress)?;
                    // Retain the verified owner before any terminal write can fail.
                    held = Some(session.clone());
                    if let Some(head) = earliest
                        && head.expected.is_none()
                        && head
                            .options(std::path::Path::new(store.workspace_root()))
                            .is_ok()
                        && let Some(claimed) = store.claim_request(&session)?
                    {
                        ensure!(
                            claimed.id == head.id,
                            "storage_busy: FIFO head changed during takeover"
                        );
                        if let Err(error) =
                            store.record_and_finish_request(&session, &claimed, Ok(takeover_pin))
                        {
                            retry_cli_recorded_completion(store, &session, cancel, error)?;
                        }
                    }
                }
                Err(error) if format!("{error:#}").contains("storage_busy") => {}
                Err(error) => return Err(error),
            }
        }
        if let Some(session) = &held {
            loop {
                match drain_requests_observed(store, session, |_, phase| progress(phase)) {
                    Ok(_) => break,
                    Err(error) => retry_cli_recorded_completion(store, session, cancel, error)?,
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
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
        store.fail_changed_root_requests(&session)?;
        session.verify()?;
        return Ok(session);
    }
    match store.leader_session() {
        Ok(session) => {
            store.fail_changed_root_requests(&session)?;
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
            coordinator.run_serving(&options, cancel, |_| {})?;
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
        let queued = pending.enqueue_request(&options, None).unwrap();
        let queue_before = fs::read(pending.request_db_path()).unwrap();
        let shared = roots.index_use_existing(&identity).unwrap();
        let busy = reconcile_workspace(&pending, &options, &cancel, |_| {}).unwrap_err();
        assert!(busy.to_string().contains("storage_busy"), "{busy:#}");
        assert_eq!(fs::read(&path).unwrap(), corrupt);
        assert_eq!(fs::read(pending.request_db_path()).unwrap(), queue_before);
        assert_eq!(
            pending.request_by_id(&queued.id).unwrap().unwrap().state,
            "queued"
        );
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

#[cfg(test)]
mod queue_completion_retry_tests {
    use super::*;
    use std::fs;

    fn fixture() -> (
        tempfile::TempDir,
        Store,
        IndexOptions,
        IndexPin,
        Arc<LeaderSession>,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("a.js"), "function a() {}\n").unwrap();
        let store = Store::open_for_tests(&tmp.path().join("state"), &root).unwrap();
        let options = IndexOptions::new(root);
        let (baseline, session) = reconcile_workspace(
            &store,
            &options,
            &Arc::new(std::sync::atomic::AtomicBool::new(false)),
            |_| {},
        )
        .unwrap();
        (tmp, store, options, baseline, session)
    }

    #[test]
    fn failed_terminal_write_retries_exact_published_head_before_next_fifo_claim() {
        let (_tmp, store, options, baseline, session) = fixture();
        let first = store.enqueue_request(&options, None).unwrap();
        let second = store.enqueue_request(&options, None).unwrap();
        assert!(first.seq < second.seq);
        store.inject_queue_finish_failure(false);
        let error = drain_requests(&store, &session).unwrap_err();
        assert!(error.to_string().contains("storage_busy"));
        let published = store.status().unwrap().revision;
        assert_eq!(published.index_revision, baseline.index_revision + 1);
        assert_eq!(
            store.request_by_id(&first.id).unwrap().unwrap().state,
            "running"
        );
        assert_eq!(
            store.request_by_id(&second.id).unwrap().unwrap().state,
            "queued"
        );
        assert_eq!(drain_requests(&store, &session).unwrap(), 2);
        let q1 = store.request_by_id(&first.id).unwrap().unwrap();
        let q2 = store.request_by_id(&second.id).unwrap().unwrap();
        assert_eq!((q1.state.as_str(), q2.state.as_str()), ("done", "done"));
        assert_eq!(q1.revision.unwrap(), published);
        assert_eq!(
            q2.revision.unwrap().index_revision,
            published.index_revision + 1
        );
        assert_eq!(store.status().unwrap().revision, q2.revision.unwrap());
    }

    #[test]
    fn ambiguous_committed_terminal_is_verified_without_republishing_head() {
        let (_tmp, store, options, baseline, session) = fixture();
        let first = store.enqueue_request(&options, None).unwrap();
        let second = store.enqueue_request(&options, None).unwrap();
        store.inject_queue_finish_failure(true);
        assert_eq!(drain_requests(&store, &session).unwrap(), 2);
        let q1 = store.request_by_id(&first.id).unwrap().unwrap();
        let q2 = store.request_by_id(&second.id).unwrap().unwrap();
        assert_eq!((q1.state.as_str(), q2.state.as_str()), ("done", "done"));
        assert_eq!(
            q1.revision.unwrap().index_revision,
            baseline.index_revision + 1
        );
        assert_eq!(
            q2.revision.unwrap().index_revision,
            baseline.index_revision + 2
        );
    }
    #[test]
    fn cli_takeover_fast_path_retries_cached_first_completion_before_next_head() {
        let (_tmp, store, options, baseline, owner) = fixture();
        drop(owner);
        let q1 = store.enqueue_request(&options, None).unwrap();
        let q2 = store.enqueue_request(&options, None).unwrap();
        store.inject_queue_finish_failure(false);
        // The one CLI caller accepts Q3 and must also drain Q1/Q2. There is no
        // daemon, restart, extra waiter or manual second drain.
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (returned, held) = enqueue_and_wait(&store, &options, &cancel).unwrap();
        assert!(held.is_leader());
        let first = store.request_by_id(&q1.id).unwrap().unwrap();
        let second = store.request_by_id(&q2.id).unwrap().unwrap();
        assert_eq!(
            (first.state.as_str(), second.state.as_str()),
            ("done", "done")
        );
        assert!(q1.seq < q2.seq);
        assert_eq!(
            first.revision.unwrap().index_revision,
            baseline.index_revision + 1
        );
        assert_eq!(
            second.revision.unwrap().index_revision,
            baseline.index_revision + 2
        );
        assert_eq!(returned.index_revision, baseline.index_revision + 3);
        assert_eq!(store.status().unwrap().revision, returned);
    }

    #[test]
    fn cli_ordinary_drain_retries_cached_completion_without_republishing() {
        let (_tmp, store, options, baseline, owner) = fixture();
        drop(owner);
        // Pin Q1 to the pair the mandatory takeover capture will produce, so
        // the unpinned-head fast path is skipped and ordinary drain executes Q1.
        let takeover_pin = IndexPin {
            index_generation: baseline.index_generation,
            index_revision: baseline.index_revision + 1,
        };
        let q1 = store.enqueue_request(&options, Some(takeover_pin)).unwrap();
        let q2 = store.enqueue_request(&options, None).unwrap();
        store.inject_queue_finish_failure(false);
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (returned, held) = enqueue_and_wait(&store, &options, &cancel).unwrap();
        assert!(held.is_leader());
        let first = store.request_by_id(&q1.id).unwrap().unwrap();
        let second = store.request_by_id(&q2.id).unwrap().unwrap();
        assert_eq!(
            (first.state.as_str(), second.state.as_str()),
            ("done", "done")
        );
        assert!(q1.seq < q2.seq);
        assert_eq!(
            first.revision.unwrap().index_revision,
            baseline.index_revision + 2
        );
        assert_eq!(
            second.revision.unwrap().index_revision,
            baseline.index_revision + 3
        );
        assert_eq!(returned.index_revision, baseline.index_revision + 4);
        assert_eq!(store.status().unwrap().revision, returned);
    }
    #[test]
    fn cli_cached_claim_mismatch_fails_closed_instead_of_retrying_forever() {
        let (_tmp, store, options, baseline, session) = fixture();
        let q1 = store.enqueue_request(&options, None).unwrap();
        let q2 = store.enqueue_request(&options, None).unwrap();
        store.inject_queue_finish_failure(false);
        let initial = drain_requests(&store, &session).unwrap_err();
        assert!(
            initial
                .to_string()
                .contains("injected terminal write failure")
        );
        let published = store.status().unwrap().revision;
        assert_eq!(published.index_revision, baseline.index_revision + 1);
        let db = rusqlite::Connection::open(store.request_db_path()).unwrap();
        db.execute(
            "UPDATE requests SET claim_incarnation='changed-incarnation' WHERE id=?1",
            [&q1.id],
        )
        .unwrap();
        drop(db);
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let error = retry_cli_recorded_completion(&store, &session, &cancel, initial).unwrap_err();
        assert!(
            error.to_string().contains("cached claim changed"),
            "{error:#}"
        );
        assert_eq!(
            store.request_by_id(&q1.id).unwrap().unwrap().state,
            "running"
        );
        assert_eq!(
            store.request_by_id(&q2.id).unwrap().unwrap().state,
            "queued"
        );
        assert_eq!(store.status().unwrap().revision, published);
    }
}
