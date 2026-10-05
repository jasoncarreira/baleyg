//! Shared admission and publication boundary for CLI and authenticated daemon index jobs.
//! A job has one expected pair, one captured source set, and one complete paired commit.
use crate::{
    capture::Capture,
    indexer::{self, IndexOptions},
    model::{CancelFlag, IndexPin, IndexProgress},
    store::{RecoveryBaseline, Store, topology::LeaderSession},
};
use anyhow::{Context, Result, ensure};
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
    drain_requests_observed_with_cancel(
        store,
        session,
        &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        progress,
    )
}

fn drain_requests_observed_with_cancel(
    store: &Store,
    session: &Arc<LeaderSession>,
    cancel: &CancelFlag,
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
    loop {
        ensure!(
            !cancel.load(Ordering::Acquire),
            "index wait interrupted; inspect the accepted request's durable state"
        );
        let Some(request) = store.claim_request(session)? else {
            break;
        };
        #[cfg(test)]
        let diagnostic_stage = std::cell::Cell::new("options");
        // Capture the authenticated expected pin from the same publication
        // admission. A BUSY COMMIT can be safely retried only if its terminal
        // queue reread and current control pin still match this baseline.
        let mut before_publish = None;
        let outcome = (|| {
            let options = request.options(std::path::Path::new(store.workspace_root()))?;
            #[cfg(test)]
            diagnostic_stage.set("prepare");
            let coordinator = IndexJobCoordinator::prepare_with_session(
                store,
                request.expected,
                session.clone(),
            )?;
            before_publish = Some(coordinator.expected.pin());
            #[cfg(test)]
            diagnostic_stage.set("run");
            coordinator.run(&options, cancel, |p| progress(&request.id, p))
        })();
        #[cfg(test)]
        if let Err(ref error) = outcome {
            // Only allowlisted error classes reach CI; no raw source/path/token
            // or generic error text is copied into public test output.
            let text = format!("{error:#}");
            let category = if text.contains("storage_busy") {
                "storage_busy"
            } else if text.contains("recovery_required") {
                "recovery_required"
            } else if text.contains("index_not_ready") {
                "index_not_ready"
            } else if text.contains("incompatible_index") {
                "incompatible_index"
            } else if text.contains("root_changed") {
                "root_changed"
            } else if text.contains("revision conflict") {
                "revision_conflict"
            } else if text.contains("database is locked") {
                "sqlite_locked"
            } else if text.contains("database disk image is malformed") {
                "sqlite_corrupt"
            } else if text.contains("unsafe or oversized") {
                "unsafe_input"
            } else if text.contains("source changed") {
                "source_changed"
            } else {
                "other"
            };
            eprintln!(
                "queue_failure stage={} category={} owner_leader={} claim_matches={} expected_pin={}",
                diagnostic_stage.get(),
                category,
                session.is_leader(),
                request.claim_incarnation.as_deref()
                    == Some(session.incarnation().to_string().as_str()),
                request.expected.is_some()
            );
        }
        // Ctrl-C stops this CLI waiter, not any accepted client's durable work.
        // The claim stays running under the old incarnation and the next
        // verified leader reclaims the FIFO head after full reconciliation.
        // Even a publication that raced Ctrl-C cannot be reported terminally
        // by a cancelled waiter; repeat-after-commit is explicitly allowed.
        ensure!(
            !cancel.load(Ordering::Acquire),
            "index wait interrupted; accepted request remains running for verified reclaim"
        );
        if outcome
            .as_ref()
            .is_err_and(crate::store::transient_storage_contention)
        {
            // The failed COMMIT might be ambiguous. Authenticate both the
            // durable terminal row and the current pin before returning the
            // SAME seq to queued; a changed pin remains running for a fresh
            // leader reconciliation, never an invented terminal ACK.
            let before = match before_publish {
                Some(pin) => pin,
                None => store.recovery_index_baseline()?.pin(),
            };
            store.requeue_busy_claim(session, &request, before)?;
            return Ok(completed);
        }
        if outcome
            .as_ref()
            .is_err_and(crate::store::nonterminal_storage_busy)
        {
            return outcome.map(|_| completed);
        }
        // No unverified worker can mark a request terminal. On fencing loss leave it running
        // for the next incarnation to reclaim after its complete root reconciliation.
        store.record_and_finish_request(session, &request, outcome)?;
        completed += 1;
    }
    Ok(completed)
}

