//! The enrolled read-only cache connection and its identity guard.
//!
//! `Store::cache()` opens a fresh connection by pathname for every read and `connect()` performs
//! writable setup and migration; `secure_database_file()` uses `.create(true)`, so a deleted cache
//! silently becomes a new empty database. None of that can back an MCP binding, so this module
//! owns a separate connection with its own lifetime, identity anchors and failure policy.
//!
//! Measured behavior this relies on (see `docs/mcp-pilot-slice0-findings.md`): a read-only open
//! creates `-wal`/`-shm` and is refused outright in a non-writable directory; `SQLITE_FCNTL_HAS_MOVED`
//! detects replacement, unlink and rename but not an inode-preserving overwrite; and an interrupted
//! read returns `SQLITE_INTERRUPT` while leaving the connection immediately reusable.

// PR1 deliberately keeps the generic foundation crate-private. PR2's typed MCP adapter is its first
// production caller; unit tests exercise the complete protocol in this slice.
#![allow(dead_code)]

use super::{ErrorCode, McpError};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags};
use std::{
    collections::HashSet,
    ffi::CString,
    fs::{File, Metadata},
    marker::PhantomData,
    os::raw::{c_int, c_void},
    os::unix::io::{FromRawFd, IntoRawFd, RawFd},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, Condvar, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, Ordering},
    },
    thread,
    time::Instant,
};

const UNAVAILABLE: &str = "The bound store is unavailable";
const LATCHED: &str = "The bound store was invalidated; restart and owner reissue are required";
const TIMED_OUT: &str = "The operation deadline elapsed";
const CONFLICT: &str = "The index revision changed";
const CONSUMED: &str = "The admitted handoff was already consumed";

/// Flags for the enrolled connection. `SQLITE_OPEN_NOFOLLOW` matters independently of the anchor
/// descriptors: if the cache pathname becomes a symlink between anchor acquisition and this open,
/// SQLite would otherwise follow it and create WAL/SHM beside the target before the identity check
/// rejects enrollment.
pub(crate) const READ_ONLY_FLAGS: OpenFlags = OpenFlags::SQLITE_OPEN_READ_ONLY
    .union(OpenFlags::SQLITE_OPEN_NO_MUTEX)
    .union(OpenFlags::SQLITE_OPEN_NOFOLLOW);

/// Unix device/inode pair. Equality is the only identity claim made here; it is not tamper
/// attestation, and an inode-preserving overwrite is indistinguishable by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId {
    dev: u64,
    ino: u64,
}

#[cfg(unix)]
fn file_id(meta: &Metadata) -> FileId {
    use std::os::unix::fs::MetadataExt;
    FileId {
        dev: meta.dev(),
        ino: meta.ino(),
    }
}
#[cfg(not(unix))]
fn file_id(_: &Metadata) -> FileId {
    FileId { dev: 0, ino: 0 }
}

/// The publication boundary shared by ordinary stores and MCP admission.
///
/// The lock inode is created only when a state directory is first initialized. Every later
/// acquisition opens it without following links or creating a replacement and verifies the
/// identity recorded here.
#[derive(Debug)]
pub(crate) struct Boundary {
    state: Mutex<BoundaryState>,
    ready: Condvar,
    in_flight: AtomicU32,
    enrolled: AtomicBool,
    state_dir: PathBuf,
    dir_id: FileId,
    lock_path: PathBuf,
    lock_id: FileId,
    _dir_anchor: File,
    _lock_anchor: File,
}

#[derive(Debug, Default)]
struct BoundaryState {
    publishing: bool,
    waiting: u32,
    failed: bool,
}

static PUBLICATION_QUARANTINE: OnceLock<RwLock<HashSet<PathBuf>>> = OnceLock::new();

fn publication_quarantine() -> &'static RwLock<HashSet<PathBuf>> {
    PUBLICATION_QUARANTINE.get_or_init(|| RwLock::new(HashSet::new()))
}

/// Held by publication for the database commit only.
pub(crate) struct Publishing<'a> {
    boundary: &'a Boundary,
    cross_process: Option<File>,
    transition: PublicationTransition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicationTransition {
    AbortedBeforeCommit,
    Committed,
    FailClosed,
}

/// Private half of an admitted handoff. Dropping it releases both publication barriers.
struct AdmissionPermit {
    boundary: Arc<Boundary>,
    cross_process: Option<File>,
    active: bool,
}

const LEASE_ACTIVE: u8 = 0;
const LEASE_CLAIMED: u8 = 1;
const LEASE_RELEASED: u8 = 2;

/// The only publication authority retained by a handoff. Expiry touches this small object only;
/// it never owns or drops the generic response/evidence value.
struct AuthorityLease {
    state: AtomicU8,
    boundary: Arc<Boundary>,
    fd: AtomicI32,
    #[cfg(test)]
    claim_pause: Mutex<Option<ClaimPause>>,
}

#[cfg(test)]
struct ClaimPause {
    reached: std::sync::mpsc::SyncSender<()>,
    resume: std::sync::mpsc::Receiver<()>,
    claimed: Arc<AtomicBool>,
}

impl AuthorityLease {
    fn from_permit(mut permit: AdmissionPermit) -> Arc<Self> {
        let file = permit
            .cross_process
            .take()
            .expect("an active admission owns its shared flock");
        permit.active = false;
        Arc::new(Self {
            state: AtomicU8::new(LEASE_ACTIVE),
            boundary: permit.boundary.clone(),
            fd: AtomicI32::new(file.into_raw_fd()),
            #[cfg(test)]
            claim_pause: Mutex::new(None),
        })
    }

    fn is_active(&self) -> bool {
        self.state.load(Ordering::Acquire) == LEASE_ACTIVE
    }

    /// Transfer the live lease to the synchronous final handoff. Expiry can no longer win after
    /// this point; the private finalizer must be infallible and nonblocking.
    fn claim_for_commit(&self, deadline: Instant) -> Result<(), McpError> {
        if Instant::now() >= deadline {
            self.release();
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        }

        // The test-only guard holds expiry immediately after the precheck so a deterministic race
        // can cross the deadline before this CAS. Production builds contain neither the hook nor
        // the guard.
        #[cfg(test)]
        let mut claim_pause = self.claim_pause.lock().unwrap_or_else(|_| fatal_poison());
        #[cfg(test)]
        let claim_observer = claim_pause.take().map(|pause| {
            pause.reached.send(()).expect("claim pause observer exists");
            pause.resume.recv().expect("claim pause releaser exists");
            pause.claimed
        });

        if self
            .state
            .compare_exchange(
                LEASE_ACTIVE,
                LEASE_CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        }
        #[cfg(test)]
        if let Some(observer) = claim_observer {
            observer.store(true, Ordering::SeqCst);
        }
        // This second sample is the proof that the preceding successful CAS happened before the
        // absolute deadline. A late claim must release authority before any finalizer or value can
        // become observable.
        if Instant::now() >= deadline {
            self.release();
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        }
        Ok(())
    }

