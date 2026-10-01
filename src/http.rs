//! Loopback-only HTTP API. Database and indexing work runs on blocking workers.
use crate::{
    acp::Acp,
    auth::valid_token,
    dependencies::{Catalog, CatalogOptions},
    indexer::IndexOptions,
    jev,
    live_jev::LiveJev,
    model::*,
    planning::{
        self, FocusedView, QuestionPacket, QuestionPreview, QuestionRequest, SelectionEnvelope,
    },
    store::{EvidenceFence, EvidenceFencePolicy, Store},
};
use anyhow::Context;
use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::{
    collections::{BTreeMap, VecDeque},
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexJob {
    pub id: String,
    pub state: String,
    pub progress: IndexProgress,
    pub revision: Option<IndexPin>,
    pub error: Option<Value>,
    pub submitted_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}
struct Jobs {
    current: Option<String>,
    jobs: BTreeMap<String, IndexJob>,
    cancel: CancelFlag,
}
const MAX_PACKETS: usize = 8;
const MAX_PACKET_BYTES: usize = 1024 * 1024;
const MAX_CACHE_BYTES: usize = 8 * MAX_PACKET_BYTES;
#[derive(Default, Clone)]
struct PacketCache {
    packets: VecDeque<(Arc<QuestionPacket>, usize)>,
    bytes: usize,
}
impl PacketCache {
    fn remember(&mut self, packet: Arc<QuestionPacket>, bytes: usize) {
        if let Some(index) = self
            .packets
            .iter()
            .position(|(p, _)| p.packet_id == packet.packet_id)
        {
            self.bytes -= self.packets.remove(index).unwrap().1;
        }
        while self.packets.len() >= MAX_PACKETS || self.bytes + bytes > MAX_CACHE_BYTES {
            self.bytes -= self.packets.pop_front().unwrap().1;
        }
        self.bytes += bytes;
        self.packets.push_back((packet, bytes));
    }
    // The final fence runs after admission while this mutex is held. A failed
    // response cannot evict an earlier packet or replace its same-ID entry.
    fn remember_fenced(
        &mut self,
        packet: Arc<QuestionPacket>,
        bytes: usize,
        finish: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let prior = self.clone();
        self.remember(packet, bytes);
        if let Err(error) = finish() {
            *self = prior;
            return Err(error);
        }
        Ok(())
    }
}
struct DependencyIndex {
    generation: u64,
    cancel: Arc<AtomicBool>,
    stopped: bool,
    worker_running: bool,
    state: &'static str,
    catalog: Option<Arc<Catalog>>,
    warnings: Vec<String>,
}
impl DependencyIndex {
    /// Replace the pending generation, but admit at most one worker driver.
    fn request_build(&mut self) -> bool {
        if self.stopped {
            return false;
        }
        self.cancel.store(true, Ordering::Release);
        self.generation += 1;
        self.cancel = Arc::new(AtomicBool::new(false));
        self.state = "loading";
        self.catalog = None;
        self.warnings.clear();
        !std::mem::replace(&mut self.worker_running, true)
    }
}
pub struct DaemonState {
    store: Store,
    serving_session: Mutex<Option<Arc<crate::store::topology::LeaderSession>>>,
    options: IndexOptions,
    browser: crate::file_tree::SourceDir,
    rust_sources: Vec<crate::rust_sources::Root>,
    dependency_options: Option<CatalogOptions>,
    dependencies: Mutex<DependencyIndex>,
    #[cfg(test)]
    dependency_capture_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    outer_fence_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    preview_finish_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    token: String,
    hosts: Vec<String>,
    origins: Vec<String>,
    jobs: Mutex<Jobs>,
    queue_tick_started: AtomicBool,
    #[cfg(test)]
    queue_takeover_attempts: AtomicUsize,
    #[cfg(test)]
    test_pending_read_failures: AtomicUsize,
    pending_requests: Mutex<Vec<String>>,
    job_progress: Mutex<BTreeMap<String, IndexProgress>>,
    native_stream: Mutex<()>,
    packets: Mutex<PacketCache>,
    provider: Option<Arc<LiveJev>>,
    acp: Option<Arc<Acp>>,
}
pub fn new(
    store: Store,
    index_options: IndexOptions,
    token: String,
    address: SocketAddr,
) -> anyhow::Result<Arc<DaemonState>> {
    new_with_jev(store, index_options, token, address, None)
}
pub fn new_with_jev(
    store: Store,
    index_options: IndexOptions,
    token: String,
    address: SocketAddr,
    provider: Option<Arc<LiveJev>>,
) -> anyhow::Result<Arc<DaemonState>> {
    new_with_providers(store, index_options, token, address, provider, None)
}
pub fn new_with_providers(
    store: Store,
    index_options: IndexOptions,
    token: String,
    address: SocketAddr,
    jev: Option<Arc<LiveJev>>,
    acp: Option<Arc<Acp>>,
) -> anyhow::Result<Arc<DaemonState>> {
    let browse_root = index_options.workspace_root.clone();
    new_with_browser_root(store, index_options, token, address, jev, acp, browse_root)
}
pub fn new_with_browser_root(
    store: Store,
    index_options: IndexOptions,
    token: String,
    address: SocketAddr,
    jev: Option<Arc<LiveJev>>,
    acp: Option<Arc<Acp>>,
    browse_root: std::path::PathBuf,
) -> anyhow::Result<Arc<DaemonState>> {
    new_with_source_roots(
        store,
        index_options,
        token,
        address,
        jev,
        acp,
        browse_root,
        vec![],
    )
}
#[allow(clippy::too_many_arguments)]
pub fn new_with_source_roots(
    store: Store,
    index_options: IndexOptions,
    token: String,
    address: SocketAddr,
    jev: Option<Arc<LiveJev>>,
    acp: Option<Arc<Acp>>,
    browse_root: std::path::PathBuf,
    source_roots: Vec<(String, std::path::PathBuf)>,
) -> anyhow::Result<Arc<DaemonState>> {
    new_with_dependency_options(
        store,
        index_options,
        token,
        address,
        jev,
        acp,
        browse_root,
        source_roots,
        None,
    )
}
/// Catalog roots are trusted daemon configuration, never request parameters.
#[allow(clippy::too_many_arguments)]
pub fn new_with_dependency_options(
    store: Store,
    index_options: IndexOptions,
    token: String,
    address: SocketAddr,
    jev: Option<Arc<LiveJev>>,
    acp: Option<Arc<Acp>>,
    browse_root: std::path::PathBuf,
    source_roots: Vec<(String, std::path::PathBuf)>,
    catalog_options: Option<CatalogOptions>,
) -> anyhow::Result<Arc<DaemonState>> {
    anyhow::ensure!(
        address.ip().is_loopback() && address.port() != 0,
        "daemon requires a bound loopback address"
    );
    anyhow::ensure!(valid_token(&token), "invalid bearer token");
    let hosts = vec![address.to_string(), format!("localhost:{}", address.port())];
    let origins = hosts.iter().map(|h| format!("http://{h}")).collect();
    Ok(Arc::new(DaemonState {
        store,
        serving_session: Mutex::new(None),
        options: index_options,
        browser: crate::file_tree::SourceDir::open(&browse_root)?,
        rust_sources: crate::rust_sources::open_roots(source_roots)?,
        dependencies: Mutex::new(DependencyIndex {
            generation: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            stopped: false,
            worker_running: false,
            state: if catalog_options.is_some() {
                "loading"
            } else {
                "disabled"
            },
            catalog: None,
            warnings: Vec::new(),
        }),
        #[cfg(test)]
        dependency_capture_hook: Mutex::new(None),
        #[cfg(test)]
        outer_fence_hook: Mutex::new(None),
        #[cfg(test)]
        preview_finish_hook: Mutex::new(None),
        dependency_options: catalog_options,
        token,
        hosts,
        origins,
        provider: jev,
        acp,
        packets: Mutex::new(PacketCache::default()),
        queue_tick_started: AtomicBool::new(false),
        #[cfg(test)]
        queue_takeover_attempts: AtomicUsize::new(0),
        #[cfg(test)]
        test_pending_read_failures: AtomicUsize::new(0),
        pending_requests: Mutex::new(Vec::new()),
        job_progress: Mutex::new(BTreeMap::new()),
        native_stream: Mutex::new(()),
        jobs: Mutex::new(Jobs {
            current: None,
            jobs: BTreeMap::new(),
            cancel: Arc::new(AtomicBool::new(false)),
        }),
    }))
}
impl DaemonState {
    pub fn retain_serving_session(
        self: &Arc<Self>,
        session: Arc<crate::store::topology::LeaderSession>,
    ) {
        *self.serving_session.lock().unwrap() = Some(session);
        self.start_queue_tick();
    }
    /// A leader checks only the queue at idle. Follower retries require an accepted local ID.
    fn start_queue_tick(self: &Arc<Self>) {
        // A synchronous fixture may retain an owner without starting a daemon runtime.
        // Do not consume the start flag until a Tokio executor can own the tick.
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        if self.queue_tick_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(20));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let Some(state) = weak.upgrade() else {
                    break;
                };
                let worker = state.clone();
                let result = tokio::task::spawn_blocking(move || worker.queue_tick()).await;
                if let Err(error) = result {
                    eprintln!("queue tick failed: {error}");
                }
            }
        });
    }
    fn queue_tick(self: &Arc<Self>) -> anyhow::Result<()> {
        let _stream = self.native_stream.lock().unwrap();
        let mut pending = self.pending_requests.lock().unwrap();
        pending.retain(|id| {
            #[cfg(test)]
            if self
                .test_pending_read_failures
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                    (count > 0).then(|| count - 1)
                })
                .is_ok()
            {
                // Model a transient requests.db read failure without changing a live
                // queue file or the protected index controls.
                return true;
            }
            match self.store.request_by_id(id) {
                Ok(Some(row)) => row.finished_at.is_none(),
                // A failed or inconclusive read cannot erase the only request-driven
                // follower retry trigger. Only an observed terminal removes this ID.
                Ok(None) | Err(_) => true,
            }
        });
        let pending_local = !pending.is_empty();
        drop(pending);
        let retained = self.serving_session.lock().unwrap().clone();
        if let Some(ref session) = retained
            && session.is_leader()
            && session.verify().is_ok()
        {
            if crate::index_coordinator::drain_requests_observed(&self.store, session, |id, p| {
                self.job_progress.lock().unwrap().insert(id.to_owned(), p);
            })? > 0
            {
                *self.packets.lock().unwrap() = PacketCache::default();
                self.start_dependency_index();
            }
            return Ok(());
        }
        if !pending_local {
            return Ok(());
        }
        // A retained follower verifies both the held leader flock and incarnation
        // without opening index.db. Retry acquisition only after that proof fails.
        if let Some(ref session) = retained
            && !session.is_leader()
            && session.verify().is_ok()
        {
            return Ok(());
        }
        #[cfg(test)]
        self.queue_takeover_attempts.fetch_add(1, Ordering::AcqRel);
        match self.store.leader_session() {
            Ok(session) => {
                let outcome = (|| {
                    let coordinator =
                        crate::index_coordinator::IndexJobCoordinator::prepare_with_session(
                            &self.store,
                            None,
                            session.clone(),
                        )?;
                    coordinator.run(&self.options, &Arc::new(AtomicBool::new(false)), |_| {})?;
                    let processed = crate::index_coordinator::drain_requests_observed(
                        &self.store,
                        &session,
                        |id, p| {
                            self.job_progress.lock().unwrap().insert(id.to_owned(), p);
                        },
                    )?;
                    if processed > 0 {
                        *self.packets.lock().unwrap() = PacketCache::default();
                        self.start_dependency_index();
                    }
                    Ok(processed)
                })();
                if outcome.is_ok() {
                    *self.serving_session.lock().unwrap() = Some(session);
                }
                outcome.map(|_| ())
            }
            Err(error) if format!("{error:#}").contains("storage_busy") => Ok(()),
            Err(error) => Err(error),
        }
    }
    pub fn retained_serving_session(
        &self,
    ) -> anyhow::Result<Arc<crate::store::topology::LeaderSession>> {
        self.serving_session
            .lock()
            .unwrap()
            .clone()
            .context("index_not_ready: no verified daemon serving session")
    }

    /// Start a replacement generation without doing filesystem or database work on the caller.
    /// A previous generation can finish, but can never publish over its replacement.
    pub fn start_dependency_index(self: &Arc<Self>) {
        let Some(options) = self.dependency_options.clone() else {
            return;
        };
        if !self.dependencies.lock().unwrap().request_build() {
            return;
        }
        let state = self.clone();
        tokio::spawn(async move {
            loop {
                let (generation, cancel) = {
                    let mut index = state.dependencies.lock().unwrap();
                    if index.stopped {
                        index.worker_running = false;
                        return;
                    }
                    (index.generation, index.cancel.clone())
                };
                let worker = state.clone();
                let worker_cancel = cancel.clone();
                let options = options.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let result = (|| {
                        let revision = worker.store.status()?.revision;
                        Catalog::build(
                            &worker.options.workspace_root,
                            revision,
                            &options,
                            &worker_cancel,
                        )
                    })();
                    worker.publish_dependency_index(generation, &worker_cancel, result);
                })
                .await;
                if result.is_err() {
                    // Error publication does not access the database.
                    state.publish_dependency_index(
                        generation,
                        &cancel,
                        Err(anyhow::anyhow!("worker failed")),
                    );
                }
                let mut index = state.dependencies.lock().unwrap();
                if index.stopped || index.generation == generation {
                    index.worker_running = false;
                    return;
                }
                // Refresh bursts coalesce to the latest generation. Only this driver
                // can launch catalog work, and its previous blocking worker has exited.
            }
        });
    }
    fn publish_dependency_index(
        &self,
        generation: u64,
        cancel: &AtomicBool,
        result: anyhow::Result<Catalog>,
    ) {
        // Called on a blocking worker for successful builds. No parsing, source I/O,
        // or database work while holding the catalog lock.
        let result = result.and_then(|catalog| {
            anyhow::ensure!(
                self.store.status()?.revision == catalog.workspace_revision,
                "revision conflict"
            );
            Ok(catalog)
        });
        let mut index = self.dependencies.lock().unwrap();
        if index.stopped || index.generation != generation || cancel.load(Ordering::Acquire) {
            return;
        }
        match result {
            Ok(catalog) => {
                index.state = "ready";
                index.catalog = Some(Arc::new(catalog));
            }
            Err(_) => {
                index.state = "failed";
                index.catalog = None;
                index.warnings = vec![
                    "Dependency catalog build failed or workspace changed; refresh to retry".into(),
                ];
            }
        }
    }
    /// Presentation-only snapshot. The caller must use a matching cached workspace revision.
    pub(crate) fn catalog_snapshot(&self, revision: IndexPin) -> Option<Arc<Catalog>> {
        self.dependencies
            .lock()
            .unwrap()
            .catalog
            .as_ref()
            .filter(|catalog| catalog.workspace_revision == revision)
            .cloned()
    }
    pub fn cancel_active(&self) {
        {
            let mut dependencies = self.dependencies.lock().unwrap();
            dependencies.stopped = true;
            dependencies.cancel.store(true, Ordering::Release);
            dependencies.generation += 1;
            dependencies.catalog = None;
            if self.dependency_options.is_some() {
                dependencies.state = "failed";
                dependencies.warnings =
                    vec!["Dependency indexing cancelled during shutdown".into()];
            }
        }
        if let Some(acp) = &self.acp {
            acp.cancel_all();
        }
        self.jobs
            .lock()
            .unwrap()
            .cancel
            .store(true, Ordering::Release);
    }
}
fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"error":{"code":code,"message":message}})),
    )
        .into_response()
}
#[derive(Debug)]
struct ApiError(StatusCode, &'static str, &'static str);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error(self.0, self.1, self.2)
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        if e.chain().any(|cause| cause.downcast_ref::<rusqlite::Error>().is_some_and(|error| {
            matches!(error, rusqlite::Error::SqliteFailure(info, _) if matches!(info.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
        })) {
            return Self(StatusCode::CONFLICT, "storage_busy", "Storage is busy");
        }
        if let Some(invalid) = e.downcast_ref::<crate::navigation::InvalidRequest>() {
            Self(
                StatusCode::BAD_REQUEST,
                "invalid_navigation_request",
                invalid.0,
            )
        } else if let Some(invalid) = e.downcast_ref::<crate::class_diagram::InvalidRequest>() {
            Self(StatusCode::BAD_REQUEST, "invalid_class_request", invalid.0)
        } else if e.to_string().starts_with("revision conflict") {
            Self(
                StatusCode::CONFLICT,
                "revision_conflict",
                "The index revision changed",
            )
        } else if matches!(
            e.to_string().as_str(),
            "saved view target replacement is not allowed"
                | "saved annotation target replacement is not allowed"
                | "native declaration target missing"
        ) {
            invalid()
        } else {
            let text = e.to_string();
            for (prefix, status, code) in [
                ("root_changed", StatusCode::CONFLICT, "root_changed"),
                (
                    "root_key_collision",
                    StatusCode::SERVICE_UNAVAILABLE,
                    "root_key_collision",
                ),
                (
                    "workspace_id_changed",
                    StatusCode::CONFLICT,
                    "workspace_id_changed",
                ),
                ("storage_busy", StatusCode::CONFLICT, "storage_busy"),
                (
                    "index_not_ready",
                    StatusCode::SERVICE_UNAVAILABLE,
                    "index_not_ready",
                ),
                (
                    "incompatible_index",
                    StatusCode::SERVICE_UNAVAILABLE,
                    "incompatible_index",
                ),
                (
                    "incompatible_record",
                    StatusCode::SERVICE_UNAVAILABLE,
                    "incompatible_record",
                ),
                (
                    "incomplete_record",
                    StatusCode::SERVICE_UNAVAILABLE,
                    "incomplete_record",
                ),
                (
                    "recovery_required",
                    StatusCode::SERVICE_UNAVAILABLE,
                    "recovery_required",
                ),
                (
                    "unsafe_index",
                    StatusCode::SERVICE_UNAVAILABLE,
                    "unsafe_index",
                ),
            ] {
                if text.starts_with(prefix) {
                    return Self(status, code, "Storage is unavailable");
                }
            }
            Self(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Operation failed",
            )
        }
    }
}
fn invalid() -> ApiError {
    ApiError(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "Invalid request",
    )
}
fn missing() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not_found", "Not found")
}
async fn db<T: Send + 'static>(
    s: Arc<DaemonState>,
    f: impl FnOnce(&Store) -> anyhow::Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(move || f(&s.store))
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Operation failed",
            )
        })?
        .map_err(Into::into)
}
async fn guard(State(s): State<Arc<DaemonState>>, mut req: Request, next: Next) -> Response {
    let headers = req.headers();
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    let origin = headers.get(header::ORIGIN);
    let mut response = if headers.get_all(header::HOST).iter().count() != 1
        || !host.is_some_and(|h| s.hosts.iter().any(|v| v == h))
    {
        error(StatusCode::FORBIDDEN, "invalid_host", "Host is not allowed")
    } else if headers.get_all(header::ORIGIN).iter().count() > 1
        || origin.is_some_and(|h| {
            !h.to_str()
                .ok()
                .is_some_and(|v| s.origins.iter().any(|o| o == v))
        })
    {
        error(
            StatusCode::FORBIDDEN,
            "invalid_origin",
            "Origin is not allowed",
        )
    } else if (req.uri().path() == "/api" || req.uri().path().starts_with("/api/"))
        && (headers.get_all(header::AUTHORIZATION).iter().count() != 1
            || !headers
                .get(header::AUTHORIZATION)
                .and_then(|h| h.to_str().ok())
                .and_then(|h| h.strip_prefix("Bearer "))
                .is_some_and(|t| bool::from(t.as_bytes().ct_eq(s.token.as_bytes()))))
    {
        error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Bearer authentication required",
        )
    } else {
        let body = std::mem::replace(req.body_mut(), Body::empty());
        match to_bytes(body, 1024 * 1024).await {
            Ok(bytes) => {
                *req.body_mut() = Body::from(bytes);
                next.run(req).await
            }
            Err(_) => error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body_too_large",
                "Request body exceeds limit",
            ),
        }
    };
    if response.status().is_client_error()
        && !response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|v| v == "application/json")
    {
        response = error(response.status(), "invalid_request", "Invalid request");
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY,"default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'".parse().unwrap());
    response
        .headers_mut()
        .insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    response
}
pub fn router(state: Arc<DaemonState>) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    include_str!("../web/index.html"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/shell.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/shell.js"),
                )
            }),
        )
        .route(
            "/fonts/jetbrains-mono-latin.woff2",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "font/woff2")],
                    &include_bytes!("../web/fonts/jetbrains-mono-latin.woff2")[..],
                )
            }),
        )
        .route(
            "/fonts/space-grotesk-latin.woff2",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "font/woff2")],
                    &include_bytes!("../web/fonts/space-grotesk-latin.woff2")[..],
                )
            }),
        )
        .route(
            "/fonts/JetBrainsMono-OFL.txt",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../web/fonts/JetBrainsMono-OFL.txt"),
                )
            }),
        )
        .route(
            "/fonts/SpaceGrotesk-OFL.txt",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../web/fonts/SpaceGrotesk-OFL.txt"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../web/style.css"),
                )
            }),
        )
        .route(
            "/healthz",
            get(|| async { Json(json!({"ok":true,"version":env!("CARGO_PKG_VERSION")})) }),
        )
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
        .route("/api/status", get(status))
        .route("/api/jev/status", get(jev_status))
        .route("/api/acp/status", get(acp_status))
        .route(
            "/api/questions/{packet_id}/acp-answer",
            post(question_answer),
        )
        .route("/api/questions/{packet_id}/jev-run", post(question_run))
        .route("/api/index", post(start_index))
        .route("/api/jobs/current", get(current_job))
        .route("/api/jobs/{id}", get(job))
        .route("/api/jobs/{id}/cancel", post(cancel_job))
        .route("/api/dependencies", get(dependency_status))
        .route("/api/dependencies/refresh", post(dependency_refresh))
        .route("/api/dependencies/symbols", get(dependency_symbols))
        .route("/api/dependencies/source", get(dependency_source))
        .route("/api/rust-sources", get(rust_source_roots))
        .route("/api/rust-sources/tree", get(rust_source_tree))
        .route("/api/rust-sources/file", get(rust_source_file))
        .route("/api/tree", get(tree))
        .route("/api/files", get(files))
        .route("/api/methods", get(methods))
        .route("/api/sequence", post(sequence))
        .route("/api/classes", get(classes))
        .route("/api/class-diagram", post(class_diagram))
        .route("/api/navigation", post(navigation))
        .route(
            "/classes.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/classes.js"),
                )
            }),
        )
        .route(
            "/classes.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../web/classes.css"),
                )
            }),
        )
        .route(
            "/navigation.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/navigation.js"),
                )
            }),
        )
        .route(
            "/navigation.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../web/navigation.css"),
                )
            }),
        )
        .route(
            "/sequence.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../web/sequence.js"),
                )
            }),
        )
        .route("/api/symbols", get(symbols))
        .route("/api/symbol", get(symbol))
        .route("/api/source", get(source))
        .route("/api/query", post(query))
        .route("/api/questions/preview", post(question_preview))
        .route(
            "/api/questions/{packet_id}/jev-request",
            get(question_export),
        )
        .route(
            "/api/questions/{packet_id}/jev-response",
            post(question_import),
        )
        .route(
            "/api/questions/{packet_id}/selection",
            post(question_selection),
        )
        .route("/api/views", get(views))
        .route(
            "/api/views/{id}",
            get(view).put(save_view).delete(delete_view),
        )
        .route("/api/annotations", get(annotations))
        .route(
            "/api/annotations/{id}",
            put(save_annotation).delete(delete_annotation),
        )
        .fallback(|| async { missing().into_response() })
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            evidence_response_guard,
        ))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}
