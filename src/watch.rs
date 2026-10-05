//! Advisory, bounded filesystem signals for the verified leader's native work stream.
//! A signal never proves a snapshot: the leader must capture and verify before clearing it.
use notify::{
    Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{AccessKind, AccessMode, MetadataKind, ModifyKind},
};
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    time::{Duration, Instant},
};

const MAX_DIRTY_PATHS: usize = 256;
const QUIET: Duration = Duration::from_millis(75);
const MAX_WAIT: Duration = Duration::from_millis(250);

/// A generation can be cleared only after the corresponding capture was verified and published.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirtyBatch {
    pub generation: u64,
    pub full: bool,
    pub paths: BTreeSet<PathBuf>,
}

struct Signal {
    event: notify::Result<Event>,
    received_at: Instant,
}

// Both the real notify callback and deterministic injected events use this exact ingress.
fn submit(
    sender: &SyncSender<Signal>,
    generation: &AtomicU64,
    overflow: &AtomicBool,
    event: notify::Result<Event>,
) {
    if event.as_ref().is_ok_and(|event| read_only(event.kind)) {
        return;
    }
    generation.fetch_add(1, Ordering::SeqCst);
    if sender
        .try_send(Signal {
            event,
            received_at: Instant::now(),
        })
        .is_err()
    {
        overflow.store(true, Ordering::SeqCst);
    }
}

fn read_only(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime))
            | EventKind::Access(AccessKind::Read)
            | EventKind::Access(AccessKind::Open(
                AccessMode::Read | AccessMode::Any | AccessMode::Execute
            ))
            | EventKind::Access(AccessKind::Close(AccessMode::Read | AccessMode::Execute))
    )
}

/// Owned by the elected leader, never by a reader or a second durable work queue.
pub struct WatchSignals {
    root: PathBuf,
    captured_inputs: BTreeSet<PathBuf>,
    watcher: Option<RecommendedWatcher>,
    sender: SyncSender<Signal>,
    receiver: Receiver<Signal>,
    generation: Arc<AtomicU64>,
    overflow: Arc<AtomicBool>,
    pending: DirtyBatch,
    first_signal: Option<Instant>,
    last_signal: Option<Instant>,
    urgent: bool,
    degraded: bool,
}