    /// Absolute expiry can revoke an unclaimed lease, but it cannot split the final synchronous
    /// handoff after `claim_for_commit` has won.
    fn expire(&self) {
        // A configured test pause retains this per-lease guard through the claim and postcheck,
        // delaying expiry just long enough to exercise a successful but late claim.
        #[cfg(test)]
        let _claim_pause = self.claim_pause.lock().unwrap_or_else(|_| fatal_poison());
        if self
            .state
            .compare_exchange(
                LEASE_ACTIVE,
                LEASE_RELEASED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.release_resources();
        }
    }

    /// Release order is part of the protocol: close the shared flock first, then publish the
    /// in-process count decrement. Neither operation takes an ordinary mutex.
    fn release(&self) {
        if self.state.swap(LEASE_RELEASED, Ordering::AcqRel) != LEASE_RELEASED {
            self.release_resources();
        }
    }

    fn release_resources(&self) {
        let fd = self.fd.swap(-1, Ordering::AcqRel);
        if fd >= 0 {
            // SAFETY: `fd` came from exactly one `File::into_raw_fd`; the atomic swap gives this
            // call its unique owner.
            drop(unsafe { File::from_raw_fd(fd as RawFd) });
            self.boundary.leave();
        }
    }
}

impl Drop for AuthorityLease {
    fn drop(&mut self) {
        self.release();
    }
}

fn fatal_poison() -> ! {
    eprintln!("fatal: MCP security authority mutex poisoned");
    std::process::abort();
}

fn poisoned_lock() -> std::io::Error {
    fatal_poison()
}

fn validate_lock_metadata(meta: &Metadata, expected: Option<FileId>) -> std::io::Result<FileId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o7777 != 0o600
            || meta.nlink() != 1
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "publication lock must be an owner-only regular file with one link",
            ));
        }
    }
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "publication lock is not a regular file",
        ));
    }
    let id = file_id(meta);
    if expected.is_some_and(|expected| expected != id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "publication lock identity changed",
        ));
    }
    Ok(id)
}

fn open_existing_lock(path: &Path, expected: Option<FileId>) -> std::io::Result<(File, FileId)> {
    let mut options = File::options();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let id = validate_lock_metadata(&file.metadata()?, expected)?;
    Ok((file, id))
}

fn legacy_state_needs_boundary(state_dir: &Path) -> bool {
    // The v1/v2 database migration tests model the one supported upgrade path from releases that
    // predate publication.lock. Install it once before either database is migrated. A current v3
    // state with a missing lock is never repaired or re-anchored.
    ["cache.db", "workspace.db"].into_iter().all(|name| {
        Connection::open_with_flags(state_dir.join(name), READ_ONLY_FLAGS)
            .and_then(|db| db.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0)))
            .is_ok_and(|version| (1..3).contains(&version))
    })
}

fn lock_file(file: File, exclusive: bool, blocking: bool) -> std::io::Result<File> {
    let mut op = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };
    if !blocking {
        op |= libc::LOCK_NB;
    }
    use std::os::unix::io::AsRawFd;
    // SAFETY: `file` owns a valid descriptor and `op` is a documented flock operation.
    let rc = unsafe { libc::flock(file.as_raw_fd(), op) };
    if rc == 0 {
        Ok(file)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

impl Boundary {
    /// Construct the one boundary owned by `Store`. This is crate-private so callers cannot forge
    /// a lock authority independently of store initialization.
    pub(crate) fn at(state_dir: &Path) -> Result<Self> {
        let state_dir = state_dir.canonicalize()?;
        ensure!(
            !publication_quarantine()
                .read()
                .unwrap_or_else(|_| fatal_poison())
                .contains(&state_dir),
            "publication authority is quarantined until process restart"
        );
        let dir_anchor = open_no_follow(&state_dir, true)?;
        let dir_meta = dir_anchor.metadata()?;
        ensure!(dir_meta.is_dir(), "state directory must be a directory");
        let lock_path = state_dir.join("publication.lock");

        if !lock_path.exists() {
            // A missing lock is initialized only for a genuinely new state. Existing database
            // state without its original coordination inode fails closed.
            let databases_absent =
                !state_dir.join("cache.db").exists() && !state_dir.join("workspace.db").exists();
            ensure!(
                databases_absent || legacy_state_needs_boundary(&state_dir),
                "existing state is missing publication.lock"
            );
            let mut options = File::options();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
            }
            match options.open(&lock_path) {
                Ok(file) => {
                    validate_lock_metadata(&file.metadata()?, None)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }

        let (lock_anchor, lock_id) = open_existing_lock(&lock_path, None)?;
        let boundary = Self {
            state: Mutex::new(BoundaryState::default()),
            ready: Condvar::new(),
            in_flight: AtomicU32::new(0),
            enrolled: AtomicBool::new(false),
            state_dir,
            dir_id: file_id(&dir_meta),
            lock_path,
            lock_id,
            _dir_anchor: dir_anchor,
            _lock_anchor: lock_anchor,
        };
        boundary.verify_identity()?;
        Ok(boundary)
    }

    fn matches_state(&self, canonical_state: &Path, dir_id: FileId) -> bool {
        self.state_dir == canonical_state && self.dir_id == dir_id
    }

    fn verify_identity(&self) -> std::io::Result<()> {
        if publication_quarantine()
            .read()
            .unwrap_or_else(|_| fatal_poison())
            .contains(&self.state_dir)
        {
            return Err(std::io::Error::other(
                "publication authority is quarantined until process restart",
            ));
        }
        self.verify_paths()
    }

    /// Final response polls may not wait on an ordinary mutex. Unexpected quarantine contention
    /// fails closed there instead of blocking the executor.
    fn verify_identity_nonblocking(&self) -> std::io::Result<()> {
        if publication_quarantine()
            .try_read()
            .map_err(|_| std::io::Error::other("publication quarantine is contended"))?
            .contains(&self.state_dir)
        {
            return Err(std::io::Error::other(
                "publication authority is quarantined until process restart",
            ));
        }
        self.verify_paths()
    }

    fn verify_paths(&self) -> std::io::Result<()> {
        let dir = std::fs::symlink_metadata(&self.state_dir)?;
        if dir.file_type().is_symlink() || !dir.is_dir() || file_id(&dir) != self.dir_id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "state directory identity changed",
            ));
        }
        let lock = std::fs::symlink_metadata(&self.lock_path)?;
        if lock.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "publication lock became a symlink",
            ));
        }
        validate_lock_metadata(&lock, Some(self.lock_id))?;
        Ok(())
    }

    fn open_lock(&self) -> std::io::Result<File> {
        self.verify_identity()?;
        let (file, _) = open_existing_lock(&self.lock_path, Some(self.lock_id))?;
        Ok(file)
    }

    /// Exclusive access for the database publication transaction.
    pub(crate) fn publish(&self) -> std::io::Result<Publishing<'_>> {
        let mut state = self.state.lock().map_err(|_| poisoned_lock())?;
        if state.failed {
            return Err(std::io::Error::other(
                "publication authority is fail-closed",
            ));
        }
        state.waiting = state.waiting.checked_add(1).ok_or_else(poisoned_lock)?;
        loop {
            while state.publishing {
                state = self.ready.wait(state).map_err(|_| poisoned_lock())?;
                if state.failed {
                    state.waiting -= 1;
                    return Err(std::io::Error::other(
                        "publication authority is fail-closed",
                    ));
                }
            }
            if self.in_flight.load(Ordering::Acquire) == 0 {
                break;
            }
            // `waiting > 0` prevents new admissions. Do not require an admission release to take
            // this mutex: final Tower handoff must be able to drop its lease without blocking.
            drop(state);
            thread::yield_now();
            state = self.state.lock().map_err(|_| poisoned_lock())?;
            if state.failed {
                state.waiting -= 1;
                return Err(std::io::Error::other(
                    "publication authority is fail-closed",
                ));
            }
        }
        state.waiting -= 1;
        state.publishing = true;
        drop(state);

        let acquired = self
            .open_lock()
            .and_then(|file| lock_file(file, true, true))
            .and_then(|file| {
                self.verify_identity()?;
                Ok(file)
            });
        match acquired {
            Ok(file) => Ok(Publishing {
                boundary: self,
                cross_process: Some(file),
                transition: PublicationTransition::AbortedBeforeCommit,
            }),
            Err(error) => {
                let mut state = self.state.lock().unwrap_or_else(|_| fatal_poison());
                state.publishing = false;
                self.ready.notify_all();
                Err(error)
            }
        }
    }

    /// Shared admission, bounded by the request's original absolute deadline.
    fn enter(self: &Arc<Self>, deadline: Instant) -> Result<AdmissionPermit, McpError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE))?;
        while state.publishing || state.waiting > 0 {
            if state.failed {
                return Err(McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
            }
            let (next, timeout) = self
                .ready
                .wait_timeout(state, deadline - now)
                .map_err(|_| McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE))?;
            state = next;
            if timeout.timed_out() && (state.publishing || state.waiting > 0) {
                return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
            }
        }
        if state.failed {
            return Err(McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE));
        }
        if Instant::now() >= deadline {
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        }
        self.in_flight
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .map_err(|_| McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE))?;
        drop(state);

        let cross_process = loop {
            let acquired = self
                .open_lock()
                .and_then(|file| lock_file(file, false, false))
                .and_then(|file| {
                    self.verify_identity()?;
                    Ok(file)
                });
            match acquired {
                Ok(file) => break file,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        self.leave();
                        return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
                    }
                    thread::yield_now();
                }
                Err(_) => {
                    self.leave();
                    return Err(McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE));
                }
            }
        };
        Ok(AdmissionPermit {
            boundary: self.clone(),
            cross_process: Some(cross_process),
            active: true,
        })
    }

    fn leave(&self) {
        let previous = self.in_flight.fetch_sub(1, Ordering::AcqRel);
        if previous == 0 {
            fatal_poison();
        }
        self.ready.notify_all();
    }

    fn claim_enrollment(&self) -> Result<()> {
        ensure!(
            self.enrolled
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "publication boundary was already enrolled"
        );
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn hold_state_for_test(
        &self,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) {
        let _guard = self.state.lock().unwrap_or_else(|_| fatal_poison());
        entered.send(()).expect("test contention observer exists");
        release.recv().expect("test contention releaser exists");
    }
}