// Only endpoints that include a native snapshot need this outer response fence.
// Durable records and external-source-only routes keep their independent policy.
fn native_response_route(path: &str) -> bool {
    matches!(
        path,
        "/api/status"
            | "/api/tree"
            | "/api/files"
            | "/api/methods"
            | "/api/sequence"
            | "/api/classes"
            | "/api/class-diagram"
            | "/api/navigation"
            | "/api/symbols"
            | "/api/symbol"
            | "/api/source"
            | "/api/query"
            | "/api/dependencies"
            | "/api/dependencies/symbols"
            | "/api/dependencies/source"
            | "/api/rust-sources/tree"
    )
}
async fn evidence_response_guard(
    State(state): State<Arc<DaemonState>>,
    request: Request,
    next: Next,
) -> Response {
    if !native_response_route(request.uri().path()) {
        return next.run(request).await;
    }
    // The snapshot is short: retain its protected leader observation through
    // serialization. An ordinary same-incarnation publish does not reject an
    // already-materialized coherent unpinned response.
    let admitted = db(state.clone(), |store| {
        let snapshot = store.evidence_response()?;
        Ok(snapshot.into_fence(EvidenceFencePolicy::T03))
    })
    .await;
    let response = next.run(request).await;
    if !response.status().is_success() {
        // Preserve validation/materialization error precedence, but still check T03.
        if let Ok(fence) = admitted {
            let _ = fence.finish(());
        }
        return response;
    }
    let fence = match admitted {
        Ok(fence) => fence,
        Err(error) => return error.into_response(),
    };
    #[cfg(test)]
    if let Some(hook) = state.outer_fence_hook.lock().unwrap().as_ref() {
        hook();
    }
    match fence.finish(response) {
        Ok(response) => response,
        Err(error) => ApiError::from(error).into_response(),
    }
}
fn dependency_stale() -> ApiError {
    ApiError(
        StatusCode::CONFLICT,
        "stale_catalog",
        "Dependency catalog or source changed; refresh the catalog",
    )
}
async fn dependency_work<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f).await.map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "dependency_error",
            "Dependency worker failed",
        )
    })?
}
impl DaemonState {
    fn requested_catalog(&self, id: &str) -> Result<Arc<Catalog>, ApiError> {
        let revision = self.store.status()?.revision;
        self.catalog_snapshot(revision)
            .filter(|catalog| catalog.id == id)
            .ok_or_else(dependency_stale)
    }
    fn check_catalog(&self, catalog: &Arc<Catalog>) -> Result<(), ApiError> {
        let current = self.requested_catalog(&catalog.id)?;
        if !Arc::ptr_eq(&current, catalog) {
            return Err(dependency_stale());
        }
        Ok(())
    }
}
async fn dependency_status(State(s): State<Arc<DaemonState>>) -> Result<Response, ApiError> {
    dependency_work(move || dependency_status_response(&s, || {}).map_err(ApiError::from)).await
}
fn dependency_status_response(
    s: &DaemonState,
    after_capture: impl FnOnce(),
) -> anyhow::Result<Response> {
    let response = s.store.evidence_response()?;
    let revision = response.status()?.revision;
    after_capture();
    #[cfg(test)]
    if let Some(hook) = s.dependency_capture_hook.lock().unwrap().as_ref() {
        hook();
    }
    let (state, catalog, mut warnings) = {
        let index = s.dependencies.lock().unwrap();
        (index.state, index.catalog.clone(), index.warnings.clone())
    };
    let payload = if let Some(catalog) = catalog {
        if catalog.workspace_revision == revision {
            json!({"state":state,"workspaceRevision":revision,
                "catalogId":catalog.id,"packages":catalog.packages,
                "symbolCount":catalog.symbols.len(),"warnings":catalog.warnings})
        } else {
            warnings.push("Workspace changed; refresh the dependency catalog".into());
            json!({"state":"failed","workspaceRevision":revision,
                "catalogId":null,"packages":[],"symbolCount":0,"warnings":warnings})
        }
    } else {
        json!({"state":state,"workspaceRevision":revision,
            "catalogId":null,"packages":[],"symbolCount":0,"warnings":warnings})
    };
    // IntoResponse serializes the owned JSON before the final T03 check. No
    // evidence body can escape on a failed fence, including the failed-catalog branch.
    let result = Json(payload).into_response();
    response.finish(result)
}
async fn dependency_refresh(
    State(s): State<Arc<DaemonState>>,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let body: Value = serde_json::from_slice(&body).map_err(|_| invalid())?;
    if !body.as_object().is_some_and(|object| object.is_empty()) {
        return Err(invalid());
    }
    if s.dependency_options.is_none() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "dependencies_disabled",
            "Dependency catalog is disabled",
        ));
    }
    if s.dependencies.lock().unwrap().stopped {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "shutting_down",
            "Daemon is shutting down",
        ));
    }
    s.start_dependency_index();
    Ok((StatusCode::ACCEPTED, Json(json!({"state":"loading"}))))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DependencySymbolsQuery {
    catalog_id: String,
    package_id: Option<String>,
    #[serde(default)]
    q: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "dependency_limit")]
    limit: usize,
}
fn dependency_limit() -> usize {
    100
}
async fn dependency_symbols(
    State(s): State<Arc<DaemonState>>,
    query: Result<Query<DependencySymbolsQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(q) = query.map_err(|_| invalid())?;
    if q.catalog_id.is_empty()
        || q.catalog_id.len() > 8192
        || q.q.len() > 8192
        || q.package_id.as_ref().is_some_and(|id| id.len() > 8192)
        || !(1..=200).contains(&q.limit)
        || q.offset > 50_000
    {
        return Err(invalid());
    }
    dependency_work(move || {
        let catalog = s.requested_catalog(&q.catalog_id)?;
        if q.package_id
            .as_ref()
            .is_some_and(|id| !catalog.packages.iter().any(|p| &p.id == id))
        {
            return Err(missing());
        }
        let search = q.q.to_lowercase();
        let mut matches = catalog
            .symbols
            .iter()
            .filter(|symbol| {
                q.package_id
                    .as_ref()
                    .is_none_or(|id| &symbol.package_id == id)
                    && (search.is_empty()
                        || symbol.qualified_name.to_lowercase().contains(&search)
                        || symbol.name.to_lowercase().contains(&search))
            })
            .skip(q.offset);
        let items: Vec<_> = matches.by_ref().take(q.limit).collect();
        let next_offset = matches.next().map(|_| q.offset + items.len());
        let response = Json(
            json!({"catalogId":catalog.id,"workspaceRevision":catalog.workspace_revision,
            "items":items,"nextOffset":next_offset}),
        );
        s.check_catalog(&catalog)?;
        Ok(response)
    })
    .await
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DependencySourceQuery {
    catalog_id: String,
    source_ref: String,
}
async fn dependency_source(
    State(s): State<Arc<DaemonState>>,
    query: Result<Query<DependencySourceQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(q) = query.map_err(|_| invalid())?;
    if q.catalog_id.is_empty()
        || q.catalog_id.len() > 8192
        || q.source_ref.is_empty()
        || q.source_ref.len() > 8192
    {
        return Err(invalid());
    }
    dependency_work(move || {
        let catalog = s.requested_catalog(&q.catalog_id)?;
        let source = catalog.sources.get(&q.source_ref).ok_or_else(missing)?;
        // This descriptor is retained from discovery. No user-selected paths or reopened roots.
        let text = source.directory.read_file(&source.path).map_err(rust_source_error)?;
        let hash = hex::encode(Sha256::digest(text.as_bytes()));
        if hash != source.hash { return Err(dependency_stale()); }
        let package = catalog.packages.iter().find(|package| package.id == source.package_id).ok_or_else(missing)?;
        let definitions: Vec<_> = catalog.symbols.iter().filter(|symbol| symbol.source_ref == q.source_ref)
            .map(|symbol| json!({"id":symbol.id,"name":symbol.name,"kind":symbol.kind,
                "parent":symbol.parent,"path":symbol.path,"range":symbol.range})).collect();
        let file = SourceFile { path: source.path.clone(), hash: hash.clone(), language: "rust".into(), text };
        let response = Json(json!({"id":q.source_ref,"rootId":package.id,"rootLabel":package.name,
            "path":source.path,"hash":hash,"file":file,"definitions":definitions,
            "warnings":["Definitional candidates only; not confirmed callees. Separate from the workspace graph and source-sharing scope."]}));
        s.check_catalog(&catalog)?;
        Ok(response)
    }).await
}

async fn rust_source_roots(State(s): State<Arc<DaemonState>>) -> Json<Value> {
    Json(
        json!({"roots": s.rust_sources.iter().map(|r| json!({"id": r.label, "label": r.label, "path": r.directory.root.to_string_lossy()})).collect::<Vec<_>>()}),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RustTreeQuery {
    root: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "file_limit")]
    limit: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RustFileQuery {
    root: String,
    path: String,
}
fn rust_source_error(e: std::io::Error) -> ApiError {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::NotFound => missing(),
        ErrorKind::FileTooLarge => ApiError(
            StatusCode::PAYLOAD_TOO_LARGE,
            "source_too_large",
            "Source exceeds 2 MiB limit",
        ),
        ErrorKind::PermissionDenied => ApiError(
            StatusCode::FORBIDDEN,
            "source_forbidden",
            "Source access is not allowed",
        ),
        ErrorKind::InvalidInput | ErrorKind::InvalidData | ErrorKind::NotADirectory => {
            browse_invalid()
        }
        _ if e.raw_os_error() == Some(libc::ELOOP) => ApiError(
            StatusCode::FORBIDDEN,
            "symlink_forbidden",
            "Symlink traversal is not allowed",
        ),
        _ => ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "source_error",
            "Source could not be read",
        ),
    }
}
async fn rust_source_tree(
    State(s): State<Arc<DaemonState>>,
    query: Result<Query<RustTreeQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<crate::file_tree::Page>, ApiError> {
    let Query(q) = query.map_err(|_| invalid())?;
    if !crate::file_tree::valid_path(&q.path)
        || !(1..=200).contains(&q.limit)
        || q.offset > crate::file_tree::SCAN_LIMIT
    {
        return Err(browse_invalid());
    }
    let revision = db(s.clone(), |s| s.status()).await?.revision;
    tokio::task::spawn_blocking(move || {
        let root = s
            .rust_sources
            .iter()
            .find(|r| r.label == q.root)
            .ok_or_else(missing)?;
        let (items, next_offset, truncated) = root
            .directory
            .list(&q.path, q.offset, q.limit)
            .map_err(rust_source_error)?;
        Ok(Json(crate::file_tree::Page {
            root: root.directory.root.to_string_lossy().into_owned(),
            indexed_workspace: String::new(),
            path: q.path,
            revision,
            items,
            next_offset,
            truncated,
        }))
    })
    .await
    .map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "source_error",
            "Source worker failed",
        )
    })?
}
async fn rust_source_file(
    State(s): State<Arc<DaemonState>>,
    query: Result<Query<RustFileQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<crate::rust_sources::Snapshot>, ApiError> {
    let Query(q) = query.map_err(|_| invalid())?;
    if q.path.is_empty() || !crate::file_tree::valid_path(&q.path) || !q.path.ends_with(".rs") {
        return Err(browse_invalid());
    }
    tokio::task::spawn_blocking(move || {
        let root = s
            .rust_sources
            .iter()
            .find(|r| r.label == q.root)
            .ok_or_else(missing)?;
        root.snapshot(&q.path).map(Json).map_err(rust_source_error)
    })
    .await
    .map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "source_error",
            "Source worker failed",
        )
    })?
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TreeQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "file_limit")]
    limit: usize,
}
async fn tree(
    State(s): State<Arc<DaemonState>>,
    query: Result<Query<TreeQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<crate::file_tree::Page>, ApiError> {
    let Query(q) = query.map_err(|_| browse_invalid())?;
    if !crate::file_tree::valid_path(&q.path)
        || !(1..=200).contains(&q.limit)
        || q.offset > crate::file_tree::SCAN_LIMIT
    {
        return Err(browse_invalid());
    }
    tokio::task::spawn_blocking(move || {
        s.store.verify_root()?;
        let (mut items, next_offset, truncated) = s
            .browser
            .list(&q.path, q.offset, q.limit)
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => ApiError(
                    StatusCode::NOT_FOUND,
                    "directory_missing",
                    "Directory no longer exists; refresh its parent",
                ),
                std::io::ErrorKind::PermissionDenied => ApiError(
                    StatusCode::FORBIDDEN,
                    "directory_forbidden",
                    "Permission denied while listing directory",
                ),
                std::io::ErrorKind::NotADirectory | std::io::ErrorKind::InvalidInput => ApiError(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_directory",
                    "Path must be a real directory; symlink traversal is not allowed",
                ),
                _ if e.raw_os_error() == Some(libc::ELOOP) => ApiError(
                    StatusCode::FORBIDDEN,
                    "symlink_forbidden",
                    "Symlink traversal is not allowed",
                ),
                _ => ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "directory_error",
                    "Directory could not be read; check permissions and refresh",
                ),
            })?;
        let (revision, indexed_workspace) = s.store.tree_metadata(&s.browser.root, &mut items)?;
        for item in items
            .iter_mut()
            .filter(|e| e.kind == "file" && e.indexed_path.is_none())
        {
            let absolute = s.browser.root.join(&item.path);
            let reason = if !absolute.starts_with(&indexed_workspace) {
                "Outside indexed workspace"
            } else {
                match absolute
                    .extension()
                    .and_then(|extension| extension.to_str())
                {
                    Some("ts" | "tsx") => "TypeScript indexing not supported yet",
                    Some("js" | "mjs" | "cjs" | "rs" | "java" | "py") => {
                        "Not indexed yet (may be excluded or size-limited)"
                    }
                    _ => "Unsupported source type",
                }
            };
            item.unindexed_reason = Some(reason.into());
        }
        s.store.verify_root()?;
        Ok(Json(crate::file_tree::Page {
            root: s.browser.root.to_string_lossy().into_owned(),
            indexed_workspace,
            path: q.path,
            revision,
            items,
            next_offset,
            truncated,
        }))
    })
    .await
    .map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "directory_error",
            "Directory worker failed",
        )
    })?
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PinQuery {
    index_generation: Option<String>,
    index_revision: Option<String>,
}
impl PinQuery {
    fn pin(&self) -> Result<Option<IndexPin>, ApiError> {
        match (&self.index_generation, &self.index_revision) {
            (None, None) => Ok(None),
            (Some(generation), Some(revision)) => {
                let revision = revision.parse::<u64>().map_err(|_| invalid())?;
                let value = json!({"indexGeneration":generation,"indexRevision":revision});
                Ok(Some(serde_json::from_value(value).map_err(|_| invalid())?))
            }
            _ => Err(invalid()),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FilesQuery {
    #[serde(flatten)]
    pin: PinQuery,
    #[serde(default)]
    offset: usize,
    #[serde(default = "file_limit")]
    limit: usize,
}
fn file_limit() -> usize {
    200
}
fn browse_invalid() -> ApiError {
    ApiError(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "Invalid browse or sequence request",
    )
}
async fn files(
    State(s): State<Arc<DaemonState>>,
    query: Result<Query<FilesQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(q) = query.map_err(|_| browse_invalid())?;
    if !(1..=200).contains(&q.limit) || q.offset > i64::MAX as usize {
        return Err(browse_invalid());
    }
    let pin = q.pin.pin()?;
    Ok(Json(
        db(s, move |s| s.files_at(pin, q.offset, q.limit)).await?,
    ))
}
async fn methods(
    State(s): State<Arc<DaemonState>>,
    query: Result<Query<SourceQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(q) = query.map_err(|_| browse_invalid())?;
    if q.path.is_empty()
        || q.path.len() > 8192
        || q.path.contains(['\0', '\\', ':'])
        || q.path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(browse_invalid());
    }
    let pin = q.pin.pin()?;
    Ok(Json(
        db(s, move |s| s.methods_at(&q.path, pin))
            .await?
            .ok_or_else(missing)?,
    ))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SequenceRequest {
    seed: String,
    expected_revision: IndexPin,
    #[serde(default)]
    show_all: bool,
}
async fn sequence(
    State(s): State<Arc<DaemonState>>,
    body: Bytes,
) -> Result<Json<crate::behavior::SequenceView>, ApiError> {
    let q: SequenceRequest = serde_json::from_slice(&body).map_err(|_| browse_invalid())?;
    if q.seed.is_empty() || q.seed.len() > 8192 || q.seed.contains('\0') {
        return Err(browse_invalid());
    }
    let result = tokio::task::spawn_blocking(move || {
        let mut view = s
            .store
            .sequence_at(&q.seed, q.expected_revision, q.show_all)?;
        if let Some(view) = view.as_mut()
            && let Some(catalog) = s.catalog_snapshot(view.revision)
            && let Some((_, file)) = s.store.source_at(&view.seed.path, Some(view.revision))?
        {
            crate::dependency_links::annotate(view, &file, &catalog);
        }
        Ok::<_, anyhow::Error>(view)
    })
    .await
    .map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "Operation failed",
        )
    })?
    .map_err(|e| {
        if e.to_string().starts_with("revision conflict")
            || e.to_string().starts_with("index_not_ready")
            || e.to_string().starts_with("incompatible_index")
        {
            e.into()
        } else {
            browse_invalid()
        }
    })?;
    Ok(Json(result.ok_or_else(missing)?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClassesQuery {
    path: Option<String>,
    #[serde(default)]
    q: String,
    #[serde(flatten)]
    pin: PinQuery,
    #[serde(default)]
    offset: usize,
    #[serde(default = "class_limit")]
    limit: usize,
}
fn class_limit() -> usize {
    100
}
async fn classes(
    State(s): State<Arc<DaemonState>>,
    query: Result<Query<ClassesQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<crate::class_diagram::ClassPage>, ApiError> {
    let Query(q) = query.map_err(|_| invalid())?;
    let pin = q.pin.pin()?;
    Ok(Json(
        db(s, move |s| {
            s.classes_at(q.path.as_deref(), &q.q, pin, q.offset, q.limit)
        })
        .await?,
    ))
}
async fn navigation(
    State(s): State<Arc<DaemonState>>,
    body: Bytes,
) -> Result<Json<crate::navigation::NavigationResult>, ApiError> {
    if body.len() > 32 * 1024 {
        return Err(invalid());
    }
    let request: crate::navigation::NavigationRequest =
        serde_json::from_slice(&body).map_err(|_| invalid())?;
    request.validate()?;
    Ok(Json(db(s, move |s| s.navigation_at(&request)).await?))
}

async fn class_diagram(
    State(s): State<Arc<DaemonState>>,
    body: Bytes,
) -> Result<Json<crate::class_diagram::ClassDiagram>, ApiError> {
    let request: crate::class_diagram::ClassDiagramRequest =
        serde_json::from_slice(&body).map_err(|_| invalid())?;
    request.validate().map_err(ApiError::from)?;
    Ok(Json(db(s, move |s| s.class_diagram_at(&request)).await?))
}

async fn status(
    State(s): State<Arc<DaemonState>>,
    uri: axum::http::Uri,
) -> Result<Json<IndexStatus>, ApiError> {
    if uri.query().is_some() {
        return Err(invalid());
    }
    Ok(Json(db(s, |s| s.status()).await?))
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IndexRequest {
    expected_revision: Option<IndexPin>,
}
fn now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}
impl From<crate::store::requests::Request> for IndexJob {
    fn from(row: crate::store::requests::Request) -> Self {
        let error = row
            .error_code
            .map(|code| json!({"code":code,"message":"Index job failed"}));
        Self {
            id: row.id,
            state: row.state,
            progress: IndexProgress::default(),
            revision: row.revision,
            error,
            submitted_at: row.submitted_at,
            started_at: row.started_at,
            finished_at: row.finished_at,
        }
    }
}
async fn start_index(
    State(s): State<Arc<DaemonState>>,
    body: Bytes,
) -> Result<(StatusCode, Json<IndexJob>), ApiError> {
    let request: IndexRequest = if body.is_empty() {
        IndexRequest::default()
    } else {
        let value: Value = serde_json::from_slice(&body).map_err(|_| invalid())?;
        if value.get("expectedRevision").is_some_and(Value::is_null) {
            return Err(invalid());
        }
        serde_json::from_value(value).map_err(|_| invalid())?
    };
    // Existing exceptional index-only recreation stays on its compatibility path
    // until the following root/recovery slice integrates its quiescence barrier.
    if s.store.is_recreate_pending() {
        if request.expected_revision.is_some() {
            return Err(ApiError::from(anyhow::anyhow!(
                "revision conflict: exceptional recovery has no decodable prior pin"
            )));
        }
        return start_exceptional_index(s);
    }
    let options = s.options.clone();
    let row = db(s.clone(), move |store| {
        store.enqueue_request(&options, request.expected_revision)
    })
    .await?;
    let job = IndexJob::from(row);
    // A later durable admission supersedes the legacy exceptional display slot.
    s.jobs.lock().unwrap().current = None;
    s.pending_requests.lock().unwrap().push(job.id.clone());
    s.start_queue_tick();
    Ok((StatusCode::ACCEPTED, Json(job)))
}
// Exceptional recovery has no decodable prior pin. Reserve the only job and detach
// any old daemon owner atomically, then drop that owner before the worker tries EX.
// An unrelated protected reader remains in control of BUSY; no retry is implicit.
#[allow(dead_code)]
fn start_exceptional_index(s: Arc<DaemonState>) -> Result<(StatusCode, Json<IndexJob>), ApiError> {
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let job = IndexJob {
        id: uuid::Uuid::new_v4().to_string(),
        state: "running".into(),
        progress: IndexProgress::default(),
        revision: None,
        error: None,
        submitted_at: now(),
        started_at: Some(now()),
        finished_at: None,
    };
    let old_owner = {
        let mut jobs = s.jobs.lock().unwrap();
        if jobs
            .current
            .as_ref()
            .and_then(|id| jobs.jobs.get(id))
            .is_some_and(|j| j.finished_at.is_none())
        {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "job_active",
                "An index job is already active",
            ));
        }
        if !s.store.is_recreate_pending() {
            return Err(ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "index_not_ready",
                "Exceptional recovery disposition changed; retry explicit indexing",
            ));
        }
        // All admission paths touching both locks use jobs -> serving_session.
        let old_owner = s.serving_session.lock().unwrap().take();
        jobs.current = Some(job.id.clone());
        jobs.cancel = cancel.clone();
        jobs.jobs.insert(job.id.clone(), job.clone());
        old_owner
    };
    drop(old_owner);
    let id = job.id.clone();
    tokio::spawn(async move {
        let worker = s.clone();
        let worker_id = id.clone();
        let worker_cancel = cancel.clone();
        let result = tokio::task::spawn_blocking(move || {
            crate::index_coordinator::reconcile_workspace(
                &worker.store,
                &worker.options,
                &worker_cancel,
                |p| {
                    if let Some(j) = worker.jobs.lock().unwrap().jobs.get_mut(&worker_id) {
                        j.progress = p;
                    }
                },
            )
        })
        .await;
        let outcome = match result {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!("index worker failed")),
        };
        finish_exceptional_index_job(&s, &id, outcome, &cancel, |_| {});
    });
    Ok((StatusCode::ACCEPTED, Json(job)))
}

