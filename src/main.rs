use anyhow::{Context, Result, ensure};
use baleyg::{
    auth, http,
    indexer::IndexOptions,
    mcp,
    model::{CancelFlag, ViewQuery},
    store::{
        Store,
        topology::{TopologyRoots, WorkspaceIdentity},
    },
};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

// Finite retry budget between terminal selected-status attempts, not a synchronous
// SQLite-call wall-clock guarantee or a claim of p99 publication latency.
const STATUS_RESULT_WAIT: Duration = Duration::from_secs(15);

// Opt-in real-process test barrier. Normal CLI runs never open this socket.
#[cfg(unix)]
fn fixture_index_exchange(
    socket: &mut std::os::unix::net::UnixStream,
    sent: u8,
    expected: u8,
) -> Result<()> {
    use std::io::{Read, Write};
    socket
        .write_all(&[sent])
        .map_err(|_| anyhow::anyhow!("fixture index IPC send failed"))?;
    let mut reply = [0u8];
    socket
        .read_exact(&mut reply)
        .map_err(|_| anyhow::anyhow!("fixture index IPC response failed"))?;
    ensure!(reply[0] == expected, "fixture index IPC response mismatch");
    Ok(())
}

#[derive(Parser)]
#[command(
    name = "baleyg",
    version,
    about = "Local, read-only JavaScript, Rust, Java and Python code index and inspection daemon"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Build and atomically publish a complete index. Does not execute repository code.
    Index(IndexArgs),
    /// Serve one read-only MCP connection on line-framed stdin/stdout.
    Mcp(WorkspaceArgs),
    /// Serve the authenticated loopback API and browser inspector. Refresh is explicit.
    Serve(Box<ServeArgs>),
    /// Print the current published index status.
    Status(WorkspaceArgs),
    /// Search indexed symbol names and IDs (literal substring).
    Symbols(SymbolsArgs),
    /// Query a bounded static call view. This is not a runtime sequence.
    Query(QueryArgs),
    /// Export the normalized snapshot graph as JSON, including indexed source.
    Export(ExportArgs),
    /// Inspect existing derived indexes and saved records without changing them.
    Gc(GcArgs),
    /// Permanently remove one saved workspace record by its exact ID.
    Forget(ForgetArgs),
}
#[derive(Args)]
struct ForgetArgs {
    record_id: String,
    /// Confirm without an interactive terminal.
    #[arg(long)]
    yes: bool,
}
#[derive(Args)]
struct GcArgs {
    /// Print the read-only cleanup inventory.
    #[arg(long, required = true)]
    report: bool,
}
#[derive(Args, Clone)]
struct WorkspaceArgs {
    /// Existing workspace directory; defaults to the nearest Git checkout from cwd.
    #[arg(long)]
    workspace: Option<PathBuf>,
}
#[derive(Args, Clone)]
struct IndexArgs {
    #[command(flatten)]
    workspace: WorkspaceArgs,
    /// Existing binary SCIP artifact. No semantic indexer is executed.
    #[arg(long)]
    scip: Option<PathBuf>,
    /// Paired flat JSON map of relative input paths to SHA-256 hashes.
    #[arg(long, requires = "scip")]
    manifest: Option<PathBuf>,
    /// Larger files are skipped with a diagnostic; valid range 1..16777216.
    #[arg(long, default_value_t = 2_097_152)]
    max_file_bytes: u64,
}
#[derive(Args)]
struct ServeArgs {
    #[command(flatten)]
    index: IndexArgs,
    /// Loopback socket only. Port 0 selects an available port.
    #[arg(long, default_value = "127.0.0.1:8877")]
    bind: SocketAddr,
    /// Root of the read-only file tree. Defaults to the indexed workspace.
    #[arg(long)]
    browse_root: Option<PathBuf>,
    /// Optional external Rust source library LABEL=PATH (read-only candidates, not resolved targets).
    #[arg(long = "rust-source-root", value_parser = parse_rust_source_root)]
    rust_source_roots: Vec<(String, PathBuf)>,
    /// Rust library source directory; defaults to trusted RUST_SRC_PATH if set.
    #[arg(long)]
    rust_library: Option<PathBuf>,
    /// Trusted absolute rustc executable for startup-only sysroot discovery (never builds code).
    #[arg(long)]
    trusted_rustc: Option<PathBuf>,
    /// Cargo cache directory; defaults to CARGO_HOME or the user Cargo home.
    #[arg(long)]
    cargo_home: Option<PathBuf>,
    /// Existing private token file or a path at which to securely create one.
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// Explicit opt-in to source-bearing Jev requests; durable shared budget directory.
    #[arg(long, requires = "jev_budget_cents")]
    jev_budget_dir: Option<PathBuf>,
    /// Authorized reservation cap in cents. Existing ledger caps cannot be changed.
    #[arg(long, requires = "jev_budget_dir", value_parser = clap::value_parser!(u64).range(10..=500))]
    jev_budget_cents: Option<u64>,
    /// Explicit opt-in: trusted executable ACP runner (not a command from the inspected repository).
    #[arg(long, requires_all = ["acp_state_dir", "acp_max_attempts"])]
    acp_runner: Option<PathBuf>,
    /// Separate private ACP allowance/audit directory. Never use a Jev budget directory.
    #[arg(long, requires_all = ["acp_runner", "acp_max_attempts"])]
    acp_state_dir: Option<PathBuf>,
    /// Authorized ACP attempt allowance; immutable across restarts. Not a monetary cap.
    #[arg(long, requires_all = ["acp_runner", "acp_state_dir"], value_parser = clap::value_parser!(u64).range(1..=20))]
    acp_max_attempts: Option<u64>,
}
#[derive(Args)]
struct SymbolsArgs {
    #[command(flatten)]
    workspace: WorkspaceArgs,
    #[arg(long, default_value = "")]
    search: String,
    #[arg(long, default_value_t = 50)]
    limit: usize,
}
#[derive(Args)]
struct QueryArgs {
    #[command(flatten)]
    workspace: WorkspaceArgs,
    #[arg(long)]
    seed: String,
    #[arg(long, default_value_t = 1)]
    depth: usize,
    #[arg(long, default_value_t = 40)]
    max_nodes: usize,
    #[arg(long, default_value_t = 200)]
    max_calls: usize,
    /// Navigate callback definitions explicitly; never turns references into calls.
    #[arg(long)]
    include_callbacks: bool,
    #[arg(long)]
    exclude_path: Vec<String>,
}
#[derive(Args)]
struct ExportArgs {
    #[command(flatten)]
    workspace: WorkspaceArgs,
    /// Without this option, write JSON to stdout. Never overwrites an existing file.
    #[arg(long)]
    output: Option<PathBuf>,
}
impl WorkspaceArgs {
    fn resolve_unattached(&self) -> Result<(TopologyRoots, WorkspaceIdentity)> {
        let roots = TopologyRoots::production()?;
        let cwd = std::env::current_dir()?;
        let identity = WorkspaceIdentity::discover_unattached(self.workspace.as_deref(), &cwd)?;
        roots.reject_root_overlap(&identity)?;
        Ok((roots, identity))
    }
    fn resolve(&self) -> Result<(TopologyRoots, WorkspaceIdentity)> {
        let (roots, identity) = self.resolve_unattached()?;
        Ok((roots, identity.attach_marker()?))
    }
    fn store(&self) -> Result<Store> {
        let (roots, identity) = self.resolve()?;
        Store::open(roots, identity)
    }
}
impl IndexArgs {
    fn resolve(&self) -> Result<(Store, IndexOptions, PathBuf)> {
        ensure!(
            (1..=16_777_216).contains(&self.max_file_bytes),
            "max-file-bytes must be 1..16777216"
        );
        let (roots, identity) = self.workspace.resolve()?;
        let root = identity.root.clone();
        let cache = roots.cache.clone();
        let store = Store::open(roots, identity)?;
        let mut options = IndexOptions::new(root);
        options.scip_path = self.scip.clone();
        options.manifest_path = self.manifest.clone();
        options.anchor_optional_inputs(&std::env::current_dir()?)?;
        options.max_file_bytes = self.max_file_bytes;
        Ok((store, options, cache))
    }
}
fn print_json(value: &impl Serialize) -> Result<()> {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

// Identity/incarnation loss outranks even direct selected SQLite BUSY.
fn terminal_status_retryable_after_guard(
    error: &anyhow::Error,
    verify: impl FnOnce() -> Result<()>,
) -> Result<bool> {
    verify()?;
    Ok(baleyg::store::terminal_status_sqlite_contention(error))
}

fn write_session_json(
    value: &impl Serialize,
    session: &Arc<baleyg::store::topology::LeaderSession>,
    mut writer: impl std::io::Write,
) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    session.verify()?;
    writer.write_all(&bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod session_output_tests {
    use super::*;
    #[test]
    fn terminal_busy_cannot_override_post_read_root_guard_loss() {
        let busy: anyhow::Error = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        )
        .into();
        let lost = terminal_status_retryable_after_guard(&busy, || {
            anyhow::bail!("root_changed: original selected root replaced")
        })
        .unwrap_err();
        assert_eq!(
            lost.to_string(),
            "root_changed: original selected root replaced"
        );
        assert!(terminal_status_retryable_after_guard(&busy, || Ok(())).unwrap());
    }

    use baleyg::store::topology::{LeaderSession, TopologyRoots, WorkspaceIdentity};
    use std::io::Write;

    struct ProbeWriter {
        roots: TopologyRoots,
        workspace: PathBuf,
        bytes: Vec<u8>,
        probed: bool,
    }
    impl Write for ProbeWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if !self.probed {
                self.probed = true;
                let identity = WorkspaceIdentity::discover(Some(&self.workspace), &self.workspace)
                    .map_err(std::io::Error::other)?;
                let error = self.roots.leader(&identity).unwrap_err();
                assert!(error.to_string().contains("storage_busy"), "{error:#}");
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn index_output_holds_leader_until_write_finishes() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let roots =
            TopologyRoots::isolated_for_tests(temp.path().join("cache"), temp.path().join("data"));
        let identity = Arc::new(WorkspaceIdentity::discover(Some(&workspace), &workspace).unwrap());
        let session = Arc::new(LeaderSession::leader(
            roots.leader(&identity).unwrap(),
            identity,
        ));
        let mut writer = ProbeWriter {
            roots: roots.clone(),
            workspace: workspace.clone(),
            bytes: vec![],
            probed: false,
        };
        write_session_json(&serde_json::json!({"ok":true}), &session, &mut writer).unwrap();
        assert!(writer.probed);
        drop(session);
        let identity = WorkspaceIdentity::discover(Some(&workspace), &workspace).unwrap();
        roots.leader(&identity).unwrap();
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "baleyg=info".into()),
        )
        .init();
    match Cli::parse().command {
        Command::Mcp(args) => {
            let (_, identity) = args.resolve()?;
            mcp::run_stdio(mcp::OpenedWorkspace::new(identity))?;
        }
        Command::Gc(_) => print_json(&TopologyRoots::production()?.gc_report()?)?,
        Command::Forget(args) => {
            use std::io::{self, Write};
            let deleted = TopologyRoots::production()?.forget_with_confirmation(
                &args.record_id,
                |record, paths| {
                    eprintln!("Record ID: {}", record.id);
                    eprintln!(
                        "Saved views: {}; annotations: {}",
                        record.views, record.annotations
                    );
                    for path in paths {
                        eprintln!("Saved path: {path}");
                    }
                    eprintln!("WARNING: every checkout sharing this UUID loses these saved items.");
                    if args.yes {
                        return Ok(true);
                    }
                    ensure!(
                        unsafe { libc::isatty(libc::STDIN_FILENO) } == 1,
                        "confirmation requires a terminal or --yes"
                    );
                    eprint!("Type the exact record ID to forget: ");
                    io::stderr().flush()?;
                    let mut answer = String::new();
                    io::stdin().read_line(&mut answer)?;
                    Ok(answer.trim_end_matches(['\r', '\n']) == record.id)
                },
            )?;
            ensure!(
                deleted,
                "record ID confirmation did not match; no files removed"
            );
            eprintln!("Forgot record {}", args.record_id);
        }
        Command::Index(args) => {
            let diagnostics = std::env::var("BALEYG_INDEX_DIAGNOSTICS").as_deref() == Ok("1");
            let command_start = Instant::now();
            let (store, options, _) = args.resolve()?;
            // Fixture-only owner barrier: unset for every normal CLI invocation.
            #[cfg(unix)]
            let fixture_socket = std::env::var_os("BALEYG_TEST_FINITE_CLI_FD")
                .map(|fd| -> Result<_> {
                    use std::os::fd::FromRawFd;
                    use std::os::unix::net::UnixStream;
                    use std::time::Duration;
                    ensure!(
                        fd.to_str() == Some("3"),
                        "fixture index IPC descriptor invalid"
                    );
                    // Only the opt-in test process passes this Unix socket via dup2.
                    let socket = unsafe { UnixStream::from_raw_fd(3) };
                    socket.set_read_timeout(Some(Duration::from_secs(20)))?;
                    socket.set_write_timeout(Some(Duration::from_secs(20)))?;
                    Ok(Arc::new(std::sync::Mutex::new(socket)))
                })
                .transpose()?;
            #[cfg(not(unix))]
            ensure!(
                std::env::var_os("BALEYG_TEST_FINITE_CLI_FD").is_none(),
                "fixture index IPC requires Unix"
            );
            let fixture_seen = Arc::new(AtomicBool::new(false));
            let fixture_failed = Arc::new(AtomicBool::new(false));
            if diagnostics {
                eprintln!(
                    "index-phase outside_setup_ms={:.3}",
                    command_start.elapsed().as_secs_f64() * 1e3
                );
            }
            let worker_store = store.clone();
            let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
            let flag = cancel.clone();
            let signal = tokio::spawn(async move {
                shutdown_signal().await;
                flag.store(true, Ordering::Release);
            });
            let queue_start = Instant::now();
            let fixture_seen_worker = fixture_seen.clone();
            let fixture_failed_worker = fixture_failed.clone();
            let fixture_cancel = cancel.clone();
            #[cfg(unix)]
            let fixture_socket_worker = fixture_socket.clone();
            let work = tokio::task::spawn_blocking(move || {
                baleyg::index_coordinator::enqueue_and_wait_observed_with_request(
                    &worker_store,
                    &options,
                    &cancel,
                    |phase| {
                        if diagnostics {
                            if let Some(name) = phase.phase.strip_prefix("timing:") {
                                eprintln!(
                                    "index-phase {name}_ms={:.3}",
                                    phase.completed as f64 / 1e3
                                );
                            } else if let Some(mode) = phase.phase.strip_prefix("mode:") {
                                eprintln!("index-mode {mode}");
                            }
                        }
                        #[cfg(unix)]
                        if phase.phase == "timing:publish"
                            && let Some(socket) = fixture_socket_worker.as_ref()
                            && !fixture_seen_worker.swap(true, Ordering::AcqRel)
                            && fixture_index_exchange(&mut socket.lock().unwrap(), b'P', b'G')
                                .is_err()
                        {
                            fixture_failed_worker.store(true, Ordering::Release);
                            fixture_cancel.store(true, Ordering::Release);
                        }
                    },
                )
            })
            .await
            .context("index worker panicked")?;
            if diagnostics {
                eprintln!(
                    "index-phase queue_and_jobs_ms={:.3}",
                    queue_start.elapsed().as_secs_f64() * 1e3
                );
            }
            signal.abort();
            ensure!(
                !fixture_failed.load(Ordering::Acquire),
                "fixture index IPC failed before FIFO claim"
            );
            let (own_request_id, revision, session) = work?;
            #[cfg(unix)]
            if let Some(socket) = fixture_socket {
                ensure!(
                    fixture_seen.load(Ordering::Acquire),
                    "fixture index IPC missed first publication"
                );
                session.verify()?;
                ensure!(session.is_leader(), "fixture owner lost leadership");
                let mut socket = socket.lock().unwrap();
                socket.set_read_timeout(Some(std::time::Duration::from_secs(45)))?;
                fixture_index_exchange(&mut socket, b'R', b'D')?;
                session.verify()?;
            }
            if diagnostics {
                eprintln!("index-phase coordinator_returned");
            }
            if diagnostics && let Some(diagnostic) = store.last_writer_diagnostic() {
                eprintln!("{diagnostic}");
            }
            let status_start = Instant::now();
            let deadline = status_start + STATUS_RESULT_WAIT;
            let mut backoff = Duration::from_millis(20);
            let status = loop {
                session.verify()?;
                if diagnostics {
                    eprintln!("index-phase terminal_status_attempt");
                }
                // A single selected snapshot binds our accepted pin to status.
                let selected = (|| -> Result<_> {
                    let response = store.evidence_response()?;
                    let result = response
                        .validate_pin(revision)
                        .and_then(|_| response.status());
                    match result {
                        Ok(status) => response.finish(status),
                        Err(error) => {
                            // Even on failed selected reads, root and follower-marker
                            // fences outrank SQLite contention and expiry.
                            response.finish(())?;
                            Err(error)
                        }
                    }
                })();
                match selected {
                    Ok(status) => break status,
                    Err(error) => {
                        if !terminal_status_retryable_after_guard(&error, || session.verify())? {
                            return Err(error);
                        }
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            anyhow::bail!(
                                "storage_busy: request {} is durable and may already be complete; check job status",
                                own_request_id
                            );
                        }
                        tokio::time::sleep(backoff.min(remaining)).await;
                        backoff = backoff.saturating_mul(2).min(Duration::from_millis(250));
                    }
                }
            };
            let output = serde_json::json!({"publishedRevision":revision,"status":status});
            write_session_json(&output, &session, std::io::stdout().lock())?;
            if diagnostics {
                eprintln!(
                    "index-phase outside_status_output_ms={:.3} total_ms={:.3}",
                    status_start.elapsed().as_secs_f64() * 1e3,
                    command_start.elapsed().as_secs_f64() * 1e3
                );
            }
        }
        Command::Serve(args) => {
            ensure!(
                args.bind.ip().is_loopback(),
                "only loopback bind addresses are supported"
            );
            let token_path = args
                .token_file
                .context("serve requires explicit --token-file")?;
            let (roots, identity) = args.index.workspace.resolve_unattached()?;
            let mut destinations = vec![token_path.clone()];
            if let Some(path) = &args.jev_budget_dir {
                destinations.push(path.clone());
            }
            if let Some(path) = &args.acp_state_dir {
                destinations.push(path.clone());
            }
            roots.validate_external(&identity, &destinations)?;
            let identity = identity.attach_marker()?;
            ensure!(
                (1..=16_777_216).contains(&args.index.max_file_bytes),
                "max-file-bytes must be 1..16777216"
            );
            let mut options = IndexOptions::new(identity.root.clone());
            options.scip_path = args.index.scip.clone();
            options.manifest_path = args.index.manifest.clone();
            options.anchor_optional_inputs(&std::env::current_dir()?)?;
            options.max_file_bytes = args.index.max_file_bytes;
            let dir = roots.cache.clone();
            let store = Store::open(roots, identity)?;
            let startup_cancel: CancelFlag = Arc::new(AtomicBool::new(false));
            let serving_session = match baleyg::index_coordinator::establish_serving_session(
                &store,
                Some(&options),
                &startup_cancel,
            ) {
                Ok(session) => Some(session),
                Err(error) => {
                    eprintln!("Evidence unavailable at startup: {error:#}");
                    None
                }
            };
            let token = auth::load_or_create_token(&token_path)?;
            let listener = tokio::net::TcpListener::bind(args.bind)
                .await
                .context("bind daemon listener")?;
            let address = listener.local_addr()?;
            let provider = if let Some(budget_dir) = args.jev_budget_dir {
                let key = std::env::var("JEV_KEY").map_err(|_| {
                    anyhow::anyhow!("JEV_KEY must be configured to enable live Jev")
                })?;
                Some(Arc::new(baleyg::live_jev::LiveJev::open(
                    &budget_dir,
                    key,
                    args.jev_budget_cents
                        .context("Jev budget cap is required")?,
                    &options.workspace_root,
                )?))
            } else {
                None
            };
            let acp = if let Some(runner) = args.acp_runner {
                Some(Arc::new(baleyg::acp::Acp::open(baleyg::acp::AcpConfig {
                    runner,
                    state_dir: args
                        .acp_state_dir
                        .context("ACP state directory is required")?,
                    max_attempts: args.acp_max_attempts.context("ACP allowance is required")?,
                    workspace: options.workspace_root.clone(),
                })?))
            } else {
                None
            };
            let inference_notice = match (provider.is_some(), acp.is_some()) {
                (false, false) => "Live inference disabled. Offline preview/export/import only.",
                (true, false) => {
                    "Live Jev enabled with durable reservations; explicit Run Jev requests send indexed source."
                }
                (false, true) => {
                    "ACP enabled with a separate attempt allowance; explicit Explain with ACP requests send indexed source to the existing Claude subscription."
                }
                (true, true) => {
                    "Live Jev and ACP enabled with separate allowances. Explicit inference requests send indexed source; no automatic model calls."
                }
            };
            let browse_root = resolve_browse_root(&options, args.browse_root);
            let cargo_home = args
                .cargo_home
                .or_else(|| std::env::var_os("CARGO_HOME").map(PathBuf::from))
                .or_else(|| directories::BaseDirs::new().map(|d| d.home_dir().join(".cargo")));
            let mut rust_library = args
                .rust_library
                .or_else(|| std::env::var_os("RUST_SRC_PATH").map(PathBuf::from));
            if rust_library.is_none()
                && let Some(compiler) = args.trusted_rustc.as_ref()
            {
                rust_library =
                    Some(discover_rust_library(compiler, &options.workspace_root).await?);
            }
            let state = http::new_with_dependency_options(
                store,
                options,
                token,
                address,
                provider,
                acp,
                browse_root,
                args.rust_source_roots,
                Some(baleyg::dependencies::CatalogOptions {
                    cargo_home,
                    rust_library,
                }),
            )?;
            if let Some(session) = serving_session {
                state.retain_serving_session(session);
            }
            state.start_dependency_index();
            eprintln!(
                "Baleyg: http://{address}/\nState: {}\nToken file: {}\nRefresh is explicit. No repository commands run.\n{inference_notice}",
                dir.display(),
                token_path.display()
            );
            let shutdown_state = state.clone();
            axum::serve(listener, http::router(state))
                .with_graceful_shutdown(async move {
                    shutdown_signal().await;
                    shutdown_state.cancel_active();
                })
                .await
                .context("serve daemon")?;
        }
        Command::Status(args) => {
            let (roots, identity) = args.resolve_unattached()?;
            let status = baleyg::store::Store::status_existing_readonly(roots, identity)?;
            use std::io::Write;
            let mut output = std::io::stdout().lock();
            serde_json::to_writer_pretty(&mut output, &status)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
        Command::Symbols(args) => {
            ensure!((1..=150).contains(&args.limit), "limit must be 1..150");
            let store = args.workspace.store()?;
            let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
            let session =
                baleyg::index_coordinator::establish_serving_session(&store, None, &cancel)?;
            let (revision, items) = store.symbols_at(&args.search, args.limit)?;
            write_session_json(
                &serde_json::json!({"revision":revision,"items":items}),
                &session,
                std::io::stdout().lock(),
            )?;
        }
        Command::Query(args) => {
            let query = ViewQuery {
                seed: args.seed,
                depth: args.depth,
                max_nodes: args.max_nodes,
                max_calls: args.max_calls,
                include_callbacks: args.include_callbacks,
                exclude_paths: args.exclude_path,
            };
            query.validate()?;
            let store = args.workspace.store()?;
            let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
            let session =
                baleyg::index_coordinator::establish_serving_session(&store, None, &cancel)?;
            let view = store
                .query_view(&query)?
                .context("seed not found in current index")?;
            write_session_json(&view, &session, std::io::stdout().lock())?;
        }
        Command::Export(args) => {
            let (roots, identity) = args.workspace.resolve_unattached()?;
            if let Some(path) = args.output.as_ref() {
                roots.validate_external(&identity, std::slice::from_ref(path))?;
            }
            let store = Store::open(roots, identity.attach_marker()?)?;
            let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
            let session =
                baleyg::index_coordinator::establish_serving_session(&store, None, &cancel)?;
            let graph = store.graph()?;
            if let Some(path) = args.output {
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options
                    .open(path)
                    .context("create export (existing files are never overwritten)")?;
                write_session_json(&graph, &session, &mut file)?;
            } else {
                write_session_json(&graph, &session, std::io::stdout().lock())?;
            }
        }
    }
    Ok(())
}