impl Publishing<'_> {
    /// Mark a known successful commit. Identity is sampled once more before publication reopens.
    pub(crate) fn committed(mut self) -> std::io::Result<()> {
        if let Err(error) = self.boundary.verify_identity() {
            self.transition = PublicationTransition::FailClosed;
            return Err(error);
        }
        self.transition = PublicationTransition::Committed;
        Ok(())
    }

    /// An error from SQLite COMMIT has an ambiguous durable outcome. Retain the exclusive flock and
    /// permanently close this process's authority instead of reopening admission.
    pub(crate) fn fail_closed(mut self) {
        self.transition = PublicationTransition::FailClosed;
    }
}

impl Drop for Publishing<'_> {
    fn drop(&mut self) {
        let mut state = self
            .boundary
            .state
            .lock()
            .unwrap_or_else(|_| fatal_poison());
        if self.transition == PublicationTransition::FailClosed {
            state.failed = true;
            publication_quarantine()
                .write()
                .unwrap_or_else(|_| fatal_poison())
                .insert(self.boundary.state_dir.clone());
            if let Some(file) = self.cross_process.take() {
                // Quarantine an ambiguous COMMIT for the rest of the process, even if every Store
                // and Boundary is later dropped. The OS releases this flock only at process exit.
                std::mem::forget(file);
            }
        }
        state.publishing = false;
        self.boundary.ready.notify_all();
    }
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        if self.active {
            self.active = false;
            drop(self.cross_process.take());
            self.boundary.leave();
        }
    }
}

/// Serializes pilot reads. A queued request keeps its own deadline running and is refused when it
/// elapses, so a waiter can never interrupt the statement currently holding the connection.
struct Gate {
    busy: Mutex<bool>,
    ready: Condvar,
}

struct GateGuard<'a>(&'a Gate);

impl Gate {
    fn new() -> Self {
        Self {
            busy: Mutex::new(false),
            ready: Condvar::new(),
        }
    }
    fn acquire(&self, deadline: Instant) -> Option<GateGuard<'_>> {
        // An already-elapsed deadline is refused even when nothing holds the connection: an idle
        // gate must not resurrect a request whose time is already gone.
        if Instant::now() >= deadline {
            return None;
        }
        let mut busy = self.busy.lock().unwrap_or_else(|_| fatal_poison());
        while *busy {
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (next, timeout) = self
                .ready
                .wait_timeout(busy, deadline - now)
                .unwrap_or_else(|_| fatal_poison());
            busy = next;
            if timeout.timed_out() && *busy {
                return None;
            }
        }
        // A wake can arrive exactly as the deadline passes, whether from a timeout or from the
        // holder releasing. Ownership is granted only if there is still time left.
        if Instant::now() >= deadline {
            return None;
        }
        *busy = true;
        Some(GateGuard(self))
    }
}

impl Drop for GateGuard<'_> {
    fn drop(&mut self) {
        let mut busy = self.0.busy.lock().unwrap_or_else(|_| fatal_poison());
        *busy = false;
        self.0.ready.notify_one();
    }
}

/// Interrupts the enrolled connection when a deadline elapses. Armed only while this request owns
/// the connection, so it can never cancel another request's work.
struct Watchdog {
    state: Arc<(Mutex<bool>, Condvar)>,
    thread: Option<thread::JoinHandle<()>>,
    fired: Arc<AtomicBool>,
}

