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

use super::{ErrorCode, EvidenceBasis, McpError};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags};
use std::{
    ffi::CString,
    fs::{File, Metadata},
    os::raw::{c_int, c_void},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Instant,
};

const UNAVAILABLE: &str = "The bound store is unavailable";
const LATCHED: &str = "The bound store was invalidated; restart and owner reissue are required";
const TIMED_OUT: &str = "The operation deadline elapsed";
const CONFLICT: &str = "The index revision changed";

/// Flags for the enrolled connection. `SQLITE_OPEN_NOFOLLOW` matters independently of the anchor
/// descriptors: if the cache pathname becomes a symlink between anchor acquisition and this open,
/// SQLite would otherwise follow it and create WAL/SHM beside the target before the identity check
/// rejects enrollment.
pub(crate) const READ_ONLY_FLAGS: OpenFlags = OpenFlags::SQLITE_OPEN_READ_ONLY
    .union(OpenFlags::SQLITE_OPEN_NO_MUTEX)
    .union(OpenFlags::SQLITE_OPEN_NOFOLLOW);

/// The publication boundary, shared between the store and the MCP pilot.
///
/// Publication is exclusive and waits for every in-flight response to finish; admission is shared
/// and hands back a ticket that keeps publication waiting until the response has been handed to
/// the server. Checking for a change after building a response cannot work on its own, because the
/// check necessarily ends before the response is emitted. Holding a ticket across that gap is what
/// makes admission and emission atomic with respect to publication.
#[derive(Debug)]
pub struct Boundary {
    state: Mutex<BoundaryState>,
    ready: Condvar,
    /// Advisory lock file in the state directory. The in-process state coordinates this process's
    /// own threads; a second `Store` in another process shares nothing but the filesystem, so the
    /// same shared/exclusive discipline is taken out on this file as well.
    lock_path: PathBuf,
}

#[derive(Debug, Default)]
struct BoundaryState {
    publishing: bool,
    in_flight: u32,
    /// Publishers waiting for the boundary. Admission yields to them, so a steady stream of
    /// responses cannot starve publication indefinitely.
    waiting: u32,
}

/// Held by publication for the whole of its commit.
pub struct Publishing<'a> {
    boundary: &'a Boundary,
    _cross_process: File,
}

/// Held by an admitted response until it has been handed to the server. `Send` on purpose: it
/// travels with the response across the await that follows admission.
pub struct Ticket {
    boundary: Arc<Boundary>,
    _cross_process: Option<File>,
    consumed: bool,
}