impl WatchSignals {
    /// `root` must already be selected and identity-verified by the leader.
    /// Optional captured SCIP/manifest paths may be ignored by the walker or
    /// outside this root; their paths still demand a full input reconciliation.
    /// The adapter does not elect a leader or retry registration on its own.
    pub fn new(root: PathBuf, scip_path: Option<PathBuf>, manifest_path: Option<PathBuf>) -> Self {
        let (sender, receiver) = mpsc::sync_channel(MAX_DIRTY_PATHS);
        let generation = Arc::new(AtomicU64::new(1));
        let overflow = Arc::new(AtomicBool::new(false));
        let callback_sender = sender.clone();
        let callback_generation = Arc::clone(&generation);
        let callback_overflow = Arc::clone(&overflow);
        let callback = move |event| {
            submit(
                &callback_sender,
                &callback_generation,
                &callback_overflow,
                event,
            );
        };
        let watcher = notify::recommended_watcher(callback).and_then(|mut watcher| {
            watcher.watch(&root, RecursiveMode::Recursive)?;
            Ok(watcher)
        });
        let degraded = watcher.is_err();
        let captured_inputs = [scip_path, manifest_path]
            .into_iter()
            .flatten()
            .map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    root.join(path)
                }
            })
            .collect();
        Self {
            root,
            captured_inputs,
            watcher: watcher.ok(),
            sender,
            receiver,
            generation,
            overflow,
            pending: DirtyBatch {
                generation: 1,
                full: true, // Wake, restart and takeover require a full inventory.
                paths: BTreeSet::new(),
            },
            first_signal: None,
            last_signal: None,
            urgent: true,
            degraded,
        }
    }

    pub fn degraded(&self) -> bool {
        self.degraded
    }

    pub fn watching(&self) -> bool {
        self.watcher.is_some() && !self.degraded
    }

    /// Wake/restart/failed publication must force a fresh full reconciliation.
    pub fn require_full(&mut self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.pending.full = true;
        self.urgent = true;
    }

    /// Feed the same bounded ingress used by notify's actual callback. A
    /// runtime error or a dropped signal forces a full inventory.
    pub fn submit_event(&self, event: notify::Result<Event>) {
        submit(&self.sender, &self.generation, &self.overflow, event);
    }

    /// Nonblocking, bounded drain: never wait for a continuously active producer.
    /// The caller must drain again before acting on a batch or checking its cutoff.
    pub fn drain(&mut self) -> DirtyBatch {
        let mut processed = 0;
        for _ in 0..MAX_DIRTY_PATHS {
            match self.receiver.try_recv() {
                Ok(Signal { event, received_at }) => {
                    processed += 1;
                    match event {
                        Ok(event) => self.record(&event, received_at),
                        Err(_) => self.fail(),
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.degraded {
                        self.fail();
                    }
                    break;
                }
            }
        }
        // The bounded drain may leave events queued. Until a complete inventory
        // accounts for them, neither a partial path set nor a generation is proof.
        if processed == MAX_DIRTY_PATHS {
            self.pending.full = true;
            self.urgent = true;
        }
        if self.overflow.swap(false, Ordering::SeqCst) {
            self.pending.full = true;
            self.urgent = true;
        }
        self.pending.generation = self.generation.load(Ordering::SeqCst);
        if self.degraded {
            self.pending.full = true;
            self.urgent = true;
        }
        self.pending.clone()
    }

    /// The single leader stream can use this deadline rather than sleeping in
    /// drain. Continuous edits never delay a batch beyond the first 250 ms.
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.urgent || self.degraded {
            return Some(Instant::now());
        }
        match (self.first_signal, self.last_signal) {
            (Some(first), Some(last)) => Some((last + QUIET).min(first + MAX_WAIT)),
            _ => None,
        }
    }

    pub fn batch_ready_at(&self, now: Instant) -> bool {
        self.urgent || self.degraded || self.next_deadline().is_some_and(|deadline| now >= deadline)
    }

    /// Call only after a verified publication (or verified unchanged capture)
    /// accounts for the batch. A concurrent event leaves the dirty bit set.
    pub fn acknowledge(&mut self, batch: &DirtyBatch) -> bool {
        self.drain();
        if self.pending.generation != batch.generation
            || (self.pending.full && !batch.full)
            || self.generation.load(Ordering::SeqCst) != batch.generation
        {
            return false;
        }
        self.pending.paths.clear();
        self.pending.full = self.degraded;
        self.first_signal = None;
        self.last_signal = None;
        self.urgent = self.degraded;
        // An event racing the clear remains in the channel and invalidates the
        // acknowledged generation; force a full inventory if its timing is uncertain.
        if self.generation.load(Ordering::SeqCst) != batch.generation {
            self.pending.full = true;
            self.urgent = true;
            return false;
        }
        true
    }

    fn fail(&mut self) {
        self.degraded = true;
        self.pending.full = true;
        self.urgent = true;
        self.watcher = None;
    }

    /// Direct feed for the leader's wider event loop. The normal notify callback
    /// goes through `submit`; both paths share event classification.
    pub fn observe_event(&mut self, event: &Event) {
        if read_only(event.kind) {
            return;
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.record(event, Instant::now());
    }

    fn record(&mut self, event: &Event, received_at: Instant) {
        self.first_signal.get_or_insert(received_at);
        self.last_signal = Some(received_at);
        if event.paths.is_empty() || matches!(event.kind, EventKind::Any | EventKind::Other) {
            self.pending.full = true;
            self.urgent = true;
        }
        // A rename must include both endpoints; an incomplete backend event
        // needs a full walk to discover the missing endpoint.
        if matches!(event.kind, EventKind::Modify(ModifyKind::Name(_))) && event.paths.len() != 2 {
            self.pending.full = true;
        }
        if matches!(
            event.kind,
            EventKind::Create(_)
                | EventKind::Remove(_)
                | EventKind::Access(_)
                | EventKind::Modify(ModifyKind::Metadata(_))
        ) {
            self.pending.full = true;
        }
        for path in &event.paths {
            let absolute = if path.is_absolute() {
                path.clone()
            } else {
                self.root.join(path)
            };
            if self.captured_inputs.contains(&absolute) {
                self.pending.full = true;
            }
            let Ok(relative) = absolute.strip_prefix(&self.root) else {
                self.pending.full = true;
                continue;
            };
            if relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            {
                self.pending.full = true;
                continue;
            }
            if relevant_input(relative) || !source(relative) || !safe_regular(&self.root, relative)
            {
                self.pending.full = true;
            }
            self.pending.paths.insert(relative.to_path_buf());
            if self.pending.paths.len() > MAX_DIRTY_PATHS {
                self.pending.full = true;
                self.urgent = true;
                self.pending.paths.clear();
            }
        }
    }
}

// Never use extension alone to classify symlinks, disappeared paths or a
// symlinked ancestor as an ordinary source edit. Capture still walks no-follow.
fn safe_regular(root: &Path, relative: &Path) -> bool {
    let mut path = root.to_path_buf();
    for part in relative.components() {
        path.push(part);
        let Ok(meta) = fs::symlink_metadata(&path) else {
            return false;
        };
        if path == root.join(relative) {
            return meta.file_type().is_file();
        }
        if !meta.file_type().is_dir() {
            return false;
        }
    }
    false
}

fn source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("js" | "mjs" | "cjs" | "rs" | "java" | "py")
    )
}

fn relevant_input(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|e| e.to_str()),
        Some(".gitignore" | ".ignore")
    ) || path.components().count() == 1
        && crate::capture::ROOT_INPUTS
            .iter()
            .any(|name| path == Path::new(name))
}