// A successful worker keeps its leader guard inside the returned Arc even across
// spawn_blocking handoff. Validation and any test barrier run while that Arc lives.
// Install it before completing the job; never publish a success without an owner.
#[allow(dead_code)]
fn finish_exceptional_index_job(
    s: &Arc<DaemonState>,
    id: &str,
    result: anyhow::Result<(IndexPin, Arc<crate::store::topology::LeaderSession>)>,
    cancel: &CancelFlag,
    before_install: impl FnOnce(&Arc<crate::store::topology::LeaderSession>),
) {
    let result = result.and_then(|(pin, session)| {
        s.store.verify_leader_session(&session)?;
        anyhow::ensure!(
            s.store.status()?.revision == pin,
            "revision conflict: exceptional recovery pin changed before owner installation"
        );
        before_install(&session);
        s.store.verify_leader_session(&session)?;
        Ok((pin, session))
    });
    let mut abandoned = None;
    let mut jobs = s.jobs.lock().unwrap();
    let mut serving = s.serving_session.lock().unwrap();
    let job = jobs
        .jobs
        .get_mut(id)
        .expect("started exceptional index job");
    let published = match result {
        Ok((_pin, session)) if serving.is_some() => {
            abandoned = Some(session);
            job.state = "failed".into();
            job.error = Some(json!({"code":"index_failed","message":"Index job failed"}));
            false
        }
        Ok((pin, session)) => {
            // A late cancellation cannot undo durable activation. Retain the
            // verified owner and report the committed revision as completed.
            *serving = Some(session);
            job.revision = Some(pin);
            job.state = "done".into();
            true
        }
        Err(error) if cancel.load(Ordering::Acquire) => {
            let _ = error;
            job.state = "failed".into();
            job.error = Some(json!({"code":"index_failed","message":"Index job failed"}));
            false
        }
        Err(error) => {
            job.state = "failed".into();
            let code = if error.to_string().starts_with("revision conflict") {
                "revision_conflict"
            } else if error.to_string().starts_with("storage_busy") {
                "storage_busy"
            } else {
                "index_failed"
            };
            job.error = Some(json!({"code":code,"message":"Index job failed"}));
            false
        }
    };
    job.finished_at = Some(now());
    drop(serving);
    drop(jobs);
    drop(abandoned);
    if published {
        *s.packets.lock().unwrap() = PacketCache::default();
        s.start_dependency_index();
        s.start_queue_tick();
    }
}