pub(crate) fn retryable_cli_completion_error(error: &anyhow::Error) -> bool {
    // Include storage_result's plain SQLite contention string, but never a
    // cached-claim or leader/root invariant that also uses storage_busy.
    #[cfg(test)]
    if error.to_string() == "storage_busy: injected terminal write failure" {
        return true;
    }
    crate::store::transient_storage_contention(error)
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
    head_before_capture: Option<&str>,
) -> Result<()> {
    // An ACK admitted during capture did not cause that publication. It must
    // be claimed and indexed from its own options, not marked done by an
    // earlier recovery pin with superficially matching options.
    let Some(prior_id) = head_before_capture else {
        return Ok(());
    };
    let Some(head) = store.earliest_unfinished_request()? else {
        return Ok(());
    };
    if head.id != prior_id {
        return Ok(());
    }
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
    let mut observed_store = store.clone();
    let mut held: Option<Arc<LeaderSession>> = None;
    loop {
        let store = &observed_store;
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
                        None => match store.follower_session() {
                            Ok(session) => session,
                            Err(error)
                                if store.is_recreate_pending()
                                    && error.to_string()
                                        == "recovery_required: exceptional index recovery deferred" =>
                            {
                                // A daemon finished our accepted row while this
                                // process still held its pre-repair disposition.
                                // Re-admit only the same existing root/index;
                                // never report success from queue bytes alone.
                                let refreshed=store.reopen_existing_current_root().with_context(||format!(
                                    "accepted request {} is done in durable queue, but current publication cannot be verified",
                                    request.id
                                ))?;
                                ensure!(
                                    !refreshed.is_recreate_pending(),
                                    "storage_busy: completed request has no verified publication"
                                );
                                observed_store = refreshed;
                                continue;
                            }
                            Err(error) => return Err(error),
                        },
                    };
                    session.verify()?;
                    let pin = row.revision.expect("done request has revision");
                    let current = store.status()?.revision;
                    ensure!(
                        pin.index_generation == current.index_generation
                            && pin.index_revision <= current.index_revision,
                        "storage_busy: completed request pin is not retained by current publication"
                    );
                    return Ok((pin, session));
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
            let head_before_capture = store.earliest_unfinished_request()?.map(|row| row.id);
            match store.recreate_pending_leader_session(options, cancel) {
                Ok((pin, session)) => {
                    store.fail_changed_root_requests(&session)?;
                    held = Some(session.clone());
                    if let Err(error) = finish_reconciled_head(
                        store,
                        &session,
                        options,
                        pin,
                        head_before_capture.as_deref(),
                    ) {
                        retry_cli_recorded_completion(store, &session, cancel, error)?;
                    }
                }
                Err(error)
                    if error.chain().any(|cause| {
                        cause
                            .downcast_ref::<crate::store::topology::StorageBusy>()
                            .is_some()
                    }) => {}
                Err(error)
                    if error.to_string()
                        == "recovery_required: exceptional index recovery deferred" =>
                {
                    // A second owner may have repaired index.db after our
                    // admission snapshot. Refresh only the same verified root
                    // and existing index; never reuse stale recreation authority.
                    match store.reopen_existing_current_root() {
                        Ok(refreshed) if !refreshed.is_recreate_pending() => {
                            observed_store=refreshed;
                            continue;
                        }
                        Ok(_) => {
                            let row=store.request_by_id(&request.id)?;
                            anyhow::bail!("recovery_required: accepted request {} remains {}; index still requires verified recovery",
                                request.id,row.map(|r|r.state).unwrap_or_else(||"unavailable".into()));
                        }
                        Err(wait) if retryable_cli_completion_error(&wait) => {},
                        Err(wait) => return Err(wait).context(format!(
                            "accepted request {} has a durable queue row; recovery could not be refreshed",
                            request.id)),
                    }
                }
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
                Err(error)
                    if format!("{error:#}").contains("storage_busy")
                        || (store.is_recreate_pending()
                            && format!("{error:#}").contains("recovery_required")) =>
                {
                    // Another verified owner may still hold the old use lock.
                    // Our request is already durable; wait rather than exit
                    // with an unreported queued acknowledgement.
                }
                Err(error) => return Err(error),
            }
        }
        if let Some(session) = &held {
            loop {
                match drain_requests_observed_with_cancel(store, session, cancel, |_, phase| {
                    progress(phase)
                }) {
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
    fn publish_commit_busy_once_must_keep_accepted_fifo_head_and_finish_same_seq() {
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("a.js"), "function a() {}\n").unwrap();
        let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
        let options = IndexOptions::new(workspace.path().to_owned());
        let cancel = Arc::new(AtomicBool::new(false));
        let (_, old_owner) = reconcile_workspace(&store, &options, &cancel, |_| {}).unwrap();
        drop(old_owner);
        let browser = store.enqueue_request(&options, None).unwrap();
        let cli = store.enqueue_request(&options, None).unwrap();
        assert!(browser.seq < cli.seq);
        let (base, leader) = reconcile_workspace(&store, &options, &cancel, |_| {}).unwrap();
        let injection = AtomicBool::new(false);
        assert_eq!(
            drain_requests_observed(&store, &leader, |id, phase| {
                if id == cli.id
                    && phase.phase == "timing:capture"
                    && !injection.swap(true, Ordering::AcqRel)
                {
                    store.fail_next_live_publish_commit_busy();
                }
            })
            .unwrap(),
            1
        );
        assert!(
            injection.load(Ordering::Acquire),
            "inject once at Q2's real commit boundary"
        );
        let a = store.request_by_id(&browser.id).unwrap().unwrap();
        let b = store.request_by_id(&cli.id).unwrap().unwrap();
        assert_eq!(a.state, "done");
        assert_eq!(
            b.seq, cli.seq,
            "accepted CLI ACK cannot be deleted/re-admitted under a new seq"
        );
        assert_eq!(
            b.state, "queued",
            "pre-commit storage_busy must return same-seq accepted ACK to queued, not index_failed"
        );
        assert!(b.finished_at.is_none() && b.error_code.is_none() && b.revision.is_none());
        assert_eq!(
            store.index_baseline().unwrap().index_revision,
            base.index_revision + 1,
            "failed commit must leave only the prior Q1 publication"
        );
        assert_eq!(
            drain_requests(&store, &leader).unwrap(),
            1,
            "same verified leader may reclaim requeued Q2 on next bounded tick"
        );
        let b = store.request_by_id(&cli.id).unwrap().unwrap();
        assert_eq!(
            b.state, "done",
            "retry must finish the original accepted same-seq Q2"
        );
        assert!(b.error_code.is_none());
        assert_eq!(a.revision.unwrap().index_generation, base.index_generation);
        assert_eq!(a.revision.unwrap().index_revision, base.index_revision + 1);
        assert_eq!(
            b.revision.unwrap().index_revision,
            a.revision.unwrap().index_revision + 1,
            "failed commit cannot leave a partially published revision"
        );
        assert_eq!(store.status().unwrap().revision, b.revision.unwrap());
        assert!(leader.verify().is_ok());
    }

    #[test]
    fn cached_completion_retry_only_accepts_real_lock_contention_not_claim_invariants() {
        assert!(retryable_cli_completion_error(&anyhow::anyhow!(
            "storage_busy: SQLite lock contention"
        )));
        assert!(!retryable_cli_completion_error(&anyhow::anyhow!(
            "storage_busy: cached claim changed"
        )));
        assert!(!retryable_cli_completion_error(&anyhow::anyhow!(
            "storage_busy: unresolved FIFO completion"
        )));
    }

    #[test]
    fn ambiguous_post_commit_busy_must_leave_running_for_verified_reconciliation() {
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("a.js"), "function a() {}\n").unwrap();
        let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
        let options = IndexOptions::new(workspace.path().to_owned());
        let cancel = Arc::new(AtomicBool::new(false));
        let (_, old) = reconcile_workspace(&store, &options, &cancel, |_| {}).unwrap();
        drop(old);
        let ack = store.enqueue_request(&options, None).unwrap();
        let (before, leader) = reconcile_workspace(&store, &options, &cancel, |_| {}).unwrap();
        store.fail_next_live_publish_post_commit_busy();
        let error = drain_requests(&store, &leader).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("publication changed across failed commit"),
            "{error:#}"
        );
        let row = store.request_by_id(&ack.id).unwrap().unwrap();
        assert_eq!(
            row.state, "running",
            "unknown result must not be requeued or failed"
        );
        assert_eq!(row.seq, ack.seq);
        assert!(row.error_code.is_none() && row.revision.is_none() && row.finished_at.is_none());
        let committed = store.index_baseline().unwrap();
        assert_eq!(committed.index_generation, before.index_generation);
        assert_eq!(
            committed.index_revision,
            before.index_revision + 1,
            "test seam committed selected pin before returning BUSY"
        );
        drop(leader);
        let successor = Store::open_for_tests(state.path(), workspace.path()).unwrap();
        let (_, new_owner) = reconcile_workspace(&successor, &options, &cancel, |_| {}).unwrap();
        assert_eq!(drain_requests(&successor, &new_owner).unwrap(), 1);
        let done = successor.request_by_id(&ack.id).unwrap().unwrap();
        assert_eq!(done.state, "done");
        assert_eq!(done.seq, ack.seq);
        assert!(done.revision.unwrap().index_revision > committed.index_revision);
    }

    #[test]
    fn cli_ctrl_c_must_not_fail_another_clients_running_fifo_request() {
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("a.js"), "function a() {}\n").unwrap();
        let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
        let options = IndexOptions::new(workspace.path().to_owned());
        let cancel = Arc::new(AtomicBool::new(false));
        let (_, old_owner) = reconcile_workspace(&store, &options, &cancel, |_| {}).unwrap();
        drop(old_owner);
        let ahead = store.enqueue_request(&options, None).unwrap();
        let browser = store.enqueue_request(&options, None).unwrap();
        let captures = std::sync::atomic::AtomicUsize::new(0);
        let error = enqueue_and_wait_observed(&store, &options, &cancel, |phase| {
            if phase.phase == "timing:capture" && captures.fetch_add(1, Ordering::AcqRel) > 0 {
                cancel.store(true, Ordering::Release);
            }
        })
        .unwrap_err();
        assert_eq!(
            captures.load(Ordering::Acquire),
            2,
            "takeover satisfied earlier A; CLI Ctrl-C happened during browser B capture"
        );
        assert!(error.to_string().contains("interrupted"), "{error:#}");
        let a = store.request_by_id(&ahead.id).unwrap().unwrap();
        let b = store.request_by_id(&browser.id).unwrap().unwrap();
        let cli = store.current_request().unwrap().unwrap();
        assert_eq!(a.state, "done");
        assert_eq!(
            b.state, "running",
            "other client's ACK must survive CLI Ctrl-C"
        );
        assert!(b.error_code.is_none() && b.revision.is_none());
        assert_eq!(cli.state, "queued", "CLI's own later ACK also survives");
        assert!(a.seq < b.seq && b.seq < cli.seq);
        let reopened = Store::open_for_tests(state.path(), workspace.path()).unwrap();
        let (_, new_leader) = reconcile_workspace(
            &reopened,
            &options,
            &Arc::new(AtomicBool::new(false)),
            |_| {},
        )
        .unwrap();
        assert_eq!(drain_requests(&reopened, &new_leader).unwrap(), 2);
        let b_done = reopened.request_by_id(&browser.id).unwrap().unwrap();
        let cli_done = reopened.request_by_id(&cli.id).unwrap().unwrap();
        assert_eq!(
            (b_done.state.as_str(), cli_done.state.as_str()),
            ("done", "done")
        );
        assert!(
            b_done.revision.unwrap().index_revision < cli_done.revision.unwrap().index_revision
        );
    }

    #[test]
    fn cli_ctrl_c_during_request_capture_leaves_own_request_running() {
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("a.js"), "function a() {}\n").unwrap();
        let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
        let options = IndexOptions::new(workspace.path().to_owned());
        let cancel = Arc::new(AtomicBool::new(false));
        let (_, old_owner) = reconcile_workspace(&store, &options, &cancel, |_| {}).unwrap();
        drop(old_owner);
        let ahead = store.enqueue_request(&options, None).unwrap();
        let captures = std::sync::atomic::AtomicUsize::new(0);
        let error = enqueue_and_wait_observed(&store, &options, &cancel, |phase| {
            if phase.phase == "timing:capture" && captures.fetch_add(1, Ordering::AcqRel) > 0 {
                cancel.store(true, Ordering::Release);
            }
        })
        .unwrap_err();
        assert_eq!(
            captures.load(Ordering::Acquire),
            2,
            "first capture satisfied earlier FIFO row; second was CLI's own drain"
        );
        assert!(error.to_string().contains("interrupted"), "{error:#}");
        assert_eq!(
            store.request_by_id(&ahead.id).unwrap().unwrap().state,
            "done"
        );
        let row = store.current_request().unwrap().unwrap();
        assert_eq!(
            row.state, "running",
            "Ctrl-C stops waiting but preserves the accepted request"
        );
        assert!(row.error_code.is_none() && row.revision.is_none());
    }

    #[test]
    fn recovery_pin_must_not_complete_a_request_admitted_after_its_capture() {
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("a.js"), "function a() {}\n").unwrap();
        let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
        let options = IndexOptions::new(workspace.path().to_owned());
        let cancel = Arc::new(AtomicBool::new(false));
        let (old_pin, session) = reconcile_workspace(&store, &options, &cancel, |_| {}).unwrap();
        // The owner published its source capture first; only then did a client
        // commit this new ACK. It cannot claim that earlier publication.
        let later = store.enqueue_request(&options, None).unwrap();
        finish_reconciled_head(&store, &session, &options, old_pin, None).unwrap();
        assert_eq!(
            store.request_by_id(&later.id).unwrap().unwrap().state,
            "queued"
        );
        assert_eq!(drain_requests(&store, &session).unwrap(), 1);
        let completed = store.request_by_id(&later.id).unwrap().unwrap();
        assert_eq!(completed.state, "done");
        assert!(completed.revision.unwrap().index_revision > old_pin.index_revision);
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