impl Watchdog {
    fn arm(interrupt: Arc<rusqlite::InterruptHandle>, deadline: Instant) -> Self {
        let state = Arc::new((Mutex::new(false), Condvar::new()));
        let fired = Arc::new(AtomicBool::new(false));
        let (worker_state, worker_fired) = (state.clone(), fired.clone());
        let thread = thread::spawn(move || {
            let (lock, cv) = &*worker_state;
            let mut done = lock.lock().unwrap_or_else(|_| fatal_poison());
            while !*done {
                let now = Instant::now();
                if now >= deadline {
                    worker_fired.store(true, Ordering::SeqCst);
                    interrupt.interrupt();
                    return;
                }
                let (next, _) = cv
                    .wait_timeout(done, deadline - now)
                    .unwrap_or_else(|_| fatal_poison());
                done = next;
            }
        });
        Self {
            state,
            thread: Some(thread),
            fired,
        }
    }
    /// True when the watchdog actually interrupted, which distinguishes a deadline from an
    /// interrupt raised for any other reason.
    fn fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        {
            let (lock, cv) = &*self.state;
            let mut done = lock.lock().unwrap_or_else(|_| fatal_poison());
            *done = true;
            cv.notify_all();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Serializes every authority transition. Waiting is async and deadline-aware; after a guard is
/// acquired, a transition never waits again.
struct Lifecycle {
    serial: Arc<tokio::sync::Mutex<LifecycleState>>,
    latched_fast: AtomicBool,
    generation_fast: AtomicGeneration,
}

struct LifecycleState {
    generation: String,
    latched: bool,
}

/// A tiny seqlock snapshot keeps the existing synchronous evidence API from taking the async
/// lifecycle lock. A torn UUID is never exposed.
struct AtomicGeneration {
    sequence: std::sync::atomic::AtomicU64,
    high: std::sync::atomic::AtomicU64,
    low: std::sync::atomic::AtomicU64,
}

impl AtomicGeneration {
    fn new(value: uuid::Uuid) -> Self {
        let bits = value.as_u128();
        Self {
            sequence: std::sync::atomic::AtomicU64::new(0),
            high: std::sync::atomic::AtomicU64::new((bits >> 64) as u64),
            low: std::sync::atomic::AtomicU64::new(bits as u64),
        }
    }

    fn store(&self, value: uuid::Uuid) {
        self.sequence.fetch_add(1, Ordering::SeqCst);
        let bits = value.as_u128();
        self.high.store((bits >> 64) as u64, Ordering::SeqCst);
        self.low.store(bits as u64, Ordering::SeqCst);
        self.sequence.fetch_add(1, Ordering::SeqCst);
    }

    fn load(&self) -> String {
        loop {
            let before = self.sequence.load(Ordering::SeqCst);
            if before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let high = self.high.load(Ordering::SeqCst);
            let low = self.low.load(Ordering::SeqCst);
            if self.sequence.load(Ordering::SeqCst) == before {
                return uuid::Uuid::from_u128(((high as u128) << 64) | low as u128).to_string();
            }
        }
    }
}

impl Lifecycle {
    fn latch_guarded(&self, state: &mut LifecycleState) {
        self.latched_fast.store(true, Ordering::SeqCst);
        if !state.latched {
            state.latched = true;
            let generation = uuid::Uuid::new_v4();
            state.generation = generation.to_string();
            self.generation_fast.store(generation);
        }
    }

    /// Synchronous identity observers publish failure immediately, then order against the current
    /// lifecycle owner before returning. If a finalizer already owns the sequencer it commits
    /// first; otherwise this latch rotates the generation before any later finalizer can proceed.
    fn latch_fast(&self) {
        self.latched_fast.store(true, Ordering::SeqCst);
        loop {
            if let Ok(mut state) = self.serial.try_lock() {
                self.latch_guarded(&mut state);
                return;
            }
            thread::yield_now();
        }
    }

    fn reconcile_fast(&self, state: &mut LifecycleState) {
        if self.latched_fast.load(Ordering::SeqCst) {
            self.latch_guarded(state);
        }
    }

    async fn acquire(
        self: &Arc<Self>,
        deadline: Instant,
    ) -> Result<tokio::sync::OwnedMutexGuard<LifecycleState>, McpError> {
        if Instant::now() >= deadline {
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        }
        let lock = self.serial.clone().lock_owned();
        let mut state = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), lock)
            .await
            .map_err(|_| McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT))?;
        self.reconcile_fast(&mut state);
        if Instant::now() >= deadline {
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        }
        Ok(state)
    }

    /// Identity failures must be ordered even if their request deadline has elapsed.
    async fn latch_ordered(self: &Arc<Self>) {
        self.latched_fast.store(true, Ordering::SeqCst);
        let mut state = self.serial.clone().lock_owned().await;
        self.latch_guarded(&mut state);
    }
}

/// Everything needed for a fresh identity sample is shared with a pending handoff. This makes a
/// prepared handoff self-authenticating; it cannot be checked against a different `Enrollment`.
struct EnrolledCore {
    lifecycle: Arc<Lifecycle>,
    state_dir: PathBuf,
    cache_path: PathBuf,
    workspace_path: PathBuf,
    dir_id: FileId,
    cache_id: FileId,
    workspace_id: FileId,
    // Pin the enrolled inodes for the binding lifetime.
    _dir: File,
    _cache: File,
    _workspace: File,
    conn: Mutex<Connection>,
    interrupt: Arc<rusqlite::InterruptHandle>,
    gate: Gate,
    admission: Arc<Boundary>,
}

impl EnrolledCore {
    fn is_available(&self) -> bool {
        !self.lifecycle.latched_fast.load(Ordering::SeqCst)
    }

    fn path_identity(&self) -> Result<(), McpError> {
        self.path_identity_with(false)
    }

    fn final_path_identity(&self) -> Result<(), McpError> {
        self.path_identity_with(true)
    }

    fn path_identity_with(&self, nonblocking: bool) -> Result<(), McpError> {
        let unavailable = || McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE);
        if nonblocking {
            self.admission.verify_identity_nonblocking()
        } else {
            self.admission.verify_identity()
        }
        .map_err(|_| unavailable())?;
        for (path, expected) in [
            (&self.state_dir, self.dir_id),
            (&self.cache_path, self.cache_id),
            (&self.workspace_path, self.workspace_id),
        ] {
            let meta = std::fs::symlink_metadata(path).map_err(|_| unavailable())?;
            if meta.file_type().is_symlink() || file_id(&meta) != expected {
                return Err(unavailable());
            }
        }
        Ok(())
    }

    fn identity_locked(&self, conn: &Connection) -> Result<(), McpError> {
        self.path_identity()?;
        if has_moved(conn)? {
            return Err(McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE));
        }
        Ok(())
    }

    /// Take deadline-aware ownership of the enrolled connection for one complete identity sample.
    fn sample_raw(&self, deadline: Instant) -> Result<u64, McpError> {
        if !self.is_available() {
            return Err(McpError::new(ErrorCode::StoreUnavailable, LATCHED));
        }
        let Some(_gate) = self.gate.acquire(deadline) else {
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        };
        if Instant::now() >= deadline {
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        }
        let conn = self
            .conn
            .try_lock()
            .map_err(|_| McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE))?;
        let watchdog = Watchdog::arm(self.interrupt.clone(), deadline);
        let identity = self.identity_locked(&conn);
        let revision = if identity.is_ok() {
            current_revision(&conn)
                .map_err(|_| McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE))
        } else {
            Err(McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE))
        };
        let watchdog_fired = watchdog.fired();
        drop(watchdog);
        let timed_out = watchdog_fired || Instant::now() >= deadline;
        drop(conn);
        identity?;
        if timed_out {
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        }
        revision
    }

    fn verify(&self, deadline: Instant) -> Result<u64, McpError> {
        match self.sample_raw(deadline) {
            Err(error) if error.code == ErrorCode::StoreUnavailable => {
                self.lifecycle.latch_fast();
                Err(error)
            }
            other => other,
        }
    }
}