fn resolve_browse_root(options: &IndexOptions, explicit: Option<PathBuf>) -> PathBuf {
    explicit.unwrap_or_else(|| options.workspace_root.clone())
}

#[cfg(test)]
mod workspace_defaults {
    use super::*;

    #[test]
    fn serve_defaults_to_cwd_and_browses_the_index_workspace() {
        let Cli {
            command: Command::Serve(args),
        } = Cli::try_parse_from(["baleyg", "serve"]).unwrap()
        else {
            panic!("expected serve");
        };
        assert_eq!(args.index.workspace.workspace, None);
        let options = IndexOptions::new(PathBuf::from("/chosen/project"));
        assert_eq!(
            resolve_browse_root(&options, args.browse_root),
            options.workspace_root
        );
        assert_eq!(
            resolve_browse_root(&options, Some(PathBuf::from("/explicit/tree"))),
            PathBuf::from("/explicit/tree")
        );
    }
}

fn parse_rust_source_root(value: &str) -> Result<(String, PathBuf), String> {
    let (label, path) = value.split_once('=').ok_or("expected LABEL=PATH")?;
    if label.is_empty()
        || label.len() > 48
        || !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        || path.is_empty()
    {
        return Err("expected a short alphanumeric LABEL and a nonempty PATH".into());
    }
    Ok((label.into(), PathBuf::from(path)))
}