// A failed publication cannot release cached packet ownership. Only a committed
// revision transition clears packets; old barriers still block their public reads.
#[allow(dead_code)]
fn finish_index_job(
    s: &Arc<DaemonState>,
    id: &str,
    result: anyhow::Result<IndexPin>,
    cancel: &CancelFlag,
) {
    let mut jobs = s.jobs.lock().unwrap();
    let job = jobs.jobs.get_mut(id).expect("started index job");
    match result {
        Ok(revision) => {
            job.state = "completed".into();
            job.revision = Some(revision);
        }
        Err(error) if cancel.load(Ordering::Acquire) => {
            let _ = error;
            job.state = "cancelled".into();
        }
        Err(error) => {
            job.state = "failed".into();
            let code = if error.to_string().starts_with("revision conflict") {
                "revision_conflict"
            } else {
                "index_failed"
            };
            job.error = Some(json!({"code":code,"message":"Index job failed"}));
        }
    }
    job.finished_at = Some(now());
    let completed = job.state == "completed";
    drop(jobs);
    if completed {
        *s.packets.lock().unwrap() = PacketCache::default();
        s.start_dependency_index();
    }
}
// The exceptional index-only recovery worker predates requests.db. Until the
// root/recovery slice moves it into the queue, its current ID remains readable
// through the authenticated routes; ordinary jobs always use durable rows.
async fn current_job(
    State(s): State<Arc<DaemonState>>,
) -> Result<Json<Option<IndexJob>>, ApiError> {
    let legacy = {
        let jobs = s.jobs.lock().unwrap();
        jobs.current
            .as_ref()
            .and_then(|id| jobs.jobs.get(id))
            .cloned()
    };
    if legacy.is_some() {
        db(s, |store| store.verify_root()).await?;
        return Ok(Json(legacy));
    }
    let row = db(s.clone(), |store| store.current_request()).await?;
    Ok(Json(row.map(|row| {
        let mut job = IndexJob::from(row);
        if job.finished_at.is_none()
            && let Some(progress) = s.job_progress.lock().unwrap().get(&job.id)
        {
            job.progress = progress.clone();
        }
        job
    })))
}
async fn job(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> Result<Json<IndexJob>, ApiError> {
    let legacy = s.jobs.lock().unwrap().jobs.get(&id).cloned();
    if let Some(legacy) = legacy {
        db(s, |store| store.verify_root()).await?;
        return Ok(Json(legacy));
    }
    let row = db(s.clone(), move |store| store.request_by_id(&id))
        .await?
        .ok_or_else(missing)?;
    let mut job = IndexJob::from(row);
    if job.finished_at.is_none()
        && let Some(progress) = s.job_progress.lock().unwrap().get(&job.id)
    {
        job.progress = progress.clone();
    }
    Ok(Json(job))
}
async fn cancel_job(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> Result<Json<IndexJob>, ApiError> {
    let legacy = s.jobs.lock().unwrap().jobs.get(&id).cloned();
    if let Some(legacy) = legacy {
        db(s, |store| store.verify_root()).await?;
        if legacy.finished_at.is_some() {
            return Ok(Json(legacy));
        }
        return Err(ApiError(
            StatusCode::CONFLICT,
            "request_not_cancellable",
            "Accepted requests cannot be cancelled",
        ));
    }
    let row = db(s.clone(), move |store| store.request_by_id(&id))
        .await?
        .ok_or_else(missing)?;
    if row.finished_at.is_none() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "request_not_cancellable",
            "Accepted requests cannot be cancelled",
        ));
    }
    // Terminal responses are immutable durable rows. Do not mix in late
    // process-local progress when cancel is read after the initial GET.
    Ok(Json(IndexJob::from(row)))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SymbolsQuery {
    #[serde(default)]
    q: String,
    #[serde(default = "limit")]
    limit: usize,
}
fn limit() -> usize {
    50
}
async fn symbols(
    State(s): State<Arc<DaemonState>>,
    Query(q): Query<SymbolsQuery>,
) -> Result<Json<Value>, ApiError> {
    if q.q.len() > 8192 || !(1..=150).contains(&q.limit) {
        return Err(invalid());
    }
    let (revision, items) = db(s, move |s| s.symbols_at(&q.q, q.limit)).await?;
    Ok(Json(json!({"revision":revision,"items":items})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SymbolQuery {
    id: String,
    #[serde(flatten)]
    pin: PinQuery,
}
async fn symbol(
    State(s): State<Arc<DaemonState>>,
    Query(q): Query<SymbolQuery>,
) -> Result<Json<Value>, ApiError> {
    if q.id.is_empty() || q.id.len() > 8192 || q.id.contains('\0') {
        return Err(invalid());
    }
    let pin = q.pin.pin()?;
    let (revision, symbol) = db(s, move |s| s.symbol_at(&q.id, pin))
        .await?
        .ok_or_else(missing)?;
    Ok(Json(json!({"revision":revision,"symbol":symbol})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceQuery {
    path: String,
    #[serde(flatten)]
    pin: PinQuery,
}
async fn source(
    State(s): State<Arc<DaemonState>>,
    Query(q): Query<SourceQuery>,
) -> Result<Json<Value>, ApiError> {
    if q.path.is_empty()
        || q.path.len() > 8192
        || q.path.contains(['\0', '\\', ':'])
        || q.path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(invalid());
    }
    let pin = q.pin.pin()?;
    let (revision, file) = db(s, move |s| s.source_at(&q.path, pin))
        .await?
        .ok_or_else(missing)?;
    Ok(Json(json!({"revision":revision,"file":file})))
}
async fn query(
    State(s): State<Arc<DaemonState>>,
    pin: Result<Query<PinQuery>, axum::extract::rejection::QueryRejection>,
    body: Result<Json<ViewQuery>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<ViewResult>, ApiError> {
    let Query(pin) = pin.map_err(|_| invalid())?;
    let expected = pin.pin()?;
    let Json(q) = body.map_err(|_| invalid())?;
    q.validate().map_err(|_| invalid())?;
    Ok(Json(
        db(s, move |s| s.query_view_at(&q, expected.as_ref()))
            .await?
            .ok_or_else(missing)?,
    ))
}
async fn views(
    State(s): State<Arc<DaemonState>>,
    pin: Result<Query<PinQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Vec<SavedViewState>>, ApiError> {
    let Query(pin) = pin.map_err(|_| invalid())?;
    let expected = pin.pin()?;
    Ok(Json(db(s, move |s| s.saved_views_at(expected)).await?))
}
async fn view(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    pin: Result<Query<PinQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<SavedViewState>, ApiError> {
    validate_record_id(&id).map_err(|_| invalid())?;
    let Query(pin) = pin.map_err(|_| invalid())?;
    let expected = pin.pin()?;
    Ok(Json(
        db(s, move |s| s.saved_view_at(&id, expected))
            .await?
            .ok_or_else(missing)?,
    ))
}
async fn save_view(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    pin: Result<Query<PinQuery>, axum::extract::rejection::QueryRejection>,
    body: Result<Json<SavedViewRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<SavedViewState>, ApiError> {
    let Query(pin) = pin.map_err(|_| invalid())?;
    let pin = pin.pin()?.ok_or_else(invalid)?;
    let Json(v) = body.map_err(|_| invalid())?;
    if v.id != id {
        return Err(invalid());
    }
    v.validate().map_err(|_| invalid())?;
    Ok(Json(db(s, move |s| s.save_view_at(pin, &v)).await?))
}
async fn delete_view(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    validate_record_id(&id).map_err(|_| invalid())?;
    db(s, move |s| s.delete_view(&id)).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn annotations(
    State(s): State<Arc<DaemonState>>,
    pin: Result<Query<PinQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Vec<AnnotationState>>, ApiError> {
    let Query(pin) = pin.map_err(|_| invalid())?;
    let expected = pin.pin()?;
    Ok(Json(
        db(s, move |s| s.saved_annotations_at(expected)).await?,
    ))
}
async fn save_annotation(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    pin: Result<Query<PinQuery>, axum::extract::rejection::QueryRejection>,
    body: Result<Json<AnnotationRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<AnnotationState>, ApiError> {
    let Query(pin) = pin.map_err(|_| invalid())?;
    let pin = pin.pin()?.ok_or_else(invalid)?;
    let Json(a) = body.map_err(|_| invalid())?;
    if a.id != id {
        return Err(invalid());
    }
    a.validate().map_err(|_| invalid())?;
    Ok(Json(db(s, move |s| s.save_annotation_at(pin, &a)).await?))
}
async fn delete_annotation(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    validate_record_id(&id).map_err(|_| invalid())?;
    db(s, move |s| s.delete_annotation(&id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

// These endpoints only transform locally indexed evidence. They never contact a provider.
fn question_error(e: anyhow::Error) -> ApiError {
    let message = e.to_string();
    if message.starts_with("revision conflict")
        || [
            "root_changed",
            "root_key_collision",
            "workspace_id_changed",
            "index_not_ready",
            "storage_busy",
            "incompatible_index",
            "incompatible_record",
            "incomplete_record",
            "recovery_required",
            "unsafe_index",
        ]
        .iter()
        .any(|prefix| message.starts_with(prefix))
        || e.chain().any(|cause| {
            cause
                .downcast_ref::<rusqlite::Error>()
                .is_some_and(|error| {
                    matches!(error, rusqlite::Error::SqliteFailure(info, _) if matches!(info.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
                })
        })
    {
        e.into()
    } else if message == "question seed not found" {
        missing()
    } else if message.contains("176000") {
        ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "evidence_too_large",
            "Jev request exceeds 176000 bytes. Narrow the evidence scope; complete source files cannot be truncated.",
        )
    } else if message.contains("1 MiB") {
        ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "evidence_too_large",
            "Complete question packet exceeds 1 MiB. Narrow the evidence scope; sources cannot be truncated.",
        )
    } else {
        ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_question_selection",
            "Invalid question or selection. Use this packet's exact candidate IDs and provide one valid decision per candidate.",
        )
    }
}
async fn question_work<T: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Operation failed",
            )
        })?
        .map_err(question_error)
}
async fn question_preview(
    State(s): State<Arc<DaemonState>>,
    body: Result<Json<QuestionRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = body.map_err(|_| invalid())?;
    request.validate().map_err(|_| invalid())?;
    question_work(move || {
        let response = s.store.evidence_response()?;
        let packet = planning::prepare_in(&response, request)?;
        let selection = planning::preview(&packet)?;
        let view = planning::assemble(&packet, &selection, "localPreview")?;
        let bytes = serde_json::to_vec(&packet)?.len();
        anyhow::ensure!(
            bytes <= MAX_PACKET_BYTES,
            "complete question packet exceeds 1 MiB"
        );
        let cached = Arc::new(packet.clone());
        let result = Json(QuestionPreview {
            packet,
            selection,
            view,
        })
        .into_response();
        // Admission to the cache is part of response assembly, before the final fence.
        s.packets
            .lock()
            .unwrap()
            .remember_fenced(cached, bytes, || {
                #[cfg(test)]
                if let Some(hook) = s.preview_finish_hook.lock().unwrap().as_ref() {
                    hook();
                }
                response.finish(())
            })?;
        Ok(result)
    })
    .await
}
async fn cached_packet(
    s: Arc<DaemonState>,
    id: String,
) -> Result<(Arc<QuestionPacket>, EvidenceFence), ApiError> {
    // Gate readiness before touching cached evidence, including stale packets.
    let response = s.store.evidence_response()?;
    let revision = response.status()?.revision;
    let packet = {
        let cache = s.packets.lock().unwrap();
        cache
            .packets
            .iter()
            .find(|(p, _)| p.packet_id == id)
            .map(|(p, _)| p.clone())
            .ok_or_else(missing)?
    };
    if revision != packet.revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "revision_conflict",
            "The index revision changed",
        ));
    }
    // A packet hash authenticates only the in-memory copy. Recheck its exact
    // selected graph and source witnesses under one pinned SQLite snapshot
    // before exporting, displaying, or sending cached evidence to a provider.
    let witness = packet.clone();
    response.validate_selected_view(&witness.context, &witness.source_files)?;
    let fence = response.into_fence(EvidenceFencePolicy::ExactPin(packet.revision));
    Ok((packet, fence))
}
// Always run the post-materialization fence, even when assembly fails. Preserve
// the original materialization error, as Store::with_evidence does.
fn finish_packet_result(
    fence: &EvidenceFence,
    result: Result<Response, ApiError>,
) -> Result<Response, ApiError> {
    let checked = fence.finish(()).map_err(ApiError::from);
    match result {
        Err(error) => Err(error),
        Ok(response) => {
            checked?;
            Ok(response)
        }
    }
}
async fn question_export(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let (packet, fence) = cached_packet(s, id).await?;
    let result = question_work(move || jev::request_for(&packet))
        .await
        .map(|value| Json(value).into_response());
    finish_packet_result(&fence, result)
}
#[derive(Serialize)]
struct QuestionSelection {
    selection: SelectionEnvelope,
    view: FocusedView,
}
async fn question_import(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(response): Json<Value>,
) -> Result<Response, ApiError> {
    let (packet, fence) = cached_packet(s, id).await?;
    let result = question_work(move || {
        let selection = jev::parse_response(&packet, &response)?;
        let warnings = jev::response_warnings(&response);
        let mut view = planning::assemble(&packet, &selection, "importedJev")?;
        view.warnings.extend(warnings);
        Ok(QuestionSelection { selection, view })
    })
    .await
    .map(|value| Json(value).into_response());
    finish_packet_result(&fence, result)
}
async fn question_selection(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(selection): Json<SelectionEnvelope>,
) -> Result<Response, ApiError> {
    let (packet, fence) = cached_packet(s, id).await?;
    let result = question_work(move || {
        let view = planning::assemble(&packet, &selection, "manual")?;
        Ok(QuestionSelection { selection, view })
    })
    .await
    .map(|value| Json(value).into_response());
    finish_packet_result(&fence, result)
}

// Status only reads the local ledger. No endpoint other than jev-run contacts Jev.
async fn acp_status(State(s): State<Arc<DaemonState>>) -> Result<Json<Value>, ApiError> {
    let Some(provider) = s.acp.clone() else {
        return Ok(Json(json!({"enabled":false,"status":null})));
    };
    let status = tokio::task::spawn_blocking(move || provider.status())
        .await
        .map_err(|_| acp_error(anyhow::anyhow!("status unavailable")))?
        .map_err(acp_error)?;
    Ok(Json(json!({"enabled":true,"status":status})))
}
fn acp_error(e: anyhow::Error) -> ApiError {
    match e.to_string().as_str() {
        "ACP allowance exhausted" => ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "acp_exhausted",
            "ACP attempt allowance exhausted",
        ),
        "ACP invalid answer" => ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "acp_invalid_answer",
            "ACP answer failed citation validation. Any reservation is retained; no automatic retry was made.",
        ),
        "ACP invalid request" => ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "acp_invalid_request",
            "Evidence exceeds the ACP request limits. Prepare a smaller packet. No provider call was made.",
        ),
        "ACP authentication required" => ApiError(
            StatusCode::BAD_GATEWAY,
            "acp_auth_required",
            "ACP could not verify Claude subscription access. Check the Claude Code subscription sign-in on this daemon host. API-key fallback is disabled. The attempt reservation is retained; no automatic retry was made.",
        ),
        "ACP model unavailable" => ApiError(
            StatusCode::BAD_GATEWAY,
            "acp_model_unavailable",
            "Sonnet is unavailable to this Claude subscription session. Check account access to Sonnet before retrying. No alternate model was used. The attempt reservation is retained; no automatic retry was made.",
        ),
        "ACP model mismatch" => ApiError(
            StatusCode::BAD_GATEWAY,
            "acp_model_mismatch",
            "ACP could not confirm Sonnet was selected. Check the configured runner and Claude adapter before retrying. No answer was accepted. The attempt reservation is retained; no automatic retry was made.",
        ),
        _ => ApiError(
            StatusCode::BAD_GATEWAY,
            "acp_failed",
            "ACP attempt failed. Any reservation is retained; no automatic retry was made.",
        ),
    }
}
async fn question_answer(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    // Accept only an empty object: never take client prompts, sources or provider overrides.
    let request: Value = serde_json::from_slice(&body).map_err(|_| invalid())?;
    if !request.as_object().is_some_and(|object| object.is_empty()) {
        return Err(invalid());
    }
    db(s.clone(), |store| store.status().map(|_| ())).await?;
    let provider = s.acp.clone().ok_or(ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "acp_disabled",
        "Live ACP is disabled",
    ))?;
    let (packet, fence) = cached_packet(s.clone(), id).await?;
    let response = provider.run(&packet).await;
    // A stale failure must not replace the state of a newer question either.
    let revision = db(s, |store| Ok(store.status()?.revision)).await;
    let fence_check = fence.finish(()).map_err(ApiError::from);
    let revision = revision?;
    fence_check?;
    if revision != packet.revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "revision_conflict",
            "The index revision changed",
        ));
    }
    let result = response.map_err(acp_error).map(|response| {
        Json(
            json!({"packetId":packet.packet_id,"revision":packet.revision,
            "source":"liveAcp","attemptId":response.attempt_id,"answer":response.answer,
            "latencyMs":response.latency_ms,"estimatedUsd":response.estimated_usd}),
        )
        .into_response()
    });
    finish_packet_result(&fence, result)
}

