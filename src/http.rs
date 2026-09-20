//! Loopback-only HTTP API. Database and indexing work runs on blocking workers.
use crate::{
    acp::Acp,
    auth::valid_token,
    dependencies::{Catalog, CatalogOptions},
    indexer::{IndexOptions, index_workspace},
    jev,
    live_jev::LiveJev,
    model::*,
    planning::{
        self, FocusedView, QuestionPacket, QuestionPreview, QuestionRequest, SelectionEnvelope,
    },
    store::Store,
};
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
    pub revision: Option<u64>,
    pub error: Option<Value>,
    pub started_at: String,
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
#[derive(Default)]
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
    options: IndexOptions,
    browser: crate::file_tree::SourceDir,
    rust_sources: Vec<crate::rust_sources::Root>,
    dependency_options: Option<CatalogOptions>,
    dependencies: Mutex<DependencyIndex>,
    token: String,
    hosts: Vec<String>,
    origins: Vec<String>,
    jobs: Mutex<Jobs>,
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
        dependency_options: catalog_options,
        token,
        hosts,
        origins,
        provider: jev,
        acp,
        packets: Mutex::new(PacketCache::default()),
        jobs: Mutex::new(Jobs {
            current: None,
            jobs: BTreeMap::new(),
            cancel: Arc::new(AtomicBool::new(false)),
        }),
    }))
}
impl DaemonState {
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
    pub(crate) fn catalog_snapshot(&self, revision: u64) -> Option<Arc<Catalog>> {
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
struct ApiError(StatusCode, &'static str, &'static str);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error(self.0, self.1, self.2)
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
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
        } else {
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
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
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
async fn dependency_status(State(s): State<Arc<DaemonState>>) -> Result<Json<Value>, ApiError> {
    dependency_work(move || {
        let revision = s.store.status()?.revision;
        let (state, catalog, mut warnings) = {
            let index = s.dependencies.lock().unwrap();
            (index.state, index.catalog.clone(), index.warnings.clone())
        };
        if let Some(catalog) = catalog {
            if catalog.workspace_revision == revision {
                return Ok(Json(json!({"state":state,"workspaceRevision":revision,
                    "catalogId":catalog.id,"packages":catalog.packages,
                    "symbolCount":catalog.symbols.len(),"warnings":catalog.warnings})));
            }
            warnings.push("Workspace changed; refresh the dependency catalog".into());
            return Ok(Json(json!({"state":"failed","workspaceRevision":revision,
                "catalogId":null,"packages":[],"symbolCount":0,"warnings":warnings})));
        }
        Ok(Json(json!({"state":state,"workspaceRevision":revision,
            "catalogId":null,"packages":[],"symbolCount":0,"warnings":warnings})))
    })
    .await
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
    let Query(q) = query.map_err(|_| browse_invalid())?;
    if !crate::file_tree::valid_path(&q.path)
        || !(1..=200).contains(&q.limit)
        || q.offset > crate::file_tree::SCAN_LIMIT
    {
        return Err(browse_invalid());
    }
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
            revision: 0,
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
    let Query(q) = query.map_err(|_| browse_invalid())?;
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
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FilesQuery {
    revision: Option<u64>,
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
        StatusCode::UNPROCESSABLE_ENTITY,
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
    Ok(Json(
        db(s, move |s| s.files_at(q.revision, q.offset, q.limit)).await?,
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
    Ok(Json(
        db(s, move |s| s.methods_at(&q.path, q.revision))
            .await?
            .ok_or_else(missing)?,
    ))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SequenceRequest {
    seed: String,
    expected_revision: u64,
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
        if e.to_string().starts_with("revision conflict") {
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
    revision: Option<u64>,
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
    Ok(Json(
        db(s, move |s| {
            s.classes_at(q.path.as_deref(), &q.q, q.revision, q.offset, q.limit)
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

async fn status(State(s): State<Arc<DaemonState>>) -> Result<Json<IndexStatus>, ApiError> {
    Ok(Json(db(s, |s| s.status()).await?))
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IndexRequest {
    expected_revision: Option<u64>,
}
fn now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}
async fn start_index(
    State(s): State<Arc<DaemonState>>,
    body: Bytes,
) -> Result<(StatusCode, Json<IndexJob>), ApiError> {
    let request: IndexRequest = if body.is_empty() {
        IndexRequest::default()
    } else {
        serde_json::from_slice(&body).map_err(|_| invalid())?
    };
    let baseline = db(s.clone(), |s| s.status()).await?.revision;
    if request.expected_revision.is_some_and(|r| r != baseline) {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "revision_conflict",
            "The index revision changed",
        ));
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let job = IndexJob {
        id: uuid::Uuid::new_v4().to_string(),
        state: "running".into(),
        progress: IndexProgress::default(),
        revision: None,
        error: None,
        started_at: now(),
        finished_at: None,
    };
    {
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
        if jobs.jobs.len() >= 100 {
            let old = jobs
                .jobs
                .iter()
                .min_by_key(|(_, j)| &j.started_at)
                .map(|(id, _)| id.clone());
            if let Some(id) = old {
                jobs.jobs.remove(&id);
            }
        }
        jobs.current = Some(job.id.clone());
        jobs.cancel = cancel.clone();
        jobs.jobs.insert(job.id.clone(), job.clone());
    }
    let id = job.id.clone();
    tokio::spawn(async move {
        let worker = s.clone();
        let worker_id = id.clone();
        let worker_cancel = cancel.clone();
        let result = tokio::task::spawn_blocking(move || {
            let graph = index_workspace(&worker.options, &worker_cancel, |p| {
                if let Some(j) = worker.jobs.lock().unwrap().jobs.get_mut(&worker_id) {
                    j.progress = p;
                }
            })?;
            worker.store.publish(&graph, Some(baseline), &worker_cancel)
        })
        .await;
        let mut jobs = s.jobs.lock().unwrap();
        let j = jobs.jobs.get_mut(&id).unwrap();
        match result {
            Ok(Ok(revision)) => {
                j.state = "completed".into();
                j.revision = Some(revision)
            }
            Ok(Err(e)) => {
                if cancel.load(Ordering::Acquire) {
                    j.state = "cancelled".into()
                } else {
                    j.state = "failed".into();
                    let code = if e.to_string().starts_with("revision conflict") {
                        "revision_conflict"
                    } else {
                        "index_failed"
                    };
                    j.error = Some(json!({"code":code,"message":"Index job failed"}));
                }
            }
            Err(_) => {
                j.state = "failed".into();
                j.error = Some(json!({"code":"index_failed","message":"Index job failed"}));
            }
        }
        j.finished_at = Some(now());
        let completed = j.state == "completed";
        drop(jobs);
        if completed {
            s.start_dependency_index();
        }
    });
    Ok((StatusCode::ACCEPTED, Json(job)))
}
async fn current_job(State(s): State<Arc<DaemonState>>) -> Json<Option<IndexJob>> {
    let jobs = s.jobs.lock().unwrap();
    Json(
        jobs.current
            .as_ref()
            .and_then(|id| jobs.jobs.get(id))
            .cloned(),
    )
}
async fn job(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> Result<Json<IndexJob>, ApiError> {
    Ok(Json(
        s.jobs
            .lock()
            .unwrap()
            .jobs
            .get(&id)
            .cloned()
            .ok_or_else(missing)?,
    ))
}
async fn cancel_job(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> Result<Json<IndexJob>, ApiError> {
    let mut jobs = s.jobs.lock().unwrap();
    let active = jobs.current.as_ref() == Some(&id);
    if active {
        jobs.cancel.store(true, Ordering::Release)
    }
    let j = jobs.jobs.get_mut(&id).ok_or_else(missing)?;
    if active && j.finished_at.is_none() {
        j.state = "cancelling".into()
    }
    Ok(Json(j.clone()))
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
    revision: Option<u64>,
}
async fn symbol(
    State(s): State<Arc<DaemonState>>,
    Query(q): Query<SymbolQuery>,
) -> Result<Json<Value>, ApiError> {
    if q.id.is_empty() || q.id.len() > 8192 || q.id.contains('\0') {
        return Err(invalid());
    }
    let (revision, symbol) = db(s, move |s| s.symbol_at(&q.id, q.revision))
        .await?
        .ok_or_else(missing)?;
    Ok(Json(json!({"revision":revision,"symbol":symbol})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceQuery {
    path: String,
    revision: Option<u64>,
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
    let (revision, file) = db(s, move |s| s.source_at(&q.path, q.revision))
        .await?
        .ok_or_else(missing)?;
    Ok(Json(json!({"revision":revision,"file":file})))
}
async fn query(
    State(s): State<Arc<DaemonState>>,
    Json(q): Json<ViewQuery>,
) -> Result<Json<ViewResult>, ApiError> {
    q.validate().map_err(|_| invalid())?;
    Ok(Json(
        db(s, move |s| s.query_view(&q))
            .await?
            .ok_or_else(missing)?,
    ))
}
async fn views(State(s): State<Arc<DaemonState>>) -> Result<Json<Vec<SavedViewState>>, ApiError> {
    Ok(Json(db(s, |s| s.views()).await?))
}
async fn view(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> Result<Json<SavedViewState>, ApiError> {
    validate_record_id(&id).map_err(|_| invalid())?;
    Ok(Json(
        db(s, move |s| s.view(&id)).await?.ok_or_else(missing)?,
    ))
}
async fn save_view(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(v): Json<SavedView>,
) -> Result<Json<SavedViewState>, ApiError> {
    if v.id != id {
        return Err(invalid());
    }
    v.validate().map_err(|_| invalid())?;
    Ok(Json(
        db(s, move |s| {
            s.put_view(&v)?;
            s.view(&id)?
                .ok_or_else(|| anyhow::anyhow!("saved view missing"))
        })
        .await?,
    ))
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
) -> Result<Json<Vec<AnnotationState>>, ApiError> {
    Ok(Json(db(s, |s| s.annotations()).await?))
}
async fn save_annotation(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(a): Json<Annotation>,
) -> Result<Json<AnnotationState>, ApiError> {
    if a.id != id {
        return Err(invalid());
    }
    a.validate().map_err(|_| invalid())?;
    Ok(Json(
        db(s, move |s| {
            s.put_annotation(&a)?;
            s.annotations()?
                .into_iter()
                .find(|v| v.annotation.id == id)
                .ok_or_else(|| anyhow::anyhow!("saved annotation missing"))
        })
        .await?,
    ))
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
    if message.starts_with("revision conflict") {
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
    Json(request): Json<QuestionRequest>,
) -> Result<Json<QuestionPreview>, ApiError> {
    request.validate().map_err(|_| invalid())?;
    let worker = s.clone();
    let (result, cached, bytes) = question_work(move || {
        let packet = planning::prepare(&worker.store, request)?;
        let selection = planning::preview(&packet)?;
        let view = planning::assemble(&packet, &selection, "localPreview")?;
        let bytes = serde_json::to_vec(&packet)?.len();
        anyhow::ensure!(
            bytes <= MAX_PACKET_BYTES,
            "complete question packet exceeds 1 MiB"
        );
        let cached = Arc::new(packet.clone());
        Ok((
            QuestionPreview {
                packet,
                selection,
                view,
            },
            cached,
            bytes,
        ))
    })
    .await?;
    // Only complete successful previews enter the cache. No mutex spans an await.
    s.packets.lock().unwrap().remember(cached, bytes);
    Ok(Json(result))
}
async fn cached_packet(s: Arc<DaemonState>, id: String) -> Result<Arc<QuestionPacket>, ApiError> {
    let packet = {
        let cache = s.packets.lock().unwrap();
        cache
            .packets
            .iter()
            .find(|(p, _)| p.packet_id == id)
            .map(|(p, _)| p.clone())
            .ok_or_else(missing)?
    };
    let revision = db(s, |store| Ok(store.status()?.revision)).await?;
    if revision != packet.revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "revision_conflict",
            "The index revision changed",
        ));
    }
    Ok(packet)
}
async fn question_export(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let packet = cached_packet(s, id).await?;
    Ok(Json(
        question_work(move || jev::request_for(&packet)).await?,
    ))
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
) -> Result<Json<QuestionSelection>, ApiError> {
    let packet = cached_packet(s, id).await?;
    Ok(Json(
        question_work(move || {
            let selection = jev::parse_response(&packet, &response)?;
            let warnings = jev::response_warnings(&response);
            let mut view = planning::assemble(&packet, &selection, "importedJev")?;
            view.warnings.extend(warnings);
            Ok(QuestionSelection { selection, view })
        })
        .await?,
    ))
}
async fn question_selection(
    State(s): State<Arc<DaemonState>>,
    Path(id): Path<String>,
    Json(selection): Json<SelectionEnvelope>,
) -> Result<Json<QuestionSelection>, ApiError> {
    let packet = cached_packet(s, id).await?;
    Ok(Json(
        question_work(move || {
            let view = planning::assemble(&packet, &selection, "manual")?;
            Ok(QuestionSelection { selection, view })
        })
        .await?,
    ))
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
) -> Result<Json<Value>, ApiError> {
    // Accept only an empty object: never take client prompts, sources or provider overrides.
    let request: Value = serde_json::from_slice(&body).map_err(|_| invalid())?;
    if !request.as_object().is_some_and(|object| object.is_empty()) {
        return Err(invalid());
    }
    let provider = s.acp.clone().ok_or(ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "acp_disabled",
        "Live ACP is disabled",
    ))?;
    let packet = cached_packet(s.clone(), id).await?;
    let response = provider.run(&packet).await;
    // A stale failure must not replace the state of a newer question either.
    let revision = db(s, |store| Ok(store.status()?.revision)).await?;
    if revision != packet.revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "revision_conflict",
            "The index revision changed",
        ));
    }
    let response = response.map_err(acp_error)?;
    Ok(Json(
        json!({"packetId":packet.packet_id,"revision":packet.revision,
        "source":"liveAcp","attemptId":response.attempt_id,"answer":response.answer,
        "latencyMs":response.latency_ms,"estimatedUsd":response.estimated_usd}),
    ))
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
) -> Result<Json<Value>, ApiError> {
    // No caller-supplied source, credentials, URL, or budget overrides.
    if !body.is_empty() && body.as_ref() != b"{}" {
        return Err(invalid());
    }
    let provider = s.provider.clone().ok_or(ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "jev_disabled",
        "Live Jev is disabled",
    ))?;
    let packet = cached_packet(s.clone(), id).await?;
    let response = provider.run(&packet).await;
    // Check even failed attempts. Reservations remain accounted on stale results.
    let revision = db(s.clone(), |store| Ok(store.status()?.revision)).await?;
    if revision != packet.revision {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "revision_conflict",
            "The index revision changed",
        ));
    }
    let response = response.map_err(live_jev_error)?;
    let selection = response.selection.clone();
    let mut view =
        question_work(move || planning::assemble(&packet, &selection, "liveJev")).await?;
    view.warnings.extend(response.warnings.clone());
    Ok(Json(json!({"selection":response.selection,"view":view,
        "attemptId":response.attempt_id,"latencyMs":response.latency_ms,
        "estimatedUsd":response.estimated_usd,"usage":response.usage,"warnings":response.warnings})))
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use tower::ServiceExt;

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
    async fn live_response_is_labeled_and_accounted() {
        mock_run(false, "success").await;
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
        let graph = index_workspace(&options, &cancel, |_| {}).unwrap();
        let seed = graph
            .nodes
            .iter()
            .find(|node| node.name == "seed")
            .unwrap()
            .id
            .clone();
        let store = Store::open(&dir.path().join("state"), &workspace).unwrap();
        store.publish(&graph, Some(0), &cancel).unwrap();
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
        let app = router(
            new_with_jev(
                store.clone(),
                options,
                token.into(),
                "127.0.0.1:7331".parse().unwrap(),
                Some(provider.clone()),
            )
            .unwrap(),
        );
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
                json!({"seed":seed,"question":"logging", "expectedRevision":1}),
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
        let run = tokio::spawn(app.oneshot(request(&path, json!({}))));
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        if change_snapshot {
            store.publish(&graph, Some(1), &cancel).unwrap();
        }
        release.notify_one();
        let response = tokio::time::timeout(std::time::Duration::from_secs(5), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if change_snapshot {
            assert_eq!(response.status(), StatusCode::CONFLICT);
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
            assert_eq!(body["view"]["revision"], 1);
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
mod dependency_lifecycle_tests {
    use super::*;
    fn catalog(id: &str, revision: u64) -> Catalog {
        Catalog {
            id: id.into(),
            workspace_revision: revision,
            packages: vec![],
            symbols: vec![],
            warnings: vec![],
            sources: Default::default(),
        }
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
        let store = Store::open(&temp.path().join("state"), temp.path()).unwrap();
        let state = new_with_dependency_options(
            store.clone(),
            IndexOptions::new(temp.path().into()),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            "127.0.0.1:7331".parse().unwrap(),
            None,
            None,
            temp.path().into(),
            vec![],
            Some(CatalogOptions::default()),
        )
        .unwrap();
        let active = AtomicBool::new(false);
        state.dependencies.lock().unwrap().generation = 2;
        state.publish_dependency_index(2, &active, Ok(catalog("new", 0)));
        state.publish_dependency_index(1, &active, Ok(catalog("old", 0)));
        state.publish_dependency_index(1, &active, Err(anyhow::anyhow!("late failure")));
        assert_eq!(state.catalog_snapshot(0).unwrap().id, "new");
        state.publish_dependency_index(2, &AtomicBool::new(true), Ok(catalog("cancelled", 0)));
        assert_eq!(state.catalog_snapshot(0).unwrap().id, "new");
        store
            .publish(
                &Graph::default(),
                Some(0),
                &Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        state.publish_dependency_index(2, &active, Ok(catalog("stale-revision", 0)));
        assert!(state.catalog_snapshot(0).is_none());
        assert_eq!(state.dependencies.lock().unwrap().state, "failed");
        state.cancel_active();
        let generation = state.dependencies.lock().unwrap().generation;
        state.publish_dependency_index(generation, &active, Ok(catalog("after-shutdown", 1)));
        assert!(state.catalog_snapshot(1).is_none());
    }
}