const AVAILABLE: u8 = 0;
const CLAIMED: u8 = 1;

struct OneShot<S> {
    claim: AtomicU8,
    state: Mutex<Option<S>>,
}

impl<S> OneShot<S> {
    fn new(state: S) -> Self {
        Self {
            claim: AtomicU8::new(AVAILABLE),
            state: Mutex::new(Some(state)),
        }
    }

    fn claim(&self) -> Result<S, McpError> {
        match self
            .claim
            .compare_exchange(AVAILABLE, CLAIMED, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Ok(self
                .state
                .lock()
                .unwrap_or_else(|_| fatal_poison())
                .take()
                .expect("an available one-shot contains state")),
            Err(_) => Err(McpError::new(ErrorCode::StoreUnavailable, CONSUMED)),
        }
    }
}

struct ExpiryJob {
    deadline: Instant,
    target: std::sync::Weak<AuthorityLease>,
}

static EXPIRY_SERVICE: OnceLock<std::sync::mpsc::Sender<ExpiryJob>> = OnceLock::new();

fn schedule_expiry(deadline: Instant, authority: &Arc<AuthorityLease>) {
    let sender = EXPIRY_SERVICE.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::channel::<ExpiryJob>();
        thread::Builder::new()
            .name("mcp-authority-expiry".into())
            .spawn(move || {
                let mut jobs: Vec<ExpiryJob> = Vec::new();
                loop {
                    let now = Instant::now();
                    let mut future = Vec::with_capacity(jobs.len());
                    for job in jobs.drain(..) {
                        if job.deadline <= now {
                            if let Some(target) = job.target.upgrade() {
                                target.expire();
                            }
                        } else {
                            future.push(job);
                        }
                    }
                    jobs = future;
                    jobs.sort_unstable_by_key(|job| job.deadline);
                    let message = match jobs.first() {
                        Some(job) => receiver
                            .recv_timeout(job.deadline.saturating_duration_since(Instant::now())),
                        None => receiver
                            .recv()
                            .map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected),
                    };
                    match message {
                        Ok(job) => jobs.push(job),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            })
            .unwrap_or_else(|_| fatal_poison());
        sender
    });
    // The process-owned sender cannot disconnect while the process lives. Failure is fatal because
    // accepting an unbounded publication lease would violate the authority contract.
    sender
        .send(ExpiryJob {
            deadline,
            target: Arc::downgrade(authority),
        })
        .unwrap_or_else(|_| fatal_poison());
}

struct HandoffState<T> {
    value: Option<T>,
    core: Arc<EnrolledCore>,
    admitted_revision: u64,
    deadline: Instant,
    authority: Arc<AuthorityLease>,
}

impl<T> Drop for HandoffState<T> {
    fn drop(&mut self) {
        // Reopen publication before Rust drops arbitrary generic `T`. A blocking or reentrant
        // destructor can retain inert memory, but it can never retain publication authority.
        self.authority.release();
    }
}

/// Opaque result of one enrolled read. Evidence cannot leave this proof directly; it must be
/// consumed by [`Enrollment::admit_snapshot`], which rechecks its enrollment and revision.
pub(crate) struct Snapshot<T> {
    value: Option<T>,
    core: Arc<EnrolledCore>,
    revision: u64,
    deadline: Instant,
}

impl<T> std::fmt::Debug for Snapshot<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Snapshot(..)")
    }
}

/// Opaque admitted value before the last blocking store sample.
pub(crate) struct Pending<T> {
    state: Arc<OneShot<HandoffState<T>>>,
}

impl<T> std::fmt::Debug for Pending<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pending(..)")
    }
}

/// Opaque, cloneable response-extension carrier after all blocking store work is complete.
/// Clones share one value and one exact admission permit.
pub(crate) struct Prepared<T> {
    state: Arc<OneShot<HandoffState<T>>>,
}

impl<T> Clone for Prepared<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
        }
    }
}

impl<T> std::fmt::Debug for Prepared<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Prepared(..)")
    }
}

#[cfg(test)]
impl<T> Prepared<T> {
    /// Install a per-lease rendezvous immediately after the claim deadline precheck and before its
    /// CAS. The returned receiver observes the pause; sending on the returned sender resumes it.
    pub(crate) fn pause_claim_after_precheck(
        &self,
    ) -> (
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::SyncSender<()>,
        Arc<AtomicBool>,
    ) {
        let handoff = self.state.state.lock().unwrap_or_else(|_| fatal_poison());
        let authority = &handoff
            .as_ref()
            .expect("test installs claim pause before consuming Prepared")
            .authority;
        let (reached, observed) = std::sync::mpsc::sync_channel(0);
        let (resume, resumed) = std::sync::mpsc::sync_channel(0);
        let claimed = Arc::new(AtomicBool::new(false));
        let mut pause = authority
            .claim_pause
            .lock()
            .unwrap_or_else(|_| fatal_poison());
        assert!(
            pause.is_none(),
            "claim pause already installed for this lease"
        );
        *pause = Some(ClaimPause {
            reached,
            resume: resumed,
            claimed: claimed.clone(),
        });
        (observed, resume, claimed)
    }
}

/// Read-only proof that a caller is inside lifecycle serialization.
pub(crate) struct LifecycleView<'a> {
    state: &'a LifecycleState,
    _serial: PhantomData<&'a mut ()>,
    _not_send: PhantomData<Rc<()>>,
}

impl LifecycleView<'_> {
    pub(crate) fn store_generation(&self) -> &str {
        &self.state.generation
    }

    pub(crate) fn is_available(&self) -> bool {
        !self.state.latched
    }

    pub(crate) fn require_available(&self) -> Result<(), McpError> {
        if self.is_available() {
            Ok(())
        } else {
            Err(McpError::new(ErrorCode::StoreUnavailable, LATCHED))
        }
    }
}

/// Read-only final-handoff context valid only during the prepare callback.
pub(crate) struct HandoffView<'a> {
    lifecycle: LifecycleView<'a>,
    #[allow(dead_code)] // consumed by the typed PR2 response finalizers
    revision: u64,
}

impl HandoffView<'_> {
    pub(crate) fn lifecycle(&self) -> &LifecycleView<'_> {
        &self.lifecycle
    }

    #[allow(dead_code)] // consumed by the typed PR2 response finalizers
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
}

/// Unforgeable proof supplied only to an infallible final authority callback.
pub(crate) struct LifecycleCommit<'a> {
    _serial: PhantomData<&'a mut ()>,
    _not_send: PhantomData<Rc<()>>,
}

fn abort_on_finalizer_panic<R>(f: impl FnOnce() -> R) -> R {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_) => {
            eprintln!("fatal: MCP authority finalizer panicked");
            std::process::abort();
        }
    }
}