async fn jev_status(State(s): State<Arc<DaemonState>>) -> Result<Json<Value>, ApiError> {
    let Some(provider) = s.provider.clone() else {
        return Ok(Json(json!({"enabled":false,"budget":null})));
    };
    let budget = db(s, move |_| provider.budget()).await?;
    Ok(Json(json!({"enabled":true,"budget":budget})))
}
fn live_jev_error(e: anyhow::Error) -> ApiError {
    let message = e.to_string();
    if message.starts_with("Jev budget exhausted") {
        ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "jev_budget_exhausted",
            "Jev budget exhausted",
        )
    } else if message.starts_with("invalid Jev request") {
        ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_jev_request",
            "Jev request is invalid or exceeds the request limit. Reduce evidence depth or choose a smaller root, then prepare a new preview. Complete source files cannot be truncated. No provider call was made.",
        )
    } else if message.starts_with("Jev requires at least one candidate") {
        ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "jev_no_candidates",
            "This packet has no candidate calls. Choose a root with outgoing calls and prepare a new preview. No provider call was made.",
        )
    } else if message == "Jev attempt failed: invalid_selection" {
        ApiError(
            StatusCode::BAD_GATEWAY,
            "jev_invalid_response",
            "The provider response failed validation and was saved for inspection. The attempt reservation is retained; no automatic retry was made.",
        )
    } else if message == "Jev attempt failed: context_exceeded" {
        ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "jev_context_exceeded",
            "The provider context limit was exceeded. Reduce evidence depth or choose a smaller root, then prepare a new preview. Complete source files cannot be truncated. The attempt reservation is retained; no automatic retry was made.",
        )
    } else {
        ApiError(
            StatusCode::BAD_GATEWAY,
            "jev_failed",
            "Jev attempt failed. Any reservation is retained; no automatic retry was made.",
        )
    }
}