/// Shared or exclusive advisory lock on the boundary file, released when the handle is dropped.
fn flock(path: &Path, exclusive: bool, blocking: bool) -> std::io::Result<File> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let mut op = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };
    if !blocking {
        op |= libc::LOCK_NB;
    }
    // SAFETY: the descriptor is owned by `file` and valid for this call, and the operation is one
    // of flock's documented constants. Closing the file releases the lock.
    let rc = unsafe { libc::flock(std::os::unix::io::AsRawFd::as_raw_fd(&file), op) };
    if rc == 0 {
        Ok(file)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

impl Boundary {
    pub fn at(state_dir: &Path) -> Self {
        Self {
            state: Mutex::new(BoundaryState::default()),
            ready: Condvar::new(),
            lock_path: state_dir.join("publication.lock"),
        }
    }

    /// Exclusive access for publication. Waits for in-flight responses rather than racing them.
    pub fn publish(&self) -> std::io::Result<Publishing<'_>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.waiting += 1;
        while state.publishing || state.in_flight > 0 {
            state = self.ready.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        state.waiting -= 1;
        state.publishing = true;
        drop(state);
        // Blocks until every other process's in-flight response has finished. A lock that cannot
        // be taken fails the publication: continuing would silently drop cross-process
        // coordination and let a separate Store commit under a live ticket.
        match flock(&self.lock_path, true, true) {
            Ok(file) => Ok(Publishing {
                boundary: self,
                _cross_process: file,
            }),
            Err(e) => {
                let mut state = self.state.lock().unwrap_or_else(|x| x.into_inner());
                state.publishing = false;
                self.ready.notify_all();
                Err(e)
            }
        }
    }

    /// Shared access for admission, bounded by the caller's deadline. Contention is an elapsed
    /// deadline, never a revision conflict: a concurrent admission is not a publication.
    fn enter(self: &Arc<Self>, deadline: Instant) -> Result<Ticket, McpError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // Yields to a waiting publisher, so admissions cannot starve publication.
        while state.publishing || state.waiting > 0 {
            let now = Instant::now();
            if now >= deadline {
                return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
            }
            let (next, timeout) = self
                .ready
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|e| e.into_inner());
            state = next;
            if timeout.timed_out() && (state.publishing || state.waiting > 0) {
                return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
            }
        }
        if Instant::now() >= deadline {
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        }
        state.in_flight += 1;
        drop(state);
        // Shared, and bounded by the caller's deadline rather than blocking: a publication in
        // another process holds the exclusive side while it commits.
        let cross_process = loop {
            match flock(&self.lock_path, false, false) {
                Ok(file) => break Some(file),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        self.leave();
                        return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                // Any other failure means the lock is unusable, so cross-process coordination is
                // not in force. Admitting anyway would hand out a ticket that cannot hold off a
                // separate Store, so the request is refused instead.
                Err(_) => {
                    self.leave();
                    return Err(McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE));
                }
            }
        };
        Ok(Ticket {
            boundary: self.clone(),
            _cross_process: cross_process,
            consumed: false,
        })
    }

    fn leave(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.in_flight = state.in_flight.saturating_sub(1);
        self.ready.notify_all();
    }
}

impl Drop for Publishing<'_> {
    fn drop(&mut self) {
        let mut state = self
            .boundary
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.publishing = false;
        self.boundary.ready.notify_all();
    }
}

impl Ticket {
    /// Commit this response for emission, or refuse it.
    ///
    /// Consumes the ticket while holding the boundary's state lock, which invalidation also takes
    /// before setting its flag. The check and the release are therefore one step: an invalidation
    /// either happens before it, and the response is refused, or after it, once the response is
    /// already committed. A separate check followed by a release leaves a window between them for
    /// exactly the schedule this avoids.
    pub fn commit(mut self, enrollment: &Enrollment) -> Result<(), McpError> {
        let mut state = self
            .boundary
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let available = enrollment.is_available();
        state.in_flight = state.in_flight.saturating_sub(1);
        self.consumed = true;
        self.boundary.ready.notify_all();
        drop(state);
        if available {
            Ok(())
        } else {
            Err(McpError::new(ErrorCode::StoreUnavailable, LATCHED))
        }
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if !self.consumed {
            self.boundary.leave();
        }
    }
}