impl<T: Send + 'static> Pending<T> {
    /// Perform the last connection, path, `HAS_MOVED`, and revision sample on Tokio's blocking
    /// pool while retaining the exact expiring authority lease.
    pub(crate) async fn prepare_handoff(self) -> Result<Prepared<T>, McpError> {
        let state = self.state.claim()?;
        let lifecycle = state.core.lifecycle.clone();
        if !state.authority.is_active() || Instant::now() >= state.deadline {
            state.authority.release();
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        }
        let sampled = tokio::task::spawn_blocking(move || {
            let result = if !state.authority.is_active() || Instant::now() >= state.deadline {
                Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT))
            } else {
                match state.core.sample_raw(state.deadline) {
                    Ok(revision) if revision != state.admitted_revision => {
                        Err(McpError::new(ErrorCode::RevisionConflict, CONFLICT))
                    }
                    Ok(_) if !state.authority.is_active() || Instant::now() >= state.deadline => {
                        Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT))
                    }
                    Ok(_) => Ok(()),
                    Err(error) => Err(error),
                }
            };
            if result
                .as_ref()
                .is_err_and(|error| error.code == ErrorCode::StoreUnavailable)
            {
                // Publication must not wait behind lifecycle ordering. Expiry and this decided
                // failure both revoke the authority before the synchronous latch can wait.
                state.authority.release();
                state.core.lifecycle.latch_fast();
            }
            (state, result)
        })
        .await;

        let (state, result) = match sampled {
            Ok(result) => result,
            Err(join) if join.is_panic() => {
                lifecycle.latch_ordered().await;
                std::panic::resume_unwind(join.into_panic());
            }
            Err(_) => {
                lifecycle.latch_ordered().await;
                return Err(McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE));
            }
        };
        if let Err(error) = result {
            state.authority.release();
            return Err(error);
        }

        let shared = Arc::new(OneShot::new(state));
        Ok(Prepared { state: shared })
    }
}

impl<T: Send + 'static> Prepared<T> {
    pub(crate) async fn commit(self) -> Result<T, McpError> {
        self.commit_with(|view| view.lifecycle().require_available(), |_, ()| {})
            .await
    }

    /// Wait for lifecycle serialization, then synchronously validate, finalize, and reveal.
    pub(crate) async fn commit_with<Plan, Prepare, Finalize>(
        self,
        prepare: Prepare,
        finalize: Finalize,
    ) -> Result<T, McpError>
    where
        Prepare: for<'a> FnOnce(&HandoffView<'a>) -> Result<Plan, McpError> + Send,
        Finalize: for<'a> FnOnce(LifecycleCommit<'a>, Plan) + Send,
    {
        let mut handoff = self.state.claim()?;
        let lifecycle = handoff.core.lifecycle.clone();
        let mut guard = lifecycle.acquire(handoff.deadline).await?;

        let first_identity = handoff.core.final_path_identity();
        if first_identity.is_err() {
            lifecycle.latch_guarded(&mut guard);
            return Err(McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE));
        }
        if guard.latched || lifecycle.latched_fast.load(Ordering::SeqCst) {
            lifecycle.latch_guarded(&mut guard);
            return Err(McpError::new(ErrorCode::StoreUnavailable, LATCHED));
        }
        if Instant::now() >= handoff.deadline {
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        }

        let view = HandoffView {
            lifecycle: LifecycleView {
                state: &guard,
                _serial: PhantomData,
                _not_send: PhantomData,
            },
            revision: handoff.admitted_revision,
        };
        let prepared = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| prepare(&view)));
        let identity = handoff.core.final_path_identity();
        if identity.is_err() || lifecycle.latched_fast.load(Ordering::SeqCst) {
            lifecycle.latch_guarded(&mut guard);
            return match prepared {
                Err(payload) => {
                    drop(guard);
                    std::panic::resume_unwind(payload)
                }
                Ok(_) => Err(McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE)),
            };
        }
        if Instant::now() >= handoff.deadline {
            return match prepared {
                Err(payload) => {
                    lifecycle.latch_guarded(&mut guard);
                    drop(guard);
                    std::panic::resume_unwind(payload)
                }
                Ok(_) => Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT)),
            };
        }
        let plan = match prepared {
            Ok(Ok(plan)) => plan,
            Ok(Err(error)) => return Err(error),
            Err(payload) => {
                lifecycle.latch_guarded(&mut guard);
                drop(guard);
                std::panic::resume_unwind(payload)
            }
        };

        // Atomically order absolute expiry against the synchronous finalizer. If expiry won, no
        // protected value can be revealed. If this claim wins, the trusted crate-private finalizer
        // runs without waiting and releases the lease before this future returns Ready.
        handoff.authority.claim_for_commit(handoff.deadline)?;
        let value = handoff.value.take().expect("prepared value is present");
        abort_on_finalizer_panic(|| {
            finalize(
                LifecycleCommit {
                    _serial: PhantomData,
                    _not_send: PhantomData,
                },
                plan,
            )
        });
        drop(guard);
        handoff.authority.release();
        Ok(value)
    }
}

/// One enrolled binding to one store. Created at daemon startup, never re-enrolled.
pub(crate) struct Enrollment {
    #[allow(dead_code)] // included in typed PR2 EvidenceBasis values
    daemon_instance_id: String,
    core: Arc<EnrolledCore>,
}

fn open_no_follow(path: &Path, directory: bool) -> Result<File> {
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut flags = libc::O_NOFOLLOW | libc::O_NONBLOCK;
        if directory {
            flags |= libc::O_DIRECTORY;
        }
        options.custom_flags(flags);
    }
    let _ = directory;
    options
        .open(path)
        .with_context(|| format!("open {}", path.display()))
}

/// `sqlite3_file_control(db, "main", SQLITE_FCNTL_HAS_MOVED, &moved)`.
///
/// Asks the VFS whether the database it actually opened still resolves from its pathname. An
/// unsupported control, an error, or an out-of-range result fails closed.
fn has_moved(conn: &Connection) -> Result<bool, McpError> {
    let mut moved: c_int = -1;
    let name = CString::new("main").expect("literal contains no NUL");
    // SAFETY: `conn` is borrowed for the whole call, so its sqlite3 handle stays live. This file
    // control reads the zero-terminated schema name and writes exactly one c_int through the out
    // pointer, which `moved` provides. The value is only trusted when the call returns SQLITE_OK.
    let rc = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            conn.handle(),
            name.as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
            std::ptr::from_mut(&mut moved).cast::<c_void>(),
        )
    };
    if rc != rusqlite::ffi::SQLITE_OK || moved < 0 {
        return Err(McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE));
    }
    Ok(moved != 0)
}