async fn question_run(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    // No caller-supplied source, credentials, URL, or budget overrides.
    if !body.is_empty() && body.as_ref() != b"{}" {
        return Err(invalid());
    }
    db(s.clone(), |store| store.status().map(|_| ())).await?;
    let provider = s.provider.clone().ok_or(ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "jev_disabled",
        "Live Jev is disabled",
    ))?;
    let (packet, fence) = cached_packet(s.clone(), id).await?;
    let response = provider.run(&packet).await;
    // Check even failed attempts. Reservations remain accounted on stale results.
    let revision = db(s.clone(), |store| Ok(store.status()?.revision)).await;
    let fence_check = fence.finish(()).map_err(ApiError::from);
    let revision = revision?;
    fence_check?;
    if revision != packet.revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "revision_conflict",
            "The index revision changed",
        ));
    }
    let result = match response.map_err(live_jev_error) {
        Err(error) => Err(error),
        Ok(response) => {
            let selection = response.selection.clone();
            question_work(move || planning::assemble(&packet, &selection, "liveJev"))
                .await
                .map(|mut view| {
                    view.warnings.extend(response.warnings.clone());
                    Json(json!({"selection":response.selection,"view":view,
                        "attemptId":response.attempt_id,"latencyMs":response.latency_ms,
                        "estimatedUsd":response.estimated_usd,"usage":response.usage,
                        "warnings":response.warnings}))
                    .into_response()
                })
        }
    };
    finish_packet_result(&fence, result)
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use crate::indexer::index_workspace_bundle;
    use tower::ServiceExt;

    #[test]
    fn outer_http_fence_covers_native_composition_not_durable_payloads() {
        for path in [
            "/api/status",
            "/api/tree",
            "/api/files",
            "/api/methods",
            "/api/sequence",
            "/api/classes",
            "/api/class-diagram",
            "/api/navigation",
            "/api/symbols",
            "/api/symbol",
            "/api/source",
            "/api/query",
            "/api/dependencies",
            "/api/dependencies/symbols",
            "/api/dependencies/source",
            "/api/rust-sources/tree",
        ] {
            assert!(native_response_route(path), "{path}");
        }
        for path in [
            "/api/views",
            "/api/views/saved",
            "/api/annotations",
            "/api/rust-sources/file",
            "/api/healthz",
            "/api/jobs/current",
        ] {
            assert!(!native_response_route(path), "{path}");
        }
    }
    #[test]
    fn question_error_keeps_typed_refusals_and_selection_errors() {
        for (message, code) in [
            ("root_changed: fixture", "root_changed"),
            ("recovery_required: fixture", "recovery_required"),
            ("unsafe_index: fixture", "unsafe_index"),
            ("index_not_ready: fixture", "index_not_ready"),
        ] {
            assert_eq!(question_error(anyhow::anyhow!("{message}")).1, code);
        }
        assert_eq!(
            question_error(anyhow::anyhow!("revision conflict")).1,
            "revision_conflict"
        );
        assert_eq!(
            question_error(anyhow::anyhow!("invalid selection")).1,
            "invalid_question_selection"
        );
        assert_eq!(
            question_error(anyhow::anyhow!("complete question packet exceeds 1 MiB")).1,
            "evidence_too_large"
        );
        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        );
        assert_eq!(question_error(busy.into()).1, "storage_busy");
    }

    #[tokio::test]
    async fn preview_root_change_after_serialization_discards_packet() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("a.js"), "function go() { measured(); }\n").unwrap();
        let options = IndexOptions::new(workspace.clone());
        let cancel = Arc::new(AtomicBool::new(false));
        let store = Store::open_for_tests(&dir.path().join("state"), &workspace).unwrap();
        let (graph, native, capture) =
            index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap();
        let seed = graph
            .nodes
            .iter()
            .find(|node| node.name == "go")
            .unwrap()
            .id
            .clone();
        let session = store.leader_session().unwrap();
        let pin = store
            .publish_native(
                &graph,
                &capture,
                &native,
                session.leader_guard().unwrap(),
                store.index_baseline().unwrap(),
                &cancel,
            )
            .unwrap();
        let state = new(
            store,
            options,
            "0123456789abcdef".repeat(4),
            "127.0.0.1:7331".parse().unwrap(),
        )
        .unwrap();
        state.retain_serving_session(session);
        let root = workspace.clone();
        *state.preview_finish_hook.lock().unwrap() = Some(Arc::new(move || {
            std::fs::rename(&root, root.with_file_name("replaced-workspace")).unwrap();
            std::fs::create_dir(&root).unwrap();
        }));
        let app = router(state.clone());
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/questions/preview")
            .header("host", "127.0.0.1:7331")
            .header(
                "authorization",
                "Bearer 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"seed":seed,"question":"what does go do?","expectedRevision":pin})
                    .to_string(),
            ))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(body["error"]["code"], "root_changed");
        assert!(body.get("packet").is_none());
        assert!(state.packets.lock().unwrap().packets.is_empty());
    }

    #[test]
    fn acp_controlled_errors_are_exact_and_sanitized() {
        for (message, code) in [
            ("ACP authentication required", "acp_auth_required"),
            ("ACP model unavailable", "acp_model_unavailable"),
            ("ACP model mismatch", "acp_model_mismatch"),
        ] {
            let known = acp_error(anyhow::anyhow!("{message}"));
            assert_eq!(known.0, StatusCode::BAD_GATEWAY);
            assert_eq!(known.1, code);
            assert!(known.2.contains("no automatic retry"));
            let untrusted = acp_error(anyhow::anyhow!("{message} private output"));
            assert_eq!(untrusted.1, "acp_failed");
            assert!(!untrusted.2.contains("private output"));
        }
    }

    #[tokio::test]
    async fn live_response_is_rejected_if_snapshot_changes_during_call() {
        mock_run(true, "success").await;
    }

    #[tokio::test]
    async fn pinned_packet_conflicts_even_when_provider_fails() {
        mock_run(true, "context_exceeded").await;
    }

    #[tokio::test]
    async fn packet_fence_survives_provider_await_and_takeover() {
        mock_run(false, "takeover").await;
    }

    #[tokio::test]
    async fn live_response_is_labeled_and_accounted() {
        mock_run(false, "success").await;
    }

    #[tokio::test]
    async fn corrupt_selected_cached_source_prevents_live_provider_call() {
        mock_run(false, "selected_corruption").await;
    }

    #[tokio::test]
    async fn context_exceeded_is_actionable_and_retains_reservation() {
        mock_run(false, "context_exceeded").await;
    }

    #[test]
    fn live_error_classification_is_allowlisted_and_redacted() {
        for (message, status, code) in [
            (
                "invalid Jev request: private source",
                422,
                "invalid_jev_request",
            ),
            (
                "Jev requires at least one candidate",
                422,
                "jev_no_candidates",
            ),
            (
                "Jev attempt failed: context_exceeded",
                422,
                "jev_context_exceeded",
            ),
            (
                "Jev attempt failed: context_exceeded private source",
                502,
                "jev_failed",
            ),
            (
                "Jev budget exhausted: private source",
                429,
                "jev_budget_exhausted",
            ),
            (
                "Jev attempt failed: invalid_selection",
                502,
                "jev_invalid_response",
            ),
            (
                "Jev attempt failed: invalid_selection private source",
                502,
                "jev_failed",
            ),
            ("private source", 502, "jev_failed"),
        ] {
            let error = live_jev_error(anyhow::anyhow!(message));
            assert_eq!(error.0.as_u16(), status);
            assert_eq!(error.1, code);
            assert!(!error.2.contains("private source"));
        }
    }

    #[tokio::test]
    async fn rounded_live_response_warns_and_invalid_response_is_actionable() {
        mock_run(false, "rounded").await;
        mock_run(false, "invalid_selection").await;
    }

    async fn mock_run(change_snapshot: bool, scenario: &'static str) {
        // Loopback mock only: no environment credentials or external endpoint.
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let notify = entered.clone();
        let gate = release.clone();
        let mock = Router::new().route("/", post(move |Json(request): Json<Value>| {
            let notify = notify.clone();
            let gate = gate.clone();
            async move {
                notify.notify_one();
                gate.notified().await;
                if scenario == "context_exceeded" {
                    return (StatusCode::BAD_REQUEST, Json(json!({"detail":{"error_type":"max_tokens_exceeded"}}))).into_response();
                }
                let answers: serde_json::Map<String, Value> = request["questions"].as_object().unwrap()
                    .keys().map(|key| (key.clone(), json!({"type":"choice","choice":"essential","confidence":1.0,
                    "probabilities":{"essential":if scenario == "rounded" {0.69} else if scenario == "invalid_selection" {0.6} else {1.0},"supporting":if scenario == "success" {0.0} else {0.2},"incidental":if scenario == "success" {0.0} else {0.1},"uncertain":0.0}}))).collect();
                Json(json!({"model":"jev-1.13.0","answers":answers})).into_response()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, mock).await.unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(
            workspace.join("a.js"),
            "function seed() { console.log('synthetic'); }",
        )
        .unwrap();
        let options = IndexOptions::new(workspace.clone());
        let cancel = Arc::new(AtomicBool::new(false));
        let store = Store::open_for_tests(&dir.path().join("state"), &workspace).unwrap();
        crate::store::topology::assert_topology_fixture(&store, &dir.path().join("state"));
        let (graph, native, capture) =
            index_workspace_bundle(&options, store.root_id(), &cancel, |_| {}).unwrap();
        let seed = graph
            .nodes
            .iter()
            .find(|node| node.name == "seed")
            .unwrap()
            .id
            .clone();
        let session = store.leader_session().unwrap();
        store
            .publish_native(
                &graph,
                &capture,
                &native,
                session.leader_guard().unwrap(),
                store.index_baseline().unwrap(),
                &cancel,
            )
            .unwrap();
        let provider = Arc::new(
            LiveJev::open(
                &dir.path().join("budget"),
                "synthetic".into(),
                10,
                &workspace,
            )
            .unwrap()
            .with_test_endpoint(endpoint),
        );
        let token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let state = new_with_jev(
            store.clone(),
            options,
            token.into(),
            "127.0.0.1:7331".parse().unwrap(),
            Some(provider.clone()),
        )
        .unwrap();
        state.retain_serving_session(session.clone());
        let app = router(state);
        let request = |path: &str, body: Value| {
            axum::http::Request::builder()
                .method("POST")
                .uri(path)
                .header("host", "127.0.0.1:7331")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        let preview = app
            .clone()
            .oneshot(request(
                "/api/questions/preview",
                json!({"seed":seed,"question":"logging", "expectedRevision":store.status().unwrap().revision}),
            ))
            .await
            .unwrap();
        assert_eq!(preview.status(), StatusCode::OK);
        let bytes = to_bytes(preview.into_body(), 1024 * 1024).await.unwrap();
        let preview: Value = serde_json::from_slice(&bytes).unwrap();
        let path = format!(
            "/api/questions/{}/jev-run",
            preview["packet"]["packetId"].as_str().unwrap()
        );
        if scenario == "selected_corruption" {
            let index_root = dir.path().join("state/cache/indexes");
            let db_path = std::fs::read_dir(index_root)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| path.is_dir())
                .unwrap()
                .join("index.db");
            let db = rusqlite::Connection::open(db_path).unwrap();
            let mut bytes: Vec<u8> = db
                .query_row(
                    "SELECT source_bytes FROM native_documents WHERE path='a.js'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            bytes[0] ^= 1;
            assert_eq!(
                db.execute(
                    "UPDATE native_documents SET source_bytes=?1 WHERE path='a.js'",
                    [bytes],
                )
                .unwrap(),
                1
            );
            assert_eq!(
                store.status().unwrap().revision,
                serde_json::from_value(preview["packet"]["revision"].clone()).unwrap()
            );
            let attempts = provider.budget().unwrap().attempts;
            let response = app.oneshot(request(&path, json!({}))).await.unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            let body: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                    .unwrap();
            assert_eq!(body["error"]["code"], "incompatible_index");
            assert_eq!(provider.budget().unwrap().attempts, attempts);
            server.abort();
            return;
        }
        let run = tokio::spawn(app.oneshot(request(&path, json!({}))));
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        if change_snapshot {
            let (updated, native, captured) = index_workspace_bundle(
                &IndexOptions::new(workspace.clone()),
                store.root_id(),
                &cancel,
                |_| {},
            )
            .unwrap();
            let expected = store.status().unwrap().revision;
            let publisher = store.clone();
            let owner = session.clone();
            let publishing = tokio::task::spawn_blocking(move || {
                publisher.publish_native(
                    &updated,
                    &captured,
                    &native,
                    owner.leader_guard().unwrap(),
                    expected,
                    &cancel,
                )
            });
            // The provider is still blocked. This commit must not wait for a
            // SQLite read transaction held by the cached packet's outer fence.
            tokio::time::timeout(std::time::Duration::from_secs(5), publishing)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        if scenario == "takeover" {
            use std::io::{Seek, SeekFrom, Write};
            fn lock_in(dir: &std::path::Path) -> Option<std::path::PathBuf> {
                for entry in std::fs::read_dir(dir).ok()?.flatten() {
                    let path = entry.path();
                    if path.file_name().is_some_and(|name| name == "leader.lock") {
                        return Some(path);
                    }
                    if path.is_dir()
                        && let Some(found) = lock_in(&path)
                    {
                        return Some(found);
                    }
                }
                None
            }
            let lock = lock_in(&dir.path().join("state")).unwrap();
            let mut file = std::fs::OpenOptions::new().write(true).open(lock).unwrap();
            file.seek(SeekFrom::Start(0)).unwrap();
            file.write_all(uuid::Uuid::new_v4().to_string().as_bytes())
                .unwrap();
            file.sync_all().unwrap();
        }
        release.notify_one();
        let response = tokio::time::timeout(std::time::Duration::from_secs(5), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if change_snapshot {
            assert_eq!(response.status(), StatusCode::CONFLICT);
            let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            assert!(!String::from_utf8_lossy(&body).contains("liveJev"));
        } else if scenario == "takeover" {
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            let body: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                    .unwrap();
            assert_eq!(body["error"]["code"], "index_not_ready");
            assert!(body.get("view").is_none());
        } else if scenario == "invalid_selection" {
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["error"]["code"], "jev_invalid_response");
            let message = body["error"]["message"].as_str().unwrap();
            assert!(message.contains("saved for inspection"));
            assert!(message.contains("no automatic retry"));
            assert!(!message.contains("probabilities"));
        } else if scenario == "context_exceeded" {
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["error"]["code"], "jev_context_exceeded");
            let message = body["error"]["message"].as_str().unwrap();
            assert!(message.contains("reservation is retained"));
            assert!(message.contains("cannot be truncated"));
            assert!(!message.contains("private source"));
        } else {
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["view"]["selectionSource"], "liveJev");
            assert_eq!(
                body["view"]["revision"],
                json!(store.status().unwrap().revision)
            );
            assert!(body["view"]["calls"].as_array().unwrap().len() <= 5);
            assert!(body["attemptId"].as_str().is_some());
            if scenario == "rounded" {
                let warnings = body["warnings"].as_array().unwrap();
                assert!(!warnings.is_empty());
                for warning in warnings {
                    assert!(
                        body["view"]["warnings"]
                            .as_array()
                            .unwrap()
                            .contains(warning)
                    );
                }
            } else {
                assert_eq!(body["warnings"], json!([]));
            }
        }
        assert_eq!(provider.budget().unwrap().reserved_cents, 10);
        assert_eq!(provider.budget().unwrap().attempts, 1);
        server.abort();
    }
}

#[cfg(test)]
mod queue_idle_follower_tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn verified_holder_needs_no_index_open_and_lost_holder_triggers_takeover() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("a.js"), "function a() {}\n").unwrap();
        let state_root = tmp.path().join("state");
        let store = Store::open_for_tests(&state_root, &root).unwrap();
        let options = IndexOptions::new(root);
        let (old_pin, owner) = crate::index_coordinator::reconcile_workspace(
            &store,
            &options,
            &Arc::new(AtomicBool::new(false)),
            |_| {},
        )
        .unwrap();
        let follower_store = Store::open_for_tests(&state_root, &options.workspace_root).unwrap();
        let follower = follower_store.follower_session().unwrap();
        let state = new(
            follower_store.clone(),
            options.clone(),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            "127.0.0.1:7331".parse().unwrap(),
        )
        .unwrap();
        *state.serving_session.lock().unwrap() = Some(follower);
        let accepted = follower_store.enqueue_request(&options, None).unwrap();
        state
            .pending_requests
            .lock()
            .unwrap()
            .push(accepted.id.clone());
        for _ in 0..3 {
            state.queue_tick().unwrap();
        }
        assert_eq!(
            state.queue_takeover_attempts.load(Ordering::Acquire),
            0,
            "verified follower must never reopen index.db to probe leadership"
        );
        assert_eq!(
            follower_store
                .request_by_id(&accepted.id)
                .unwrap()
                .unwrap()
                .state,
            "queued"
        );
        drop(owner);
        state.queue_tick().unwrap();
        assert_eq!(state.queue_takeover_attempts.load(Ordering::Acquire), 1);
        assert!(state.retained_serving_session().unwrap().is_leader());
        let completed = follower_store.request_by_id(&accepted.id).unwrap().unwrap();
        assert_eq!(completed.state, "done");
        assert!(completed.revision.unwrap().index_revision > old_pin.index_revision);
    }
    #[tokio::test]
    async fn failed_pending_read_keeps_same_id_for_holder_loss_takeover() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("a.js"), "function a() {}\n").unwrap();
        let state_root = tmp.path().join("state");
        let owner_store = Store::open_for_tests(&state_root, &root).unwrap();
        let options = IndexOptions::new(root);
        let (_, owner) = crate::index_coordinator::reconcile_workspace(
            &owner_store,
            &options,
            &Arc::new(AtomicBool::new(false)),
            |_| {},
        )
        .unwrap();
        let follower_store = Store::open_for_tests(&state_root, &options.workspace_root).unwrap();
        let follower = follower_store.follower_session().unwrap();
        let state = new(
            follower_store.clone(),
            options.clone(),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            "127.0.0.1:7331".parse().unwrap(),
        )
        .unwrap();
        *state.serving_session.lock().unwrap() = Some(follower);
        let request = follower_store.enqueue_request(&options, None).unwrap();
        state
            .pending_requests
            .lock()
            .unwrap()
            .push(request.id.clone());
        state.test_pending_read_failures.store(1, Ordering::Release);
        state.queue_tick().unwrap();
        assert_eq!(
            *state.pending_requests.lock().unwrap(),
            vec![request.id.clone()]
        );
        assert_eq!(
            follower_store
                .request_by_id(&request.id)
                .unwrap()
                .unwrap()
                .state,
            "queued"
        );
        drop(owner);
        state.queue_tick().unwrap();
        assert!(state.retained_serving_session().unwrap().is_leader());
        assert_eq!(
            follower_store
                .request_by_id(&request.id)
                .unwrap()
                .unwrap()
                .state,
            "done"
        );
        assert_eq!(state.queue_takeover_attempts.load(Ordering::Acquire), 1);
    }
}

