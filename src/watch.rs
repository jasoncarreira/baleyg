//! Advisory, bounded filesystem signals for the verified leader's native work stream.
//! A signal never proves a snapshot: the leader must capture and verify before clearing it.
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher, event::ModifyKind};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
};

const MAX_DIRTY_PATHS: usize = 256;

/// A generation can be cleared only after the corresponding capture was verified and published.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirtyBatch {
    pub generation: u64,
    pub full: bool,
    pub paths: BTreeSet<PathBuf>,
}

struct Signal {
    event: notify::Result<Event>,
}

/// Owned by the elected leader, never by a reader or a second durable work queue.
pub struct WatchSignals {
    root: PathBuf,
    watcher: Option<RecommendedWatcher>,
    receiver: Receiver<Signal>,
    generation: Arc<AtomicU64>,
    overflow: Arc<AtomicBool>,
    pending: DirtyBatch,
    degraded: bool,
}

impl WatchSignals {
    /// `root` must already be selected and identity-verified by the leader. The
    /// adapter does not elect a leader or retry registration on its own.
    pub fn new(root: PathBuf) -> Self {
        let (sender, receiver) = mpsc::sync_channel(MAX_DIRTY_PATHS);
        let generation = Arc::new(AtomicU64::new(1));
        let overflow = Arc::new(AtomicBool::new(false));
        let callback_generation = Arc::clone(&generation);
        let callback_overflow = Arc::clone(&overflow);
        let callback = move |event| {
            callback_generation.fetch_add(1, Ordering::SeqCst);
            if sender.try_send(Signal { event }).is_err() {
                callback_overflow.store(true, Ordering::SeqCst);
            }
        };
        let watcher = notify::recommended_watcher(callback).and_then(|mut watcher| {
            watcher.watch(&root, RecursiveMode::Recursive)?;
            Ok(watcher)
        });
        let degraded = watcher.is_err();
        Self {
            root,
            watcher: watcher.ok(),
            receiver,
            generation,
            overflow,
            pending: DirtyBatch {
                generation: 1,
                full: true, // Wake, restart and takeover always require a full inventory.
                paths: BTreeSet::new(),
            },
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
    }

    /// Nonblocking drain. A disconnected or failed watcher stays degraded;
    /// callers must keep doing full inventory/stat walks, not cease indexing.
    pub fn drain(&mut self) -> DirtyBatch {
        loop {
            match self.receiver.try_recv() {
                Ok(Signal { event }) => match event {
                    Ok(event) => self.record(&event),
                    Err(_) => self.fail(),
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.degraded {
                        self.fail();
                    }
                    break;
                }
            }
        }
        if self.overflow.swap(false, Ordering::SeqCst) {
            self.pending.full = true;
        }
        self.pending.generation = self.generation.load(Ordering::SeqCst);
        if self.degraded {
            self.pending.full = true;
        }
        self.pending.clone()
    }

    /// Call only after a verified publication (or verified unchanged capture)
    /// accounts for the batch. A concurrent event leaves the dirty bit set.
    pub fn acknowledge(&mut self, batch: &DirtyBatch) -> bool {
        self.drain();
        if self.pending.generation != batch.generation {
            return false;
        }
        self.pending.paths.clear();
        self.pending.full = self.degraded; // A failed watcher requires every later scan to be full.
        true
    }

    fn fail(&mut self) {
        self.degraded = true;
        self.pending.full = true;
        self.watcher = None;
    }

    /// Feed an externally received backend event into the same advisory path.
    /// Useful when a platform's watcher callback is owned by a wider event loop.
    pub fn observe_event(&mut self, event: &Event) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.record(event);
    }

    fn record(&mut self, event: &Event) {
        if event.paths.is_empty() || matches!(event.kind, EventKind::Any | EventKind::Other) {
            self.pending.full = true;
        }
        // A rename must include both endpoints. If a backend cannot supply
        // both, a full walk discovers the absent/new paths instead.
        if matches!(event.kind, EventKind::Modify(ModifyKind::Name(_))) && event.paths.len() != 2 {
            self.pending.full = true;
        }
        if matches!(event.kind, EventKind::Create(_) | EventKind::Remove(_)) {
            self.pending.full = true; // Includes new directories and removed ignore rules.
        }
        for path in &event.paths {
            let absolute = if path.is_absolute() {
                path.clone()
            } else {
                self.root.join(path)
            };
            let Ok(relative) = absolute.strip_prefix(&self.root) else {
                self.pending.full = true;
                continue;
            };
            if relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                self.pending.full = true;
                continue;
            }
            if relevant_input(relative) || !source(relative) {
                self.pending.full = true;
            }
            self.pending.paths.insert(relative.to_path_buf());
            if self.pending.paths.len() > MAX_DIRTY_PATHS {
                self.pending.full = true;
                self.pending.paths.clear();
            }
        }
    }
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
