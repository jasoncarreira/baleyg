use anyhow::{Context, Result, ensure};
use baleyg::{
    auth, http,
    indexer::{IndexOptions, index_workspace},
    model::{CancelFlag, ViewQuery},
    store::Store,
};
use clap::{Args, Parser, Subcommand};
use directories::ProjectDirs;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

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
}
#[derive(Args, Clone)]
struct WorkspaceArgs {
    /// Repository to inspect. Defaults to cwd; state defaults outside this directory.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    /// Override the per-workspace application data directory.
    #[arg(long)]
    state_dir: Option<PathBuf>,
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
    fn resolve(&self) -> Result<(PathBuf, PathBuf)> {
        let root = self.workspace.canonicalize().context("resolve workspace")?;
        ensure!(root.is_dir(), "workspace must be a directory");
        let name = root.to_str().context("workspace path must be UTF-8")?;
        let dir = match &self.state_dir {
            Some(p) => p.clone(),
            None => {
                let dirs = ProjectDirs::from("dev", "odin", "baleyg")
                    .context("cannot determine application data directory; use --state-dir")?;
                let key = hex::encode(Sha256::digest(name.as_bytes()));
                dirs.data_local_dir().join("workspaces").join(key)
            }
        };
        Ok((root, dir))
    }
    fn store(&self) -> Result<Store> {
        let (root, dir) = self.resolve()?;
        Store::open(&dir, &root)
    }
}
impl IndexArgs {
    fn resolve(&self) -> Result<(Store, IndexOptions, PathBuf)> {
        ensure!(
            (1..=16_777_216).contains(&self.max_file_bytes),
            "max-file-bytes must be 1..16777216"
        );
        let (root, dir) = self.workspace.resolve()?;
        let store = Store::open(&dir, &root)?;
        let mut options = IndexOptions::new(root);
        options.scip_path = self.scip.clone();
        options.manifest_path = self.manifest.clone();
        options.max_file_bytes = self.max_file_bytes;
        Ok((store, options, dir))
    }
}
fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
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
        Command::Index(args) => {
            let (store, options, _) = args.resolve()?;
            let expected = store.status()?.revision;
            let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
            let flag = cancel.clone();
            let signal = tokio::spawn(async move {
                shutdown_signal().await;
                flag.store(true, Ordering::Release);
            });
            let writer = store.clone();
            let work = tokio::task::spawn_blocking(move || -> Result<u64> {
                let graph = index_workspace(&options, &cancel, |p| {
                    if p.completed == p.total {
                        eprintln!("{}: {}/{}", p.phase, p.completed, p.total);
                    }
                })?;
                writer.publish(&graph, Some(expected), &cancel)
            })
            .await
            .context("index worker panicked")?;
            signal.abort();
            let revision = work?;
            print_json(
                &serde_json::json!({"publishedRevision":revision,"status":store.status()?}),
            )?;
        }
        Command::Serve(args) => {
            ensure!(
                args.bind.ip().is_loopback(),
                "only loopback bind addresses are supported"
            );
            let (store, options, dir) = args.index.resolve()?;
            let token_path = args.token_file.unwrap_or_else(|| dir.join("daemon.token"));
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
                    &args.index.workspace.workspace.canonicalize()?,
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
                    workspace: args.index.workspace.workspace.canonicalize()?,
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
        Command::Status(args) => print_json(&args.store()?.status()?)?,
        Command::Symbols(args) => {
            ensure!((1..=150).contains(&args.limit), "limit must be 1..150");
            let (revision, items) = args
                .workspace
                .store()?
                .symbols_at(&args.search, args.limit)?;
            print_json(&serde_json::json!({"revision":revision,"items":items}))?;
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
            let view = args
                .workspace
                .store()?
                .query_view(&query)?
                .context("seed not found in current index")?;
            print_json(&view)?;
        }
        Command::Export(args) => {
            let graph = args.workspace.store()?.graph()?;
            if let Some(path) = args.output {
                use std::io::Write;
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
                serde_json::to_writer_pretty(&mut file, &graph)?;
                file.write_all(b"\n")?;
            } else {
                print_json(&graph)?;
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
        assert_eq!(args.index.workspace.workspace, PathBuf::from("."));
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