#[cfg(test)]
mod exceptional_recovery_tests {
    use super::*;
    use crate::store::topology::{TopologyRoots, WorkspaceIdentity};

    fn fixture() -> (
        tempfile::TempDir,
        Store,
        Arc<DaemonState>,
        TopologyRoots,
        WorkspaceIdentity,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.js"), "function a() {}\n").unwrap();
        let roots = TopologyRoots::isolated_for_tests(
            tmp.path().join("state/cache"),
            tmp.path().join("state/data"),
        );
        let identity = WorkspaceIdentity::discover(Some(&root), &root).unwrap();
        let old = Store::open_for_tests(&tmp.path().join("state"), &root).unwrap();
        drop(old);
        std::fs::write(roots.index_db(&identity), b"bad sqlite index header").unwrap();
        let store = Store::open_for_tests(&tmp.path().join("state"), &root).unwrap();
        assert!(store.is_recreate_pending());
        let state = new(
            store.clone(),
            IndexOptions::new(root),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            "127.0.0.1:7331".parse().unwrap(),
        )
        .unwrap();
        (tmp, store, state, roots, identity)
    }

    #[tokio::test]
    async fn concurrent_posts_admit_one_exceptional_worker() {
        // The current-thread executor cannot poll the spawned worker between
        // these synchronous admissions. No timer or filesystem race is needed.
        let (_tmp, _store, state, _roots, _identity) = fixture();
        let (status, first) = start_exceptional_index(state.clone())
            .unwrap_or_else(|error| panic!("unexpected admission: {} {}", error.1, error.2));
        assert_eq!(status, StatusCode::ACCEPTED);
        let error = start_index(State(state.clone()), Bytes::from_static(b"{}"))
            .await
            .err()
            .unwrap();
        assert_eq!(error.0, StatusCode::CONFLICT);
        assert_eq!(error.1, "job_active");
        assert_eq!(state.jobs.lock().unwrap().jobs.len(), 1);
        let id = first.0.id;
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if state.jobs.lock().unwrap().jobs[&id].finished_at.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn multiple_posts_commit_fifo_while_other_process_holds_leader() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.js"), "function a() {}\n").unwrap();
        let store = Store::open_for_tests(&tmp.path().join("state"), &root).unwrap();
        let owner = store.leader_session().unwrap();
        let state = new(
            store.clone(),
            IndexOptions::new(root),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            "127.0.0.1:7331".parse().unwrap(),
        )
        .unwrap();
        let (first_code, first) = start_index(State(state.clone()), Bytes::new())
            .await
            .unwrap();
        let (second_code, second) = start_index(State(state.clone()), Bytes::new())
            .await
            .unwrap();
        assert_eq!(
            (first_code, second_code),
            (StatusCode::ACCEPTED, StatusCode::ACCEPTED)
        );
        assert_eq!(
            (first.0.state.as_str(), second.0.state.as_str()),
            ("queued", "queued")
        );
        let a = store.request_by_id(&first.0.id).unwrap().unwrap();
        let b = store.request_by_id(&second.0.id).unwrap().unwrap();
        assert!(a.seq < b.seq);
        assert_eq!(store.current_request().unwrap().unwrap().id, b.id);
        drop(owner);
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                if store
                    .request_by_id(&second.0.id)
                    .unwrap()
                    .unwrap()
                    .finished_at
                    .is_some()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(store.request_by_id(&a.id).unwrap().unwrap().state, "done");
        assert_eq!(store.request_by_id(&b.id).unwrap().unwrap().state, "done");
    }

    #[tokio::test]
    async fn leader_remains_locked_across_worker_result_and_atomic_install() {
        let (_tmp, store, state, roots, identity) = fixture();
        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        let options = state.options.clone();
        let id = uuid::Uuid::new_v4().to_string();
        {
            let mut jobs = state.jobs.lock().unwrap();
            jobs.current = Some(id.clone());
            jobs.cancel = cancel.clone();
            jobs.jobs.insert(
                id.clone(),
                IndexJob {
                    id: id.clone(),
                    state: "running".into(),
                    progress: IndexProgress::default(),
                    revision: None,
                    error: None,
                    submitted_at: now(),
                    started_at: Some(now()),
                    finished_at: None,
                },
            );
        }
        let result =
            crate::index_coordinator::reconcile_workspace(&store, &options, &cancel, |_| {})
                .unwrap();
        let pin = result.0;
        let before = &state;
        finish_exceptional_index_job(&state, &id, Ok(result), &cancel, |session| {
            assert!(session.is_leader());
            assert!(before.retained_serving_session().is_err());
            assert!(
                roots
                    .leader(&identity)
                    .unwrap_err()
                    .to_string()
                    .contains("storage_busy")
            );
            assert_eq!(store.status().unwrap().revision, pin);
        });
        assert_eq!(state.jobs.lock().unwrap().jobs[&id].state, "done");
        assert_eq!(state.jobs.lock().unwrap().jobs[&id].revision, Some(pin));
        assert!(state.retained_serving_session().unwrap().is_leader());
        assert!(
            roots
                .leader(&identity)
                .unwrap_err()
                .to_string()
                .contains("storage_busy")
        );
    }

    #[tokio::test]
    async fn cancellation_after_durable_recovery_reports_completed_with_new_owner() {
        let (_tmp, store, state, roots, identity) = fixture();
        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        let id = uuid::Uuid::new_v4().to_string();
        {
            let mut jobs = state.jobs.lock().unwrap();
            jobs.current = Some(id.clone());
            jobs.cancel = cancel.clone();
            jobs.jobs.insert(
                id.clone(),
                IndexJob {
                    id: id.clone(),
                    state: "running".into(),
                    progress: IndexProgress::default(),
                    revision: None,
                    error: None,
                    submitted_at: now(),
                    started_at: Some(now()),
                    finished_at: None,
                },
            );
        }
        let result =
            crate::index_coordinator::reconcile_workspace(&store, &state.options, &cancel, |_| {})
                .unwrap();
        let pin = result.0;
        cancel.store(true, Ordering::Release);
        finish_exceptional_index_job(&state, &id, Ok(result), &cancel, |_| {});
        let job = state.jobs.lock().unwrap().jobs[&id].clone();
        assert_eq!(job.state, "done");
        assert_eq!(job.revision, Some(pin));
        assert!(state.retained_serving_session().unwrap().is_leader());
        assert_eq!(store.status().unwrap().revision, pin);
        assert!(
            roots
                .leader(&identity)
                .unwrap_err()
                .to_string()
                .contains("storage_busy")
        );
    }

    #[tokio::test]
    async fn failed_worker_never_installs_owner_or_reports_completed() {
        let (_tmp, store, state, roots, identity) = fixture();
        let before = std::fs::read(roots.index_db(&identity)).unwrap();
        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        let id = uuid::Uuid::new_v4().to_string();
        {
            let mut jobs = state.jobs.lock().unwrap();
            jobs.current = Some(id.clone());
            jobs.jobs.insert(
                id.clone(),
                IndexJob {
                    id: id.clone(),
                    state: "running".into(),
                    progress: IndexProgress::default(),
                    revision: None,
                    error: None,
                    submitted_at: now(),
                    started_at: Some(now()),
                    finished_at: None,
                },
            );
        }
        // The Store's own fault tests cover actual post-rename failures. Here
        // exercise the HTTP handoff for their error result: no false success.
        finish_exceptional_index_job(
            &state,
            &id,
            Err(anyhow::anyhow!("after rename fsync failed")),
            &cancel,
            |_| panic!("failed worker cannot cross install barrier"),
        );
        assert_eq!(state.jobs.lock().unwrap().jobs[&id].state, "failed");
        assert!(state.jobs.lock().unwrap().jobs[&id].revision.is_none());
        assert!(state.retained_serving_session().is_err());
        assert!(store.evidence_response().is_err());
        assert_eq!(std::fs::read(roots.index_db(&identity)).unwrap(), before);
    }
}

#[cfg(test)]
mod normal_post_capture_cancellation_tests {
    use super::*;
    use crate::store::topology::{TopologyRoots, WorkspaceIdentity};
    use rusqlite::{OpenFlags, types::Value as SqlValue};
    use std::{fs, time::Duration};
    use tower::ServiceExt;

    #[derive(Debug, PartialEq)]
    struct PairSnapshot {
        pin: IndexPin,
        rows: BTreeMap<String, Vec<String>>,
    }