impl Enrollment {
    /// Enroll once, after ordinary store initialization and before any grant may be issued.
    pub(crate) fn enroll(state_dir: &Path, publication: Arc<Boundary>) -> Result<Self> {
        ensure!(
            cfg!(unix),
            "the MCP pilot binding requires Unix file identities"
        );
        let state_dir = state_dir
            .canonicalize()
            .context("resolve the canonical state directory")?;
        let cache_path = state_dir.join("cache.db");
        let workspace_path = state_dir.join("workspace.db");

        let dir = open_no_follow(&state_dir, true)?;
        let cache = open_no_follow(&cache_path, false)?;
        let workspace = open_no_follow(&workspace_path, false)?;
        let (dir_meta, cache_meta, workspace_meta) =
            (dir.metadata()?, cache.metadata()?, workspace.metadata()?);
        ensure!(dir_meta.is_dir(), "state directory must be a directory");
        ensure!(cache_meta.is_file(), "cache must be a regular file");
        ensure!(workspace_meta.is_file(), "workspace must be a regular file");
        ensure!(
            publication.matches_state(&state_dir, file_id(&dir_meta)),
            "publication boundary belongs to a different state directory"
        );
        publication
            .verify_identity()
            .context("verify publication lock identity")?;
        // The claim is permanent for this Boundary, including after the Enrollment is dropped.
        // A daemon/store boundary has exactly one lifecycle and can never mint a bypass authority.
        publication.claim_enrollment()?;

        let conn = Connection::open_with_flags(&cache_path, READ_ONLY_FLAGS)
            .context("open the bound cache read-only")?;
        conn.query_row("SELECT count(*) FROM sqlite_schema", [], |r| {
            r.get::<_, i64>(0)
        })
        .context("establish read-only access to the bound cache")?;

        let initial_generation = uuid::Uuid::new_v4();
        let lifecycle = Arc::new(Lifecycle {
            serial: Arc::new(tokio::sync::Mutex::new(LifecycleState {
                generation: initial_generation.to_string(),
                latched: false,
            })),
            latched_fast: AtomicBool::new(false),
            generation_fast: AtomicGeneration::new(initial_generation),
        });
        let core = Arc::new(EnrolledCore {
            lifecycle,
            state_dir,
            cache_path,
            workspace_path,
            dir_id: file_id(&dir_meta),
            cache_id: file_id(&cache_meta),
            workspace_id: file_id(&workspace_meta),
            _dir: dir,
            _cache: cache,
            _workspace: workspace,
            interrupt: Arc::new(conn.get_interrupt_handle()),
            conn: Mutex::new(conn),
            gate: Gate::new(),
            admission: publication,
        });
        // Setup is not request work, but still uses the same ownership discipline. The open above
        // has already bounded SQLite setup; this sample mainly closes the setup race.
        core.verify(Instant::now() + std::time::Duration::from_secs(5))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(Self {
            daemon_instance_id: uuid::Uuid::new_v4().to_string(),
            core,
        })
    }

    #[allow(dead_code)] // included in typed PR2 EvidenceBasis values
    pub(crate) fn daemon_instance_id(&self) -> String {
        self.daemon_instance_id.clone()
    }

    pub(crate) fn store_generation(&self) -> String {
        self.core.lifecycle.generation_fast.load()
    }

    pub(crate) fn is_available(&self) -> bool {
        self.core.is_available()
    }