impl std::fmt::Debug for Ticket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ticket")
    }
}

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
        let mut busy = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        while *busy {
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (next, timeout) = self
                .ready
                .wait_timeout(busy, deadline - now)
                .unwrap_or_else(|e| e.into_inner());
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
        let mut busy = self.0.busy.lock().unwrap_or_else(|e| e.into_inner());
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
            let mut done = lock.lock().unwrap_or_else(|e| e.into_inner());
            while !*done {
                let now = Instant::now();
                if now >= deadline {
                    worker_fired.store(true, Ordering::SeqCst);
                    interrupt.interrupt();
                    return;
                }
                let (next, _) = cv
                    .wait_timeout(done, deadline - now)
                    .unwrap_or_else(|e| e.into_inner());
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
            let mut done = lock.lock().unwrap_or_else(|e| e.into_inner());
            *done = true;
            cv.notify_all();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// One enrolled binding to one store. Created at daemon startup, never re-enrolled.
pub struct Enrollment {
    daemon_instance_id: String,
    generation: Mutex<String>,
    state_dir: PathBuf,
    cache_path: PathBuf,
    workspace_path: PathBuf,
    dir_id: FileId,
    cache_id: FileId,
    workspace_id: FileId,
    // Retained so the anchored inodes cannot be recycled while this binding lives. SQLite does not
    // read through these descriptors; they pin identity, they do not supply the connection.
    _dir: File,
    _cache: File,
    _workspace: File,
    conn: Mutex<Connection>,
    interrupt: Arc<rusqlite::InterruptHandle>,
    gate: Gate,
    latched: AtomicBool,
    /// One boundary shared with `Store::publish`, so admission, every latching path and
    /// publication are mutually exclusive: a response or credential can never be produced from a
    /// snapshot that is being replaced or that another request has observed to be gone.
    ///
    /// Lock order is always this mutex before `conn`. A latching path must therefore hold no
    /// connection when it takes it, which is why `read` releases the connection first.
    admission: Arc<Boundary>,
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
    ///
    /// Succeeds against an unindexed store: `cache.db` exists with revision 0, which the grant
    /// issuer reports as `no_published_index` rather than a storage failure.
    pub fn enroll(state_dir: &Path, publication: Arc<Boundary>) -> Result<Self> {
        ensure!(
            cfg!(unix),
            "the MCP pilot binding requires Unix file identities"
        );
        // Resolve ancestors once, here, rather than trusting the caller to pass a canonical path.
        // SQLITE_OPEN_NOFOLLOW rejects a symbolic link anywhere in the path, so an uncanonicalized
        // state directory would fail the open even when the database itself is a regular file.
        // Ancestors are resolved at enrollment; the final component must still not be a link.
        let state_dir = &state_dir
            .canonicalize()
            .context("resolve the canonical state directory")?;
        let cache_path = state_dir.join("cache.db");
        let workspace_path = state_dir.join("workspace.db");

        let dir = open_no_follow(state_dir, true)?;
        let cache = open_no_follow(&cache_path, false)?;
        let workspace = open_no_follow(&workspace_path, false)?;
        let (dir_meta, cache_meta, workspace_meta) =
            (dir.metadata()?, cache.metadata()?, workspace.metadata()?);
        ensure!(dir_meta.is_dir(), "state directory must be a directory");
        ensure!(cache_meta.is_file(), "cache must be a regular file");
        ensure!(workspace_meta.is_file(), "workspace must be a regular file");

        // Read-only: this must never create or migrate a main database. A missing cache is a
        // storage failure, not an empty store.
        let conn = Connection::open_with_flags(&cache_path, READ_ONLY_FLAGS)
            .context("open the bound cache read-only")?;
        // Force the VFS to open the main file so the identity check below is meaningful, and
        // surface a refused WAL setup here rather than on the first tool call.
        conn.query_row("SELECT count(*) FROM sqlite_schema", [], |r| {
            r.get::<_, i64>(0)
        })
        .context("establish read-only access to the bound cache")?;

        let enrollment = Self {
            daemon_instance_id: uuid::Uuid::new_v4().to_string(),
            generation: Mutex::new(uuid::Uuid::new_v4().to_string()),
            state_dir: state_dir.to_path_buf(),
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
            latched: AtomicBool::new(false),
            admission: publication,
        };
        // Check identities after connection setup as well as before, so a swap racing the open is
        // caught rather than adopted.
        enrollment
            .check_identity()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(enrollment)
    }

    pub fn daemon_instance_id(&self) -> String {
        self.daemon_instance_id.clone()
    }

    pub fn store_generation(&self) -> String {
        self.generation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn is_available(&self) -> bool {
        !self.latched.load(Ordering::SeqCst)
    }

    /// Latch this binding off for the rest of the daemon's lifetime and rotate the generation once.
    ///
    /// There is no in-process recovery: a later republication, a restored file or a browser read
    /// that recreates the cache must not clear the latch.
    pub fn invalidate(&self) {
        self.latch_locked();
    }

    /// Latch immediately.
    ///
    /// Deliberately takes no boundary. Invalidation is a fail-closed signal that every other
    /// request must observe at once, and an in-flight admitted response would otherwise block the
    /// observer from recording it. Suppression of an already-admitted response is handled where it
    /// belongs, by re-reading this flag immediately before emitting.
    fn latch(&self) {
        self.latch_locked();
    }

    fn latch_locked(&self) {
        // Taken so that latching and a ticket's commit cannot interleave. Held only for the flag
        // itself: this never waits for in-flight responses, so an observer is never blocked.
        let _state = self
            .admission
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !self.latched.swap(true, Ordering::SeqCst) {
            let mut generation = self.generation.lock().unwrap_or_else(|e| e.into_inner());
            *generation = uuid::Uuid::new_v4().to_string();
        }
    }

    fn path_identity(&self) -> Result<(), McpError> {
        let unavailable = || McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE);
        for (path, expected) in [
            (&self.state_dir, self.dir_id),
            (&self.cache_path, self.cache_id),
            (&self.workspace_path, self.workspace_id),
        ] {
            // lstat: a path replaced by a symlink must not resolve to its target.
            let meta = std::fs::symlink_metadata(path).map_err(|_| unavailable())?;
            if meta.file_type().is_symlink() || file_id(&meta) != expected {
                return Err(unavailable());
            }
        }
        Ok(())
    }

    /// Path anchors plus the opened connection's moved status, and the revision pinned in the same
    /// lock hold. Pure: the caller decides whether to latch.
    fn identity_and_revision(&self) -> Result<u64, McpError> {
        self.path_identity()?;
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if has_moved(&conn)? {
            return Err(McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE));
        }
        current_revision(&conn).map_err(|_| McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE))
    }

    /// Full identity check. Any failure latches the binding before returning.
    fn check_identity(&self) -> Result<(), McpError> {
        if !self.is_available() {
            return Err(McpError::new(ErrorCode::StoreUnavailable, LATCHED));
        }
        let verdict = self.identity_and_revision().map(|_| ());
        if verdict.is_err() {
            // Not `invalidate`: this runs on the read path, which must not take the admission lock.
            self.latch();
        }
        verdict
    }

    /// Final admission.
    ///
    /// Re-verifies availability, identity and the current revision, then produces the caller's
    /// value without releasing the boundary. Publication and invalidation take the same boundary,
    /// so a response or credential can never be produced from a snapshot that is being replaced or
    /// that another request has observed to be gone.
    ///
    /// The revision is re-read from the database rather than compared against an in-process
    /// counter, so a publication by a second `Store` or a separate process is detected too. Bytes
    /// already sent cannot be recalled; this bounds what is allowed to start being sent.
    pub fn admit<T>(
        &self,
        basis_revision: u64,
        deadline: Instant,
        produce: impl FnOnce() -> T,
    ) -> Result<(T, Ticket), McpError> {
        let (value, ticket) = self.admit_current(deadline, |current| {
            if current != basis_revision {
                return Err(McpError::new(ErrorCode::RevisionConflict, CONFLICT));
            }
            Ok(produce())
        })?;
        Ok((value?, ticket))
    }

    /// Admission for a response that carries no evidence.
    ///
    /// Re-verifies availability and identity under the boundary and hands the closure the revision
    /// observed in that same hold. A response that describes current state has nothing to conflict
    /// with, so a publication racing it is reported rather than refused.
    pub fn admit_current<T>(
        &self,
        deadline: Instant,
        produce: impl FnOnce(u64) -> T,
    ) -> Result<(T, Ticket), McpError> {
        self.admit_with(deadline, |current| (produce(current), ()), |(), _| {})
    }

    /// Admission for a producer with an effect that must be undone if the snapshot moves.
    ///
    /// `produce` returns its value together with a handle; if verification after production fails,
    /// `undo` is called with that handle so nothing it created survives. The alternative is
    /// discarding the value on the error path, which leaves the effect behind.
    pub fn admit_with<T, H>(
        &self,
        deadline: Instant,
        produce: impl FnOnce(u64) -> (T, H),
        undo: impl FnOnce(H, &McpError),
    ) -> Result<(T, Ticket), McpError> {
        let ticket = self.admission.enter(deadline)?;
        let current = self.verify()?;
        // Checked before any effect: verification performs filesystem and database work, so an
        // admission can cross its deadline before the producer ever runs.
        if Instant::now() >= deadline {
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        }
        let (produced, handle) = produce(current);
        // Verified again after the producer, still inside the same hold. The boundary serializes
        // same-instance publication, but a second Store or another process has its own mutex, so
        // only re-reading the database proves the snapshot did not move while the value was being
        // built. A value built over a replaced snapshot is discarded rather than returned.
        let outcome = match self.verify() {
            Ok(after) if after != current => {
                Err(McpError::new(ErrorCode::RevisionConflict, CONFLICT))
            }
            Ok(_) if Instant::now() >= deadline => {
                Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT))
            }
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        };
        match outcome {
            Ok(()) => Ok((produced, ticket)),
            Err(e) => {
                undo(handle, &e);
                Err(e)
            }
        }
    }

    /// Availability, path and connection identity, and the current revision, latching on failure.
    /// Caller holds the boundary.
    fn verify(&self) -> Result<u64, McpError> {
        if !self.is_available() {
            return Err(McpError::new(ErrorCode::StoreUnavailable, LATCHED));
        }
        self.identity_and_revision()
            .inspect_err(|_| self.latch_locked())
    }

    /// Run one guarded read.
    ///
    /// Revision and evidence are read in a single transaction on the enrolled connection, with the
    /// identity checked before and after. `expected` is compared against the transaction-pinned
    /// revision; callers that admit a grant compare it against the admitted revision separately.
    pub fn read<T>(
        &self,
        expected: Option<u64>,
        deadline: Instant,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<(u64, T), McpError> {
        // Ownership first. `check_identity` locks the connection, so checking before the gate would
        // park a queued reader on that mutex where its own deadline cannot reach it.
        if !self.is_available() {
            return Err(McpError::new(ErrorCode::StoreUnavailable, LATCHED));
        }
        let Some(_gate) = self.gate.acquire(deadline) else {
            // Refused while queued: this request never reached the connection, so nothing was
            // interrupted and the admitted request count is still spent.
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        };
        self.check_identity()?;

        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let watchdog = Watchdog::arm(self.interrupt.clone(), deadline);
        let outcome: rusqlite::Result<Result<(u64, T), McpError>> = (|| {
            conn.execute_batch("BEGIN DEFERRED")?;
            let revision = current_revision(&conn)?;
            if expected.is_some_and(|r| r != revision) {
                return Ok(Err(McpError::new(ErrorCode::RevisionConflict, CONFLICT)));
            }
            let value = f(&conn)?;
            Ok(Ok((revision, value)))
        })();
        // The transaction is read-only, so rollback is the only correct exit. An interrupted read
        // leaves it open but the connection usable, so this also restores autocommit.
        let _ = conn.execute_batch("ROLLBACK");
        // Disarm and join before ownership is released, so the watchdog can never interrupt the
        // next request. A callback that does no SQL outlives an interrupt, so completion is judged
        // by the clock as well as by whether the watchdog fired.
        let timed_out = watchdog.fired() || Instant::now() >= deadline;
        drop(watchdog);
        drop(conn);

        // Checked on every exit, not only on success: a rename or replacement observed during a
        // read that timed out or errored must still latch the binding, or the next request would
        // find it available.
        let identity = self.check_identity();
        if timed_out {
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        }
        identity?;
        let value = match outcome {
            Ok(Ok(value)) => value,
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(McpError::new(ErrorCode::StoreUnavailable, UNAVAILABLE)),
        };
        // Last word on the clock. The identity check above performs filesystem and database work,
        // so time can elapse after the earlier sample; a read must never succeed past its deadline.
        if Instant::now() >= deadline {
            return Err(McpError::new(ErrorCode::DeadlineExceeded, TIMED_OUT));
        }
        Ok(value)
    }

    /// Current published revision. Zero means no index has been published in this generation.
    pub fn current_revision(&self, deadline: Instant) -> Result<u64, McpError> {
        self.read(None, deadline, |_| Ok(())).map(|(r, ())| r)
    }

    pub fn basis(&self, index_revision: u64) -> EvidenceBasis {
        EvidenceBasis {
            daemon_instance_id: self.daemon_instance_id(),
            store_generation: self.store_generation(),
            index_revision,
        }
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