    // Read all derived graph, native, class and source rows in one SQLite snapshot.
    fn pair_snapshot(index: &std::path::Path) -> PairSnapshot {
        let db =
            rusqlite::Connection::open_with_flags(index, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        db.execute_batch("BEGIN DEFERRED").unwrap();
        let (generation, revision): (String, i64) = db
            .query_row(
                "SELECT index_generation,index_revision FROM index_metadata WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let pin = IndexPin {
            index_generation: uuid::Uuid::parse_str(&generation).unwrap(),
            index_revision: u64::try_from(revision).unwrap(),
        };
        let mut tables = vec![
            "files".to_owned(),
            "nodes".to_owned(),
            "calls".to_owned(),
            "regions".to_owned(),
            "class_catalog".to_owned(),
            "classes".to_owned(),
            "class_relations".to_owned(),
        ];
        let mut names = db
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name GLOB 'native_*' ORDER BY name")
            .unwrap();
        tables.extend(
            names
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap(),
        );
        drop(names);
        assert!(tables.iter().any(|table| table == "native_documents"));
        let mut rows = BTreeMap::new();
        for table in tables {
            assert!(
                table
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            );
            let mut stmt = db.prepare(&format!("SELECT * FROM {table}")).unwrap();
            let columns = stmt.column_count();
            let mut values = stmt
                .query_map([], |row| {
                    let cells = (0..columns)
                        .map(|column| row.get::<_, SqlValue>(column))
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(format!("{cells:?}"))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            values.sort();
            rows.insert(table, values);
        }
        db.execute_batch("ROLLBACK").unwrap();
        PairSnapshot { pin, rows }
    }

    async fn api(
        app: &Router,
        token: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let request = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:7331")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(
                body.map(|value| value.to_string()).unwrap_or_default(),
            ))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn accepted_job_cannot_be_cancelled_and_full_pair_changes_only_after_leader_drains() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("one.js"),
            "function seed() { old_step(); } function old_step() {}\n",
        )
        .unwrap();
        let state_root = tmp.path().join("state");
        let roots =
            TopologyRoots::isolated_for_tests(state_root.join("cache"), state_root.join("data"));
        let identity = WorkspaceIdentity::discover(Some(&root), &root).unwrap();
        let index = roots.index_db(&identity);
        let store = Store::open_for_tests(&state_root, &root).unwrap();
        let options = IndexOptions::new(root.clone());
        let (_, owner) = crate::index_coordinator::reconcile_workspace(
            &store,
            &options,
            &Arc::new(AtomicBool::new(false)),
            |_| {},
        )
        .unwrap();
        let before = pair_snapshot(&index);
        fs::write(
            root.join("one.js"),
            "function seed() { fresh(); } function fresh() {}\n",
        )
        .unwrap();
        fs::write(root.join("two.js"), "function extra() {}\n").unwrap();
        let token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let state = new(
            store.clone(),
            options,
            token.into(),
            "127.0.0.1:7331".parse().unwrap(),
        )
        .unwrap();
        let app = router(state.clone());
        let (code, accepted) = api(&app, token, "POST", "/api/index", Some(json!({}))).await;
        assert_eq!(code, StatusCode::ACCEPTED, "{accepted}");
        assert_eq!(accepted["state"], "queued");
        let id = accepted["id"].as_str().unwrap();
        let (code, rejected) =
            api(&app, token, "POST", &format!("/api/jobs/{id}/cancel"), None).await;
        assert_eq!(code, StatusCode::CONFLICT, "{rejected}");
        assert_eq!(rejected["error"]["code"], "request_not_cancellable");
        assert_eq!(pair_snapshot(&index), before);
        assert_eq!(store.request_by_id(id).unwrap().unwrap().state, "queued");
        drop(owner);
        let completed = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let (code, row) = api(&app, token, "GET", &format!("/api/jobs/{id}"), None).await;
                assert_eq!(code, StatusCode::OK, "{row}");
                if !row["finishedAt"].is_null() {
                    break row;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(completed["state"], "done", "{completed}");
        let after = pair_snapshot(&index);
        assert!(after.pin.index_revision > before.pin.index_revision);
        assert_ne!(after.rows["files"], before.rows["files"]);
        assert_ne!(
            after.rows["native_documents"],
            before.rows["native_documents"]
        );
        assert!(after.rows["files"].iter().any(|row| row.contains("two.js")));
        assert_eq!(
            after.pin,
            serde_json::from_value(completed["revision"].clone()).unwrap()
        );
    }
}

#[cfg(test)]
mod dependency_lifecycle_tests {
    use super::*;
    fn catalog(id: &str, revision: IndexPin) -> Catalog {
        Catalog {
            id: id.into(),
            workspace_revision: revision,
            packages: vec![],
            symbols: vec![],
            warnings: vec![],
            sources: Default::default(),
        }
    }
    #[tokio::test]
    async fn dependency_status_whole_response_fence() {
        use std::io::{Seek, SeekFrom, Write};
        fn find_lock(path: &std::path::Path) -> Option<std::path::PathBuf> {
            for entry in std::fs::read_dir(path).ok()?.flatten() {
                let path = entry.path();
                if path.file_name().is_some_and(|name| name == "leader.lock") {
                    return Some(path);
                }
                if path.is_dir()
                    && let Some(found) = find_lock(&path)
                {
                    return Some(found);
                }
            }
            None
        }
        async fn call(app: &Router, path: &str) -> (StatusCode, Value) {
            use tower::ServiceExt;
            let request = axum::http::Request::builder()
                .uri(path)
                .header("host", "127.0.0.1:7331")
                .header(
                    "authorization",
                    "Bearer 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                )
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            let status = response.status();
            let body =
                serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                    .unwrap();
            (status, body)
        }
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let store = Store::open_for_tests(&temp.path().join("state"), &workspace).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let options = IndexOptions::new(workspace.clone());
        let (graph, native, capture) =
            crate::indexer::index_workspace_bundle(&options, store.root_id(), &cancel, |_| {})
                .unwrap();
        let session = store.leader_session().unwrap();
        let pin = store
            .publish_native(
                &graph,
                &capture,
                &native,
                session.leader_guard().unwrap(),
                store.index_baseline().unwrap(),
                &cancel,
            )
            .unwrap();
        let state = new(
            store.clone(),
            options.clone(),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            "127.0.0.1:7331".parse().unwrap(),
        )
        .unwrap();
        state.retain_serving_session(session);
        {
            let mut deps = state.dependencies.lock().unwrap();
            deps.state = "ready";
            deps.catalog = Some(Arc::new(catalog("matching", pin)));
        }
        let app = router(state.clone());
        let (valid_status, valid) = call(&app, "/api/dependencies").await;
        assert_eq!(valid_status, StatusCode::OK);
        assert_eq!(valid["workspaceRevision"], json!(pin));
        assert_eq!(valid["catalogId"], "matching");
        let lock = find_lock(&temp.path().join("state")).unwrap();
        let matching_lock = lock.clone();
        *state.dependency_capture_hook.lock().unwrap() = Some(Arc::new(move || {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(&matching_lock)
                .unwrap();
            file.seek(SeekFrom::Start(0)).unwrap();
            file.write_all(uuid::Uuid::new_v4().to_string().as_bytes())
                .unwrap();
            file.sync_all().unwrap();
        }));
        let (refused_status, refused) = call(&app, "/api/dependencies").await;
        assert_eq!(refused_status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(refused["error"]["code"], "index_not_ready");
        assert!(refused.get("workspaceRevision").is_none());
        assert!(refused.get("catalogId").is_none());
        *state.dependency_capture_hook.lock().unwrap() = None;
        let incarnation = state
            .retained_serving_session()
            .unwrap()
            .leader_guard()
            .unwrap()
            .incarnation;
        let mut file = std::fs::OpenOptions::new().write(true).open(&lock).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(incarnation.to_string().as_bytes()).unwrap();
        file.sync_all().unwrap();
        let mismatched = IndexPin {
            index_revision: pin.index_revision + 1,
            ..pin
        };
        state.dependencies.lock().unwrap().catalog = Some(Arc::new(catalog("old", mismatched)));
        let (failed_status, failed) = call(&app, "/api/dependencies").await;
        assert_eq!(failed_status, StatusCode::OK);
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["catalogId"], Value::Null);
        assert_eq!(failed["packages"], json!([]));
        assert_eq!(failed["symbolCount"], 0);
        assert_eq!(
            failed["warnings"],
            json!(["Workspace changed; refresh the dependency catalog"])
        );
        let (browse_status, browse) = call(&app, "/api/files").await;
        assert_eq!(browse_status, StatusCode::OK);
        assert_eq!(browse["revision"], json!(pin));
        let publishing_store = store.clone();
        let publishing_options = options.clone();
        let publishing_session = state.retained_serving_session().unwrap();
        *state.outer_fence_hook.lock().unwrap() = Some(Arc::new(move || {
            let cancel = Arc::new(AtomicBool::new(false));
            let (graph, native, capture) = crate::indexer::index_workspace_bundle(
                &publishing_options,
                publishing_store.root_id(),
                &cancel,
                |_| {},
            )
            .unwrap();
            publishing_store
                .publish_native(
                    &graph,
                    &capture,
                    &native,
                    publishing_session.leader_guard().unwrap(),
                    pin,
                    &cancel,
                )
                .unwrap();
        }));
        let (published_status, old_response) = call(&app, "/api/files").await;
        assert_eq!(published_status, StatusCode::OK);
        assert_eq!(old_response["revision"], json!(pin));
        *state.outer_fence_hook.lock().unwrap() = None;
        let current = store.status().unwrap().revision;
        assert_eq!(current.index_revision, pin.index_revision + 1);
        let hook_lock = lock.clone();
        *state.outer_fence_hook.lock().unwrap() = Some(Arc::new(move || {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(&hook_lock)
                .unwrap();
            file.seek(SeekFrom::Start(0)).unwrap();
            file.write_all(uuid::Uuid::new_v4().to_string().as_bytes())
                .unwrap();
            file.sync_all().unwrap();
        }));
        let (browse_status, browse) = call(&app, "/api/files").await;
        assert_eq!(browse_status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(browse["error"]["code"], "index_not_ready");
        assert!(browse.get("revision").is_none());
        assert!(browse.get("files").is_none());
        *state.outer_fence_hook.lock().unwrap() = None;
        let incarnation = state
            .retained_serving_session()
            .unwrap()
            .leader_guard()
            .unwrap()
            .incarnation;
        let mut file = std::fs::OpenOptions::new().write(true).open(&lock).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(incarnation.to_string().as_bytes()).unwrap();
        file.sync_all().unwrap();
    }
    #[test]
    fn refresh_bursts_admit_one_worker_and_keep_only_latest_generation() {
        let mut index = DependencyIndex {
            generation: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            stopped: false,
            worker_running: false,
            state: "loading",
            catalog: None,
            warnings: vec![],
        };
        assert!(index.request_build());
        let first_cancel = index.cancel.clone();
        for _ in 0..1000 {
            assert!(!index.request_build());
        }
        assert!(first_cancel.load(Ordering::Acquire));
        assert!(!index.cancel.load(Ordering::Acquire));
        assert_eq!(index.generation, 1001);
        // A finished driver relinquishes the slot; the next request can restart it.
        index.worker_running = false;
        assert!(index.request_build());
        index.stopped = true;
        assert!(!index.request_build());
        assert_eq!(index.generation, 1002);
    }
    #[test]
    fn stale_generation_cancel_and_revision_never_publish() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let store = Store::open_for_tests(&temp.path().join("state"), &workspace).unwrap();
        crate::store::topology::assert_topology_fixture(&store, &temp.path().join("state"));
        let cancel = Arc::new(AtomicBool::new(false));
        let (graph, native, capture) = crate::indexer::index_workspace_bundle(
            &IndexOptions::new(workspace.clone()),
            store.root_id(),
            &cancel,
            |_| {},
        )
        .unwrap();
        let session = store.leader_session().unwrap();
        let pin0 = store
            .publish_native(
                &graph,
                &capture,
                &native,
                session.leader_guard().unwrap(),
                store.index_baseline().unwrap(),
                &cancel,
            )
            .unwrap();
        let state = new_with_dependency_options(
            store.clone(),
            IndexOptions::new(workspace.clone()),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            "127.0.0.1:7331".parse().unwrap(),
            None,
            None,
            workspace.clone(),
            vec![],
            Some(CatalogOptions::default()),
        )
        .unwrap();
        state.retain_serving_session(session.clone());
        let active = AtomicBool::new(false);
        state.dependencies.lock().unwrap().generation = 2;
        state.publish_dependency_index(2, &active, Ok(catalog("new", pin0)));
        state.publish_dependency_index(1, &active, Ok(catalog("old", pin0)));
        state.publish_dependency_index(1, &active, Err(anyhow::anyhow!("late failure")));
        assert_eq!(state.catalog_snapshot(pin0).unwrap().id, "new");
        state.publish_dependency_index(2, &AtomicBool::new(true), Ok(catalog("cancelled", pin0)));
        assert_eq!(state.catalog_snapshot(pin0).unwrap().id, "new");
        let (updated, native, captured) = crate::indexer::index_workspace_bundle(
            &IndexOptions::new(workspace),
            store.root_id(),
            &cancel,
            |_| {},
        )
        .unwrap();
        store
            .publish_native(
                &updated,
                &captured,
                &native,
                session.leader_guard().unwrap(),
                pin0,
                &cancel,
            )
            .unwrap();
        let pin1 = store.status().unwrap().revision;
        state.publish_dependency_index(2, &active, Ok(catalog("stale-revision", pin0)));
        assert!(state.catalog_snapshot(pin0).is_none());
        assert_eq!(state.dependencies.lock().unwrap().state, "failed");
        state.cancel_active();
        let generation = state.dependencies.lock().unwrap().generation;
        state.publish_dependency_index(generation, &active, Ok(catalog("after-shutdown", pin1)));
        assert!(state.catalog_snapshot(pin1).is_none());
    }
}

#[cfg(test)]
mod rebaseline_packet_cache_tests {
    use super::*;
    use crate::indexer::index_workspace_bundle;
    use std::{fs, sync::atomic::AtomicBool};

    #[test]
    fn preview_fence_failure_restores_evicted_and_same_id_packets() {
        let revision = IndexPin {
            index_generation: uuid::Uuid::new_v4(),
            index_revision: 1,
        };
        let packet = QuestionPacket {
            packet_id: "original".into(),
            revision,
            request: QuestionRequest {
                seed: "seed".into(),
                question: "what?".into(),
                expected_revision: revision,
                evidence_depth: 0,
                max_visible: 1,
                allow_deeper_display: false,
                focus_terms: vec![],
            },
            context: ViewResult {
                revision,
                query: ViewQuery {
                    seed: "seed".into(),
                    depth: 0,
                    max_nodes: 1,
                    max_calls: 1,
                    include_callbacks: false,
                    exclude_paths: vec![],
                },
                nodes: vec![],
                calls: vec![],
                regions: vec![],
                truncated: false,
                omitted_nodes: 0,
                warnings: vec![],
            },
            source_files: vec![],
            warnings: vec![],
        };
        let mut cache = PacketCache::default();
        for i in 0..MAX_PACKETS {
            let mut item = packet.clone();
            item.packet_id = format!("packet-{i}");
            cache.remember(Arc::new(item), MAX_PACKET_BYTES);
        }
        let original: Vec<_> = cache
            .packets
            .iter()
            .map(|(p, bytes)| (p.clone(), *bytes))
            .collect();
        let original_bytes = cache.bytes;
        let mut incoming = packet.clone();
        incoming.packet_id = "new".into();
        let failed = cache.remember_fenced(Arc::new(incoming), MAX_PACKET_BYTES, || {
            anyhow::bail!("index_not_ready: incarnation lost")
        });
        assert!(
            failed
                .unwrap_err()
                .to_string()
                .starts_with("index_not_ready")
        );
        assert_eq!(cache.bytes, original_bytes);
        assert_eq!(cache.packets.len(), MAX_PACKETS);
        for ((actual, size), (before, expected)) in cache.packets.iter().zip(&original) {
            assert!(Arc::ptr_eq(actual, before));
            assert_eq!(size, expected);
        }
        let mut replacement = packet;
        replacement.packet_id = "packet-3".into();
        assert!(
            cache
                .remember_fenced(
                    Arc::new(replacement),
                    MAX_PACKET_BYTES + 1,
                    || anyhow::bail!("root_changed: root replaced")
                )
                .is_err()
        );
        assert_eq!(cache.bytes, original_bytes);
        assert_eq!(cache.packets.len(), MAX_PACKETS);
        for ((actual, size), (before, expected)) in cache.packets.iter().zip(&original) {
            assert!(Arc::ptr_eq(actual, before));
            assert_eq!(size, expected);
        }
    }

    #[test]
    fn failed_known_old_commit_keeps_private_packet_cache_success_clears_it() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::write(workspace.join("one.js"), "function go() { measured(); }\n").unwrap();
        let options = IndexOptions::new(workspace.clone());
        let ready = Arc::new(AtomicBool::new(false));
        let store = Store::open_for_tests(&temp.path().join("state"), &workspace).unwrap();
        let (graph, native, capture) =
            index_workspace_bundle(&options, store.root_id(), &ready, |_| {}).unwrap();
        let old = store
            .publish_native(
                &graph,
                &capture,
                &native,
                &store.leader().unwrap(),
                store.index_baseline().unwrap(),
                &ready,
            )
            .unwrap();
        let db_path = fs::read_dir(temp.path().join("state/cache/indexes"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.is_dir())
            .unwrap()
            .join("index.db");
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.pragma_update(None, "foreign_keys", "OFF").unwrap();
        let tables = {
            let mut stmt = db
                .prepare(
                    "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'native_%'",
                )
                .unwrap();
            stmt.query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        for table in tables {
            db.execute(&format!("DROP TABLE \"{table}\""), []).unwrap();
        }
        for index in ["nodes_path", "calls_path", "regions_path"] {
            db.execute_batch(&format!("DROP INDEX {index}")).unwrap();
        }
        db.execute(
            "UPDATE index_metadata SET schema_version=4,extractor_version='native-v1'",
            [],
        )
        .unwrap();
        db.pragma_update(None, "user_version", 4).unwrap();
        drop(db);
        let old_bytes = fs::read(&db_path).unwrap();
        let state = new(
            store.clone(),
            options,
            "0123456789abcdef".repeat(4),
            "127.0.0.1:7332".parse().unwrap(),
        )
        .unwrap();
        let seed = graph
            .nodes
            .iter()
            .find(|n| n.name == "go")
            .unwrap()
            .id
            .clone();
        let packet = Arc::new(QuestionPacket {
            packet_id: "cached-before-old".into(),
            revision: old,
            request: QuestionRequest {
                seed: seed.clone(),
                question: "what?".into(),
                expected_revision: old,
                evidence_depth: 0,
                max_visible: 1,
                allow_deeper_display: false,
                focus_terms: vec![],
            },
            context: ViewResult {
                revision: old,
                query: ViewQuery {
                    seed,
                    depth: 0,
                    max_nodes: 1,
                    max_calls: 1,
                    include_callbacks: false,
                    exclude_paths: vec![],
                },
                nodes: vec![],
                calls: vec![],
                regions: vec![],
                truncated: false,
                omitted_nodes: 0,
                warnings: vec![],
            },
            source_files: vec![],
            warnings: vec![],
        });
        state.packets.lock().unwrap().remember(packet, 128);
        let register = |id: &str| {
            state.jobs.lock().unwrap().jobs.insert(
                id.into(),
                IndexJob {
                    id: id.into(),
                    state: "running".into(),
                    progress: IndexProgress::default(),
                    revision: None,
                    error: None,
                    submitted_at: now(),
                    started_at: Some(now()),
                    finished_at: None,
                },
            )
        };
        register("failed");
        let cancelled = Arc::new(AtomicBool::new(true));
        let error = store
            .publish_native(
                &graph,
                &capture,
                &native,
                &store.leader().unwrap(),
                old,
                &cancelled,
            )
            .unwrap_err();
        finish_index_job(&state, "failed", Err(error), &cancelled);
        assert_eq!(state.jobs.lock().unwrap().jobs["failed"].state, "cancelled");
        assert_eq!(state.packets.lock().unwrap().packets.len(), 1);
        assert_eq!(state.packets.lock().unwrap().bytes, 128);
        assert_eq!(fs::read(&db_path).unwrap(), old_bytes);
        assert!(
            store
                .status()
                .unwrap_err()
                .to_string()
                .contains("index_not_ready")
        );
        register("committed");
        let revision = store
            .publish_native(
                &graph,
                &capture,
                &native,
                &store.leader().unwrap(),
                old,
                &ready,
            )
            .unwrap();
        finish_index_job(&state, "committed", Ok(revision), &ready);
        assert_ne!(revision.index_generation, old.index_generation);
        assert_eq!(state.packets.lock().unwrap().packets.len(), 0);
        assert_eq!(state.packets.lock().unwrap().bytes, 0);
    }
}