#[cfg(test)]
mod rust_source_arguments {
    use super::*;
    #[test]
    fn roots_are_explicit_named_paths_not_commands() {
        assert_eq!(
            parse_rust_source_root("std=/a/b=c").unwrap(),
            ("std".into(), PathBuf::from("/a/b=c"))
        );
        for value in [
            "/a/b",
            "=path",
            "std=",
            "../escape=/tmp",
            "label with spaces=/tmp",
        ] {
            assert!(parse_rust_source_root(value).is_err());
        }
        let Cli {
            command: Command::Serve(args),
        } = Cli::try_parse_from(["baleyg", "serve"]).unwrap()
        else {
            panic!("serve")
        };
        assert!(args.rust_source_roots.is_empty());
    }
}

async fn discover_rust_library(
    compiler: &std::path::Path,
    workspace: &std::path::Path,
) -> Result<PathBuf> {
    use tokio::io::AsyncReadExt;
    ensure!(
        compiler.is_absolute(),
        "trusted rustc must be an absolute executable path"
    );
    let compiler = compiler.canonicalize().context("resolve trusted rustc")?;
    ensure!(
        !compiler.starts_with(workspace.canonicalize()?),
        "trusted rustc must be outside the inspected workspace"
    );
    let neutral = std::env::temp_dir()
        .canonicalize()
        .context("resolve neutral toolchain discovery directory")?;
    ensure!(
        !neutral.starts_with(workspace.canonicalize()?),
        "toolchain discovery directory must be outside the workspace"
    );
    let mut child = tokio::process::Command::new(&compiler)
        .args(["--print", "sysroot"])
        .env_clear()
        .current_dir(neutral)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("start trusted rustc metadata probe")?;
    let stdout = child
        .stdout
        .take()
        .context("capture trusted rustc metadata")?;
    let mut bytes = Vec::new();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        stdout.take(4097).read_to_end(&mut bytes).await?;
        ensure!(bytes.len() <= 4096, "toolchain metadata exceeds limit");
        ensure!(
            child.wait().await?.success(),
            "trusted rustc metadata probe failed"
        );
        Ok::<_, anyhow::Error>(())
    })
    .await;
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            let _ = child.kill().await;
            return Err(error);
        }
        Err(_) => {
            let _ = child.kill().await;
            anyhow::bail!("trusted rustc metadata probe timed out");
        }
    }
    let sysroot = std::str::from_utf8(&bytes)
        .context("toolchain path is not UTF8")?
        .trim();
    ensure!(
        !sysroot.is_empty() && std::path::Path::new(sysroot).is_absolute(),
        "invalid toolchain sysroot path"
    );
    let library = PathBuf::from(sysroot).join("lib/rustlib/src/rust/library");
    ensure!(
        library.is_dir(),
        "Rust standard-library sources are missing from the trusted toolchain"
    );
    Ok(library)
}