    /// Latch and rotate the binding, then run infallible authority teardown under the same
    /// lifecycle sequencer.
    pub(crate) async fn invalidate_with<R, Finalize>(
        &self,
        deadline: Instant,
        finalize: Finalize,
    ) -> Result<R, McpError>
    where
        Finalize: for<'a> FnOnce(LifecycleCommit<'a>) -> R + Send,
    {
        let lifecycle = self.core.lifecycle.clone();
        let mut guard = lifecycle.acquire(deadline).await?;
        lifecycle.latch_guarded(&mut guard);
        let result = abort_on_finalizer_panic(|| {
            finalize(LifecycleCommit {
                _serial: PhantomData,
                _not_send: PhantomData,
            })
        });
        drop(guard);
        Ok(result)
    }

    pub(crate) async fn invalidate(&self, deadline: Instant) -> Result<(), McpError> {
        self.invalidate_with(deadline, |_| ()).await
    }

    /// Serialize revoke, expiry, issuance, and budget mutation with response finalization.
    pub(crate) async fn lifecycle_transition<Plan, R, Prepare, Finalize>(
        &self,
        deadline: Instant,
        prepare: Prepare,
        finalize: Finalize,
    ) -> Result<R, McpError>
    where
        Prepare: for<'a> FnOnce(&LifecycleView<'a>) -> Result<Plan, McpError> + Send,
        Finalize: for<'a> FnOnce(LifecycleCommit<'a>, Plan) -> R + Send,
    {
        let lifecycle = self.core.lifecycle.clone();
        let mut guard = lifecycle.acquire(deadline).await?;
        let available_on_entry = !guard.latched;
        let view = LifecycleView {
            state: &guard,
            _serial: PhantomData,
            _not_send: PhantomData,
        };
        let prepared = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| prepare(&view)));
        lifecycle.reconcile_fast(&mut guard);
        if available_on_entry && guard.latched {
            return match prepared {
                Err(payload) => {
                    drop(guard);
                    std::panic::resume_unwind(payload)
                }
                Ok(_) => Err(McpError::new(ErrorCode::StoreUnavailable, LATCHED)),
            };
        }
        if Instant::now() >= deadline {
            return match prepared {
                Err(payload) => {
                    lifecycle.latch_guarded(&mut guard);
                    drop(guard);
                    std::panic::resume_unwind(payload)
                }
                Ok(_) => Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT)),
            };
        }
        let plan = match prepared {
            Ok(Ok(plan)) => plan,
            Ok(Err(error)) => return Err(error),
            Err(payload) => {
                lifecycle.latch_guarded(&mut guard);
                drop(guard);
                std::panic::resume_unwind(payload)
            }
        };
        let result = abort_on_finalizer_panic(|| {
            finalize(
                LifecycleCommit {
                    _serial: PhantomData,
                    _not_send: PhantomData,
                },
                plan,
            )
        });
        drop(guard);
        Ok(result)
    }

    /// Consume an enrollment-owned read proof and admit a derived inert value. Callers cannot
    /// forge or choose the proof's enrollment, revision, or original deadline association.
    pub(crate) fn admit_snapshot<T, U: Send + 'static>(
        &self,
        mut snapshot: Snapshot<T>,
        produce: impl FnOnce(u64, T) -> U,
    ) -> Result<Pending<U>, McpError> {
        if !Arc::ptr_eq(&self.core, &snapshot.core) {
            return Err(McpError::new(
                ErrorCode::BindingMismatch,
                "The evidence binding does not match",
            ));
        }
        let deadline = snapshot.deadline;
        let (permit, current) = self.begin_admission(deadline)?;
        if current != snapshot.revision {
            // Inert evidence is destroyed while publication is still excluded.
            drop(snapshot);
            drop(permit);
            return Err(McpError::new(ErrorCode::RevisionConflict, CONFLICT));
        }
        let value = snapshot.value.take().expect("snapshot value is present");
        let produced = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            produce(current, value)
        })) {
            Ok(produced) => produced,
            Err(payload) => {
                drop(permit);
                self.core.lifecycle.latch_fast();
                std::panic::resume_unwind(payload);
            }
        };
        self.finish_admission(produced, permit, current, deadline)
    }

    pub(crate) fn admit_current<T: Send + 'static>(
        &self,
        deadline: Instant,
        produce: impl FnOnce(u64) -> T,
    ) -> Result<Pending<T>, McpError> {
        let (permit, current) = self.begin_admission(deadline)?;
        let produced =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| produce(current))) {
                Ok(produced) => produced,
                Err(payload) => {
                    drop(permit);
                    self.core.lifecycle.latch_fast();
                    std::panic::resume_unwind(payload);
                }
            };
        self.finish_admission(produced, permit, current, deadline)
    }

    fn begin_admission(&self, deadline: Instant) -> Result<(AdmissionPermit, u64), McpError> {
        let permit = match self.core.admission.enter(deadline) {
            Ok(permit) => permit,
            Err(error) => {
                if error.code == ErrorCode::StoreUnavailable {
                    self.core.lifecycle.latch_fast();
                }
                return Err(error);
            }
        };
        let current = match self.core.sample_raw(deadline) {
            Err(error) if error.code == ErrorCode::StoreUnavailable => {
                drop(permit);
                self.core.lifecycle.latch_fast();
                return Err(error);
            }
            Err(error) => return Err(error),
            Ok(current) => current,
        };
        if Instant::now() >= deadline {
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        }
        Ok((permit, current))
    }

    fn finish_admission<T: Send + 'static>(
        &self,
        produced: T,
        permit: AdmissionPermit,
        before: u64,
        deadline: Instant,
    ) -> Result<Pending<T>, McpError> {
        let authority = AuthorityLease::from_permit(permit);
        schedule_expiry(deadline, &authority);
        let state = HandoffState {
            value: Some(produced),
            core: self.core.clone(),
            admitted_revision: before,
            deadline,
            authority,
        };
        let outcome = match self.core.sample_raw(deadline) {
            Ok(after) if after != before => {
                Err(McpError::new(ErrorCode::RevisionConflict, CONFLICT))
            }
            Ok(_) if Instant::now() >= deadline => {
                Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT))
            }
            Ok(_) => Ok(()),
            Err(error) if error.code == ErrorCode::StoreUnavailable => {
                state.authority.release();
                self.core.lifecycle.latch_fast();
                Err(error)
            }
            Err(error) => Err(error),
        };
        outcome?;
        let shared = Arc::new(OneShot::new(state));
        Ok(Pending { state: shared })
    }

    /// Run one guarded read. Rollback and restored autocommit are mandatory before any evidence can
    /// be returned. A callback panic is contained until cleanup completes, then invalidates and is
    /// resumed without poisoning the connection mutex.
    pub(crate) fn read<T>(
        &self,
        expected: Option<u64>,
        deadline: Instant,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<Snapshot<T>, McpError> {
        if !self.is_available() {
            return Err(McpError::new(ErrorCode::StoreUnavailable, LATCHED));
        }
        let Some(_gate) = self.core.gate.acquire(deadline) else {
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        };
        let conn = match self.core.conn.try_lock() {
            Ok(conn) => conn,
            Err(_) => {
                self.core.lifecycle.latch_fast();
                return Err(McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE));
            }
        };
        let watchdog = Watchdog::arm(self.core.interrupt.clone(), deadline);
        let mut identity_error = self.core.identity_locked(&conn).err();
        let mut began = false;
        let mut callback_panic = None;
        let mut outcome: Option<Result<(u64, T), McpError>> = None;

        if identity_error.is_none() {
            match conn.execute_batch("BEGIN DEFERRED") {
                Ok(()) => {
                    began = true;
                    match current_revision(&conn) {
                        Ok(revision) if expected.is_some_and(|value| value != revision) => {
                            outcome =
                                Some(Err(McpError::new(ErrorCode::RevisionConflict, CONFLICT)));
                        }
                        Ok(revision) => {
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                f(&conn)
                            })) {
                                Ok(Ok(value)) => outcome = Some(Ok((revision, value))),
                                Ok(Err(_)) => {
                                    outcome = Some(Err(McpError::new(
                                        ErrorCode::StoreUnavailable,
                                        UNAVAILABLE,
                                    )));
                                }
                                Err(payload) => callback_panic = Some(payload),
                            }
                        }
                        Err(_) => {
                            outcome =
                                Some(Err(McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE)));
                        }
                    }
                }
                Err(_) => {
                    outcome = Some(Err(McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE)));
                }
            }
        }

        let rollback_ok =
            !began || (conn.execute_batch("ROLLBACK").is_ok() && conn.is_autocommit());
        if let Err(error) = self.core.identity_locked(&conn) {
            identity_error = Some(error);
        }
        let watchdog_fired = watchdog.fired();
        drop(watchdog);
        let timed_out = watchdog_fired || Instant::now() >= deadline;
        drop(conn);

        if callback_panic.is_some() || !rollback_ok || identity_error.is_some() {
            self.core.lifecycle.latch_fast();
        }
        if let Some(payload) = callback_panic {
            std::panic::resume_unwind(payload);
        }
        if !rollback_ok || identity_error.is_some() {
            return Err(McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE));
        }
        if timed_out {
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        }
        outcome
            .expect("a completed transaction has an outcome")
            .map(|(revision, value)| Snapshot {
                value: Some(value),
                core: self.core.clone(),
                revision,
                deadline,
            })
    }

    pub(crate) fn current_revision(&self, deadline: Instant) -> Result<u64, McpError> {
        self.core.verify(deadline)
    }
}

/// Mirrors the revision projection of `Store::read_status`: an absent singleton row is revision 0,
/// which is an unindexed store rather than a missing one.
fn current_revision(conn: &Connection) -> rusqlite::Result<u64> {
    use rusqlite::OptionalExtension;
    // Only an absent singleton row is an unindexed store. A missing table, or a row that will not
    // convert, is a storage failure and must propagate rather than read as revision zero.
    let revision: Option<i64> = conn
        .query_row("SELECT revision FROM revision WHERE singleton=1", [], |r| {
            r.get(0)
        })
        .optional()?;
    // Zero is reserved for an absent row. A stored negative is a corrupt store, not revision zero.
    match revision {
        None => Ok(0),
        Some(value) => {
            u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The anchor descriptors cannot protect SQLite's own open, so the flag has to do it. This
    /// asserts the platform behavior the setup-race defence depends on.
    #[test]
    #[cfg(unix)]
    fn the_enrolled_flags_refuse_a_symlinked_database() {
        let dir = tempfile::tempdir().unwrap();
        // The flag rejects a symbolic link anywhere in the path, and temporary directories often
        // sit behind one, so compare against a canonical base.
        let base = dir.path().canonicalize().unwrap();
        let target = base.join("target.db");
        Connection::open(&target)
            .unwrap()
            .execute_batch("CREATE TABLE t(x)")
            .unwrap();
        let link = base.join("cache.db");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(
            Connection::open_with_flags(&link, READ_ONLY_FLAGS).is_err(),
            "a symlinked database must not be opened"
        );
        // Following the link would have created sidecars beside the target.
        assert!(!base.join("target.db-wal").exists());
        assert!(!base.join("target.db-shm").exists());

        // The same path opens when it is a regular file, so the refusal is the symlink, not the flags.
        assert!(Connection::open_with_flags(&target, READ_ONLY_FLAGS).is_ok());
    }
}