#[cfg(all(test, unix))]
mod trusted_toolchain_probe_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[tokio::test]
    async fn rejects_workspace_compiler_without_execution() {
        let temp = tempfile::tempdir().unwrap();
        let compiler = temp.path().join("rustc");
        let marker = temp.path().join("executed");
        std::fs::write(&compiler, format!("#!/bin/sh\ntouch {:?}\n", marker)).unwrap();
        std::fs::set_permissions(&compiler, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(discover_rust_library(&compiler, temp.path()).await.is_err());
        assert!(!marker.exists());
    }
    #[tokio::test]
    async fn trusted_probe_uses_fixed_metadata_arguments() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let sysroot = temp.path().join("toolchain");
        std::fs::create_dir_all(sysroot.join("lib/rustlib/src/rust/library")).unwrap();
        let compiler = temp.path().join("trusted-rustc");
        std::fs::write(&compiler,format!("#!/bin/sh\n[ \"$#\" = 2 ] || exit 1\n[ \"$1\" = --print ] || exit 1\n[ \"$2\" = sysroot ] || exit 1\n[ -z \"${{CARGO_MANIFEST_DIR+x}}\" ] || exit 1\nprintf '%s\\n' '{}'\n",sysroot.display())).unwrap();
        std::fs::set_permissions(&compiler, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            discover_rust_library(&compiler, &workspace).await.unwrap(),
            sysroot.join("lib/rustlib/src/rust/library")
        );
    }
}
