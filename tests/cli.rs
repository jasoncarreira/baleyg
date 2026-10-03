use serde_json::Value;
use std::{fs, process::Command};
use tempfile::TempDir;

/// Only fixed startup markers are retained. Never copy arbitrary daemon stderr,
/// filesystem paths, source text or the bearer into a failure artifact.
fn readiness_stderr_category(sample: &[u8]) -> &'static str {
    let text = String::from_utf8_lossy(sample);
    if text.contains("bind daemon listener") {
        "listener_bind_error"
    } else if text.contains("Error:") {
        "other_startup_error"
    } else if text.contains("Evidence unavailable at startup:") {
        "index_startup_unavailable"
    } else if text.contains("Baleyg: http://") {
        "server_banner_present"
    } else {
        "no_allowlisted_marker"
    }
}

/// Opt-in failure-only report, outside the tracked worktree. Every value written
/// here is a fixed category or bounded number, not untrusted process output.
fn write_private_readiness_diagnostic(
    dir: &std::path::Path,
    child: &mut std::process::Child,
    stderr_path: &std::path::Path,
    address: std::net::SocketAddr,
    healthz: &str,
) -> std::io::Result<()> {
    use std::io::{Read, Write};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let meta = fs::symlink_metadata(dir)?;
    if !dir.is_absolute()
        || !meta.is_dir()
        || meta.file_type().is_symlink()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o777 != 0o700
        || dir
            .canonicalize()?
            .starts_with(std::env::current_dir()?.canonicalize()?)
    {
        return Err(std::io::Error::other(
            "unsafe readiness diagnostic directory",
        ));
    }
    let child_state = match child.try_wait()? {
        Some(status) => match status.code() {
            Some(code) => format!("exited_code_{code}"),
            None => {
                use std::os::unix::process::ExitStatusExt;
                format!("exited_signal_{}", status.signal().unwrap_or_default())
            }
        },
        None => "alive_at_deadline".to_owned(),
    };
    let listener =
        match std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(150))
        {
            Ok(_) => "tcp_accepts",
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => "tcp_refused",
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => "tcp_timeout",
            Err(_) => "tcp_other_error",
        };
    let mut sample = Vec::new();
    fs::File::open(stderr_path)?
        .take(16_384)
        .read_to_end(&mut sample)?;
    let category = readiness_stderr_category(&sample);
    let output = dir.join(format!(
        "queue-api-readiness-{}-{}.log",
        std::process::id(),
        child.id()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(output)?;
    // The caller supplies `healthz` from a closed set of literals or HTTP status
    // numbers. Do not accept arbitrary reqwest error text here.
    writeln!(
        file,
        "child={child_state} healthz={healthz} listener={listener} stderr_category={category}"
    )?;
    file.sync_all()?;
    Ok(())
}

#[test]
fn readiness_stderr_category_never_copies_secret_or_temp_path() {
    let poisoned = b"Error: bind daemon listener /private/tmp/secret Bearer 0123456789abcdef\n";
    let category = readiness_stderr_category(poisoned);
    assert_eq!(category, "listener_bind_error");
    assert!(!category.contains("Bearer"));
    assert!(!category.contains("/private"));
    assert!(!category.contains("0123456789abcdef"));
    let mixed = b"Baleyg: http://127.0.0.1:7331/\nEvidence unavailable at startup: /private/tmp/secret\nError: failed after banner; Bearer 0123456789abcdef\n";
    let fatal = readiness_stderr_category(mixed);
    assert_eq!(fatal, "other_startup_error");
    assert!(!fatal.contains("Bearer"));
    assert!(!fatal.contains("/private"));
    assert!(!fatal.contains("0123456789abcdef"));
}

#[test]
fn private_readiness_report_is_mode_600_bounded_and_never_copies_raw_stderr() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("private");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let stderr_path = tmp.path().join("daemon-stderr");
    let secret = "Bearer 0123456789abcdef /private/tmp/user-workspace source-data";
    fs::write(
        &stderr_path,
        format!("Error: bind daemon listener {secret}\n"),
    )
    .unwrap();
    let mut child = Command::new("true").spawn().unwrap();
    write_private_readiness_diagnostic(
        &dir,
        &mut child,
        &stderr_path,
        "127.0.0.1:9".parse().unwrap(),
        "connect_error",
    )
    .unwrap();
    let _ = child.wait();
    let path = fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
    let report = fs::read_to_string(&path).unwrap();
    assert!(report.len() < 256);
    assert!(report.contains("stderr_category=listener_bind_error"));
    assert!(!report.contains(secret));
    assert!(!report.contains("Bearer"));
    assert!(!report.contains("/private/tmp"));
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

fn isolated_command(home: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_baleyg"));
    // ProjectDirs uses inherited XDG roots before HOME on Linux.
    cmd.env("HOME", home)
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_DATA_HOME");
    cmd
}

fn command(root: &std::path::Path, state: &std::path::Path, sub: &str) -> Command {
    let mut c = isolated_command(state);
    c.arg(sub).arg("--workspace").arg(root);
    if sub == "serve" {
        c.arg("--token-file").arg(state.join("token"));
    }
    c
}

fn write_optional_presentation(dir: &std::path::Path, label: &str, source_hash: &str) {
    use protobuf::Message;
    let mut index = scip::types::Index::new();
    let mut document = scip::types::Document::new();
    document.relative_path = "main.js".into();
    let mut occurrence = scip::types::Occurrence::new();
    occurrence.range = vec![0, 9, 10];
    occurrence.symbol_roles = 1;
    occurrence.symbol = label.into();
    document.occurrences.push(occurrence);
    index.documents.push(document);
    fs::write(dir.join("index.scip"), index.write_to_bytes().unwrap()).unwrap();
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec(&serde_json::json!({"main.js":source_hash})).unwrap(),
    )
    .unwrap();
}

#[test]
fn cli_helper_uses_isolated_home_instead_of_inherited_xdg_roots() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace).unwrap();

    let mut cmd = command(&workspace, &home, "status");
    let overrides = cmd.get_envs().collect::<Vec<_>>();
    for key in ["XDG_CACHE_HOME", "XDG_DATA_HOME"] {
        assert!(
            overrides
                .iter()
                .any(|(name, value)| *name == key && value.is_none()),
            "{key} must be removed from the child environment"
        );
    }
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["stats"]["files"], 0);
    let cache = home.join(if cfg!(target_os = "macos") {
        "Library/Caches/dev.odin.baleyg"
    } else {
        ".cache/baleyg"
    });
    assert!(cache.exists());
}

#[test]
fn standalone_takeover_reuses_recorded_nondefault_file_cap() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    let mut boundary = vec![b' '; 2_621_440];
    boundary.extend_from_slice(
        b"
function boundary() {}
",
    );
    fs::write(root.join("boundary.js"), boundary).unwrap();

    let indexed = command(&root, &home, "index")
        .arg("--max-file-bytes")
        .arg("3145728")
        .output()
        .unwrap();
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    let indexed: Value = serde_json::from_slice(&indexed.stdout).unwrap();
    assert_eq!(indexed["status"]["stats"]["files"], 1);
    let indexes = if cfg!(target_os = "macos") {
        home.join("Library/Caches/dev.odin.baleyg/indexes")
    } else {
        home.join(".cache/baleyg/indexes")
    };
    let db_path = fs::read_dir(indexes)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
        .join("index.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let options: String = db
        .query_row(
            "SELECT reconcile_options FROM index_metadata WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let options: Value = serde_json::from_str(&options).unwrap();
    assert_eq!(options["maxFileBytes"], 3_145_728);
    drop(db);

    let status = command(&root, &home, "status").output().unwrap();
    assert!(status.status.success());
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["stats"]["files"], 1);
    assert_eq!(status["revision"]["indexRevision"], 2);
    let exported = command(&root, &home, "export").output().unwrap();
    assert!(exported.status.success());
    let graph: Value = serde_json::from_slice(&exported.stdout).unwrap();
    let files = graph["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["path"], "boundary.js");
    assert_eq!(
        files[0]["text"].as_str().unwrap().as_bytes(),
        fs::read(root.join("boundary.js")).unwrap()
    );
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let (options, revision): (String, i64) = db
        .query_row(
            "SELECT reconcile_options,index_revision FROM index_metadata WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&options).unwrap()["maxFileBytes"],
        3_145_728
    );
    assert_eq!(
        revision, 3,
        "export must perform exactly one recorded-option takeover"
    );

    let default_home = temp.path().join("default-home");
    let default = command(&root, &default_home, "index").output().unwrap();
    assert!(!default.status.success());
    assert!(default.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&default.stderr).contains("unsafe or oversized input"),
        "{}",
        String::from_utf8_lossy(&default.stderr)
    );
}

#[test]
fn standalone_takeover_replays_original_relative_presentation_from_another_cwd() {
    use sha2::{Digest, Sha256};
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    let home = temp.path().join("home");
    let first_cwd = temp.path().join("ingress");
    let second_cwd = temp.path().join("takeover");
    for dir in [&root, &first_cwd, &second_cwd] {
        fs::create_dir(dir).unwrap();
    }
    let ingress_cwd = first_cwd.canonicalize().unwrap();
    let source = "function f() {}\nf();\n";
    fs::write(root.join("main.js"), source).unwrap();
    let source_hash = hex::encode(Sha256::digest(source.as_bytes()));
    let original_label = "scip npm display 1 main.js/f().";
    write_optional_presentation(&first_cwd, original_label, &source_hash);
    // A cwd-rebound replay would use this same-name but wrong presentation.
    write_optional_presentation(&second_cwd, "scip npm display 1 other().", "stale");
    let indexed = command(&root, &home, "index")
        .current_dir(&first_cwd)
        .arg("--scip")
        .arg("index.scip")
        .arg("--manifest")
        .arg("manifest.json")
        .output()
        .unwrap();
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    let initial: Value = serde_json::from_slice(&indexed.stdout).unwrap();
    assert_eq!(initial["publishedRevision"]["indexRevision"], 1);
    let indexes = if cfg!(target_os = "macos") {
        home.join("Library/Caches/dev.odin.baleyg/indexes")
    } else {
        home.join(".cache/baleyg/indexes")
    };
    let index_db = fs::read_dir(indexes)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
        .join("index.db");
    let recorded_options = || -> Value {
        let db = rusqlite::Connection::open(&index_db).unwrap();
        let text: String = db
            .query_row(
                "SELECT reconcile_options FROM index_metadata WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&text).unwrap()
    };
    let recorded = recorded_options();
    assert_eq!(
        recorded["scipPath"],
        ingress_cwd.join("index.scip").to_str().unwrap()
    );
    assert_eq!(
        recorded["manifestPath"],
        ingress_cwd.join("manifest.json").to_str().unwrap()
    );
    let status = command(&root, &home, "status")
        .current_dir(&second_cwd)
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["revision"]["indexRevision"], 2);
    assert_eq!(
        status["revision"]["indexGeneration"],
        initial["publishedRevision"]["indexGeneration"]
    );
    let export = command(&root, &home, "export")
        .current_dir(&second_cwd)
        .output()
        .unwrap();
    assert!(
        export.status.success(),
        "{}",
        String::from_utf8_lossy(&export.stderr)
    );
    let graph: Value = serde_json::from_slice(&export.stdout).unwrap();
    let f = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == "f")
        .unwrap();
    assert_eq!(f["displayLabel"], original_label);
    assert_eq!(
        recorded_options(),
        recorded,
        "takeovers must retain ingress identities"
    );
}

#[test]
fn standalone_takeover_keeps_configured_missing_presentation_absent() {
    use sha2::{Digest, Sha256};
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    let home = temp.path().join("home");
    let first_cwd = temp.path().join("ingress");
    let second_cwd = temp.path().join("takeover");
    for dir in [&root, &first_cwd, &second_cwd] {
        fs::create_dir(dir).unwrap();
    }
    let source = "function f() {}\nf();\n";
    fs::write(root.join("main.js"), source).unwrap();
    let source_hash = hex::encode(Sha256::digest(source.as_bytes()));
    // Inputs with these names exist only in B, never in the configured A.
    write_optional_presentation(&second_cwd, "scip npm display 1 main.js/f().", &source_hash);
    let indexed = command(&root, &home, "index")
        .current_dir(&first_cwd)
        .arg("--scip")
        .arg("index.scip")
        .arg("--manifest")
        .arg("manifest.json")
        .output()
        .unwrap();
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    assert!(!first_cwd.join("index.scip").exists());
    assert!(!first_cwd.join("manifest.json").exists());
    let export = command(&root, &home, "export")
        .current_dir(&second_cwd)
        .output()
        .unwrap();
    assert!(
        export.status.success(),
        "{}",
        String::from_utf8_lossy(&export.stderr)
    );
    let graph: Value = serde_json::from_slice(&export.stdout).unwrap();
    let f = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == "f")
        .unwrap();
    assert!(
        f["displayLabel"].is_null(),
        "absent A inputs must not become B labels"
    );
}

#[tokio::test]
async fn serve_ingress_persists_relative_presentation_paths_before_cross_cwd_takeover() {
    use sha2::{Digest, Sha256};
    use std::{
        io::{BufRead, BufReader},
        os::unix::fs::PermissionsExt,
        process::Stdio,
    };
    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    let home = temp.path().join("home");
    let first_cwd = temp.path().join("ingress");
    let second_cwd = temp.path().join("takeover");
    for dir in [&root, &first_cwd, &second_cwd] {
        fs::create_dir(dir).unwrap();
    }
    let ingress_cwd = first_cwd.canonicalize().unwrap();
    let source = "function f() {}\nf();\n";
    fs::write(root.join("main.js"), source).unwrap();
    let source_hash = hex::encode(Sha256::digest(source.as_bytes()));
    let original_label = "scip npm display 1 main.js/f().";
    write_optional_presentation(&first_cwd, original_label, &source_hash);
    write_optional_presentation(&second_cwd, "scip npm display 1 other().", "stale");
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let token = temp.path().join("token");
    fs::write(&token, TOKEN).unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut child = isolated_command(&home)
        .arg("serve")
        .arg("--workspace")
        .arg(&root)
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .arg("--token-file")
        .arg(&token)
        .arg("--scip")
        .arg("index.scip")
        .arg("--manifest")
        .arg("manifest.json")
        .current_dir(&first_cwd)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let server = Server(child);
    let startup = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        tokio::task::spawn_blocking(move || {
            let mut reader = BufReader::new(stderr);
            let mut text = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    return Err(text);
                }
                text.push_str(&line);
                if line.contains("Baleyg:") {
                    return Ok(text);
                }
            }
        }),
    )
    .await
    .expect("daemon startup timed out")
    .unwrap()
    .unwrap_or_else(|text| panic!("daemon exited before binding: {text}"));
    assert!(
        !startup.contains("Evidence unavailable at startup"),
        "{startup}"
    );
    drop(server);
    let export = command(&root, &home, "export")
        .current_dir(&second_cwd)
        .output()
        .unwrap();
    assert!(
        export.status.success(),
        "{}",
        String::from_utf8_lossy(&export.stderr)
    );
    let graph: Value = serde_json::from_slice(&export.stdout).unwrap();
    let f = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["name"] == "f")
        .unwrap();
    assert_eq!(f["displayLabel"], original_label);
    let indexes = if cfg!(target_os = "macos") {
        home.join("Library/Caches/dev.odin.baleyg/indexes")
    } else {
        home.join(".cache/baleyg/indexes")
    };
    let index_db = fs::read_dir(indexes)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
        .join("index.db");
    let db = rusqlite::Connection::open(&index_db).unwrap();
    let recorded: String = db
        .query_row(
            "SELECT reconcile_options FROM index_metadata WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let recorded: Value = serde_json::from_str(&recorded).unwrap();
    assert_eq!(
        recorded["scipPath"],
        ingress_cwd.join("index.scip").to_str().unwrap()
    );
    assert_eq!(
        recorded["manifestPath"],
        ingress_cwd.join("manifest.json").to_str().unwrap()
    );
}

#[test]
fn standalone_read_refuses_legacy_relative_recorded_presentation_options() {
    use sha2::{Digest, Sha256};
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    let home = temp.path().join("home");
    let first_cwd = temp.path().join("ingress");
    let second_cwd = temp.path().join("takeover");
    for dir in [&root, &first_cwd, &second_cwd] {
        fs::create_dir(dir).unwrap();
    }
    let ingress_cwd = first_cwd.canonicalize().unwrap();
    let source = "function f() {}\nf();\n";
    fs::write(root.join("main.js"), source).unwrap();
    let source_hash = hex::encode(Sha256::digest(source.as_bytes()));
    write_optional_presentation(&first_cwd, "scip npm display 1 main.js/f().", &source_hash);
    write_optional_presentation(&second_cwd, "scip npm display 1 other().", "stale");
    let indexed = command(&root, &home, "index")
        .current_dir(&first_cwd)
        .arg("--scip")
        .arg("index.scip")
        .arg("--manifest")
        .arg("manifest.json")
        .output()
        .unwrap();
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    let indexes = if cfg!(target_os = "macos") {
        home.join("Library/Caches/dev.odin.baleyg/indexes")
    } else {
        home.join(".cache/baleyg/indexes")
    };
    let index_db = fs::read_dir(indexes)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap()
        .join("index.db");
    // Model one previously persisted relative record with a matching inventory;
    // changing only reconcile_options would be rejected earlier as torn metadata.
    let db = rusqlite::Connection::open(&index_db).unwrap();
    let raw: String = db
        .query_row(
            "SELECT reconcile_options FROM index_metadata WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut previous: baleyg::indexer::ReconcileOptions = serde_json::from_str(&raw).unwrap();
    previous.scip_path = Some("index.scip".into());
    previous.manifest_path = Some("manifest.json".into());
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    db.execute(
        "UPDATE index_metadata SET reconcile_options=?1 WHERE singleton=1",
        [serde_json::to_string(&previous).unwrap()],
    )
    .unwrap();
    for (role, name) in [("scip", "index.scip"), ("manifest", "manifest.json")] {
        let old_key = format!("presentation-{role}:{}", ingress_cwd.join(name).display());
        let new_key = format!("presentation-{role}:{name}");
        assert_eq!(
            db.execute(
                "UPDATE revision_capture_inputs SET input_key=?1 WHERE input_key=?2",
                rusqlite::params![new_key, old_key],
            )
            .unwrap(),
            1
        );
    }
    db.execute_batch("COMMIT").unwrap();
    drop(db);
    let rejected = command(&root, &home, "status")
        .current_dir(&second_cwd)
        .output()
        .unwrap();
    assert!(
        !rejected.status.success(),
        "legacy relative options were replayed"
    );
    assert!(
        rejected.stdout.is_empty(),
        "no cwd-rebound evidence may escape"
    );
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("run explicit baleyg index"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    let db = rusqlite::Connection::open(&index_db).unwrap();
    let revision: i64 = db
        .query_row(
            "SELECT index_revision FROM index_metadata WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        revision, 1,
        "relative legacy record cannot republish from B"
    );
}

#[test]
fn explicit_cli_index_recreates_corruption_but_bounded_reads_refuse_unknown_options() {
    use std::os::unix::fs::MetadataExt;
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.js"), "function a() {}\n").unwrap();
    let first = command(&root, &home, "index")
        .arg("--max-file-bytes")
        .arg("2097152")
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let indexes = if cfg!(target_os = "macos") {
        home.join("Library/Caches/dev.odin.baleyg/indexes")
    } else {
        home.join(".cache/baleyg/indexes")
    };
    let dir = fs::read_dir(&indexes)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap();
    let index = dir.join("index.db");
    let leader = dir.join("leader.lock");
    let old_inode = fs::symlink_metadata(&leader).unwrap();
    fs::write(&index, b"invalid-sqlite-header-with-at-least-20-bytes").unwrap();
    let corrupt = fs::read(&index).unwrap();
    for sub in ["status", "symbols", "query", "export"] {
        let mut cmd = command(&root, &home, sub);
        match sub {
            "symbols" => {
                cmd.arg("--search").arg("a");
            }
            "query" => {
                cmd.arg("--seed").arg("unknown");
            }
            _ => {}
        }
        let refused = cmd.output().unwrap();
        assert!(!refused.status.success(), "{sub} unexpectedly succeeded");
        assert!(
            refused.stdout.is_empty(),
            "{sub} emitted unverified evidence"
        );
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("explicit baleyg index"),
            "{sub}: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
        assert_eq!(fs::read(&index).unwrap(), corrupt);
    }
    let recovered = command(&root, &home, "index")
        .arg("--max-file-bytes")
        .arg("2097152")
        .output()
        .unwrap();
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    let output: Value = serde_json::from_slice(&recovered.stdout).unwrap();
    assert_eq!(output["publishedRevision"], output["status"]["revision"]);
    assert_eq!(output["publishedRevision"]["indexRevision"], 1);
    assert_ne!(
        output["publishedRevision"]["indexGeneration"],
        first["publishedRevision"]["indexGeneration"]
    );
    let new_inode = fs::symlink_metadata(&leader).unwrap();
    assert_eq!(
        (old_inode.dev(), old_inode.ino()),
        (new_inode.dev(), new_inode.ino())
    );
    let db = rusqlite::Connection::open(&index).unwrap();
    let recorded: String = db
        .query_row(
            "SELECT reconcile_options FROM index_metadata WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&recorded).unwrap()["maxFileBytes"],
        2_097_152
    );
}

#[tokio::test]
async fn corrupted_daemon_startup_uses_configured_options_and_does_not_fallback_when_busy() {
    use baleyg::store::topology::UseGuard;
    use std::{os::unix::fs::PermissionsExt, process::Stdio};
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn startup_diagnostic(server: &mut Server, stderr_path: &std::path::Path, port: u16) -> String {
        let status = format!("{:?}", server.0.try_wait());
        let stderr = fs::read(stderr_path)
            .map(|bytes| {
                let redacted = String::from_utf8_lossy(&bytes).replace(TOKEN, "[REDACTED]");
                redacted
                    .chars()
                    .rev()
                    .take(1024)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>()
            })
            .unwrap_or_else(|error| format!("<stderr unreadable: {error}>"));
        format!("port={port}, child_status={status}, stderr_tail={stderr:?}")
    }
    async fn serve(
        root: &std::path::Path,
        home: &std::path::Path,
        token: &std::path::Path,
        phase: &str,
    ) -> (Server, String, reqwest::Client) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        fs::create_dir_all(home).unwrap();
        let stderr_path = home.join(format!("daemon-{phase}-{port}.stderr"));
        let stderr = fs::File::create(&stderr_path).unwrap();
        let child = isolated_command(home)
            .arg("serve")
            .arg("--workspace")
            .arg(root)
            .arg("--bind")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--token-file")
            .arg(token)
            .arg("--max-file-bytes")
            .arg("3145728")
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .unwrap();
        let mut server = Server(child);
        let base = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        let mut last_transport = String::new();
        loop {
            match server.0.try_wait() {
                Ok(None) => {}
                Ok(Some(status)) => panic!(
                    "daemon exited before /healthz readiness ({status}); {}",
                    startup_diagnostic(&mut server, &stderr_path, port)
                ),
                Err(error) => panic!(
                    "daemon liveness check failed ({error}); {}",
                    startup_diagnostic(&mut server, &stderr_path, port)
                ),
            }
            match tokio::time::timeout_at(deadline, client.get(format!("{base}/healthz")).send())
                .await
            {
                Ok(Ok(response)) => {
                    assert_eq!(
                        response.status(),
                        reqwest::StatusCode::OK,
                        "daemon /healthz returned wrong HTTP status; {}",
                        startup_diagnostic(&mut server, &stderr_path, port)
                    );
                    return (server, base, client);
                }
                Ok(Err(error)) if error.is_connect() || error.is_timeout() => {
                    last_transport = error.to_string();
                }
                Ok(Err(error)) => panic!(
                    "daemon /healthz non-transport error ({error}); {}",
                    startup_diagnostic(&mut server, &stderr_path, port)
                ),
                Err(_) => panic!(
                    "daemon /healthz readiness timed out (last transport: {last_transport}); {}",
                    startup_diagnostic(&mut server, &stderr_path, port)
                ),
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "daemon /healthz readiness timed out (last transport: {last_transport}); {}",
                    startup_diagnostic(&mut server, &stderr_path, port)
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.js"), "function a() {}\n").unwrap();
    let initial = command(&root, &home, "index").output().unwrap();
    assert!(initial.status.success());
    let old_pin: Value = serde_json::from_slice(&initial.stdout).unwrap();
    let indexes = if cfg!(target_os = "macos") {
        home.join("Library/Caches/dev.odin.baleyg/indexes")
    } else {
        home.join(".cache/baleyg/indexes")
    };
    let dir = fs::read_dir(indexes)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.is_dir())
        .unwrap();
    let index = dir.join("index.db");
    fs::write(&index, b"broken sqlite index header").unwrap();
    let corrupt = fs::read(&index).unwrap();
    let token = temp.path().join("token");
    fs::write(&token, TOKEN).unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let use_lock = dir.parent().unwrap().join(format!(
        "{}.lock",
        dir.file_name().unwrap().to_string_lossy()
    ));
    let reader = UseGuard::acquire_existing(&use_lock, false, true).unwrap();
    let (busy_server, busy_url, busy_client) = serve(&root, &home, &token, "busy").await;
    let busy = busy_client
        .get(format!("{busy_url}/api/status"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(busy.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = busy.json().await.unwrap();
    assert_eq!(body["error"]["code"], "recovery_required");
    assert_eq!(fs::read(&index).unwrap(), corrupt);
    drop(busy_server);
    drop(busy_client);
    drop(reader);
    let (ready_server, url, ready_client) = serve(&root, &home, &token, "ready").await;
    let ready = ready_client
        .get(format!("{url}/api/status"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(ready.status(), reqwest::StatusCode::OK);
    let status: Value = ready.json().await.unwrap();
    assert_eq!(status["revision"]["indexRevision"], 1);
    assert_ne!(
        status["revision"]["indexGeneration"],
        old_pin["publishedRevision"]["indexGeneration"]
    );
    let db = rusqlite::Connection::open(&index).unwrap();
    let options: String = db
        .query_row(
            "SELECT reconcile_options FROM index_metadata WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&options).unwrap()["maxFileBytes"],
        3_145_728
    );
    drop(ready_server);
}

#[test]
fn cli_round_trip_uses_persistent_store_and_never_executes_workspace() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    let state = temp.path().join("state");
    fs::create_dir(&root).unwrap();
    fs::write(
        root.join("flow.js"),
        "export function start() { return send(); }\nfunction send() {}\n",
    )
    .unwrap();
    fs::write(
        root.join("package.json"),
        r#"{"scripts":{"postinstall":"touch SHOULD_NOT_EXIST"}}"#,
    )
    .unwrap();
    let before = fs::read(root.join("flow.js")).unwrap();
    let output = command(&root, &state, "index")
        .arg("--max-file-bytes")
        .arg("1048576")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"]["stats"]["files"], 1);
    assert_eq!(result["status"]["stats"]["semanticState"], "unavailable");
    let status = command(&root, &state, "status").output().unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["stats"]["files"], 1);
    let output = command(&root, &state, "symbols")
        .arg("--search")
        .arg("start")
        .output()
        .unwrap();
    assert!(output.status.success());
    let symbols: Value = serde_json::from_slice(&output.stdout).unwrap();
    let seed = symbols["items"][0]["id"].as_str().unwrap();
    let output = command(&root, &state, "query")
        .arg("--seed")
        .arg(seed)
        .output()
        .unwrap();
    assert!(output.status.success());
    let view: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(view["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(view["calls"].as_array().unwrap().len(), 1);
    assert!(view["calls"][0].get("resolution").is_none());
    assert!(view["calls"][0].get("target").is_none());
    let path = temp.path().join("graph.json");
    assert!(
        command(&root, &state, "export")
            .arg("--output")
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let bytes = fs::read(&path).unwrap();
    let graph: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        graph["files"][0]["text"],
        std::str::from_utf8(&before).unwrap()
    );
    assert!(
        !command(&root, &state, "export")
            .arg("--output")
            .arg(&path)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::read(root.join("flow.js")).unwrap(), before);
    assert!(!root.join("SHOULD_NOT_EXIST").exists());
    assert!(!root.join(".baleyg").exists());
}
#[test]
fn cli_refuses_remote_bind_and_invalid_limits() {
    let temp = TempDir::new().unwrap();
    let state = temp.path().join("state");
    let output = command(temp.path(), &state, "serve")
        .arg("--bind")
        .arg("0.0.0.0:8877")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("loopback"));
    let output = command(temp.path(), &state, "index")
        .arg("--max-file-bytes")
        .arg("0")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let output = Command::new(env!("CARGO_BIN_EXE_baleyg"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn live_jev_requires_paired_explicit_budget_flags() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let state = temp.path().join("state");
    for flags in [
        vec!["--jev-budget-dir", "unused"],
        vec!["--jev-budget-cents", "500"],
        vec!["--jev-budget-dir", "unused", "--jev-budget-cents", "501"],
        vec!["--jev-budget-dir", "unused", "--jev-budget-cents", "0"],
    ] {
        let output = command(&source, &state, "serve")
            .args(flags)
            .env_remove("JEV_KEY")
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
    assert!(
        !state.exists(),
        "invalid opt-in must fail before state creation"
    );
}

#[test]
fn live_jev_missing_key_fails_without_creating_budget() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let state = temp.path().join("state");
    let budget = temp.path().join("budget");
    let output = command(&source, &state, "serve")
        .args([
            "--bind",
            "127.0.0.1:0",
            "--jev-budget-cents",
            "500",
            "--jev-budget-dir",
        ])
        .arg(&budget)
        .env_remove("JEV_KEY")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("JEV_KEY must be configured"));
    assert!(!budget.exists());
}

#[cfg(unix)]
#[test]
fn malformed_jev_environment_never_echoes_credential_bytes() {
    use std::os::unix::ffi::OsStringExt;
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let budget = temp.path().join("budget");
    let output = command(&source, &temp.path().join("state"), "serve")
        .args([
            "--bind",
            "127.0.0.1:0",
            "--jev-budget-cents",
            "500",
            "--jev-budget-dir",
        ])
        .arg(&budget)
        .env(
            "JEV_KEY",
            std::ffi::OsString::from_vec(b"SYNTHETIC_INVALID_CREDENTIAL_\xff".to_vec()),
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("JEV_KEY must be configured"));
    assert!(!stderr.contains("SYNTHETIC_INVALID_CREDENTIAL"));
    assert!(!budget.exists());
}

#[test]
fn acp_requires_complete_explicit_allowance_flags() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let state = temp.path().join("state");
    for flags in [
        vec!["--acp-runner", "unused"],
        vec!["--acp-state-dir", "unused"],
        vec!["--acp-max-attempts", "1"],
        vec![
            "--acp-runner",
            "unused",
            "--acp-state-dir",
            "unused",
            "--acp-max-attempts",
            "0",
        ],
        vec![
            "--acp-runner",
            "unused",
            "--acp-state-dir",
            "unused",
            "--acp-max-attempts",
            "21",
        ],
    ] {
        let output = command(&source, &state, "serve")
            .args(flags)
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
    assert!(
        !state.exists(),
        "invalid opt-in must fail before state creation"
    );
}

#[test]
fn fixed_locations_and_removed_flag() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.js"), "function seed() {}").unwrap();
    let initial = command(&root, &home, "status").output().unwrap();
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    let initial: Value = serde_json::from_slice(&initial.stdout).unwrap();
    assert_eq!(initial["revision"]["indexRevision"], 1);
    let status = command(&root, &home, "index").output().unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let first: Value = serde_json::from_slice(&status.stdout).unwrap();
    let first = &first["status"];
    assert_eq!(first["revision"]["indexRevision"], 2);
    assert_eq!(
        first["revision"]["indexGeneration"],
        initial["revision"]["indexGeneration"]
    );
    let generation = first["revision"]["indexGeneration"].as_str().unwrap();
    assert_eq!(
        uuid::Uuid::parse_str(generation).unwrap().get_version_num(),
        4
    );
    let cache = home.join(if cfg!(target_os = "macos") {
        "Library/Caches/dev.odin.baleyg"
    } else {
        ".cache/baleyg"
    });
    assert!(
        cache.exists(),
        "fixed cache missing under {}",
        home.display()
    );
    assert!(!home.join("state/cache.db").exists());
    assert!(!home.join("state/workspace.db").exists());
    let removed = command(&root, &home, "status")
        .arg("--state-dir")
        .arg(home.join("override"))
        .output()
        .unwrap();
    assert!(!removed.status.success());
    assert!(String::from_utf8_lossy(&removed.stderr).contains("--state-dir"));
    let final_status: Value =
        serde_json::from_slice(&command(&root, &home, "status").output().unwrap().stdout).unwrap();
    assert_eq!(
        final_status["revision"]["indexGeneration"],
        first["revision"]["indexGeneration"]
    );
    assert_eq!(final_status["revision"]["indexRevision"], 3);
}
#[test]
fn index_forwards_pair_and_reports_pair() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.js"), "function seed() {}").unwrap();
    let initial = command(&root, &home, "status").output().unwrap();
    assert!(initial.status.success());
    let initial: Value = serde_json::from_slice(&initial.stdout).unwrap();
    let result = command(&root, &home, "index").output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let published: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        published["status"]["revision"],
        published["publishedRevision"]
    );
    assert_eq!(
        published["status"]["evidenceFormat"],
        "terminal-native-graph-v1"
    );
    assert_eq!(published["publishedRevision"]["indexRevision"], 2);
    assert_eq!(
        published["publishedRevision"]["indexGeneration"],
        initial["revision"]["indexGeneration"]
    );
}
#[test]
fn export_path_refusals() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.js"), "function seed() {}").unwrap();
    assert!(
        command(&root, &home, "index")
            .output()
            .unwrap()
            .status
            .success()
    );
    let forbidden = root.join("graph.json");
    let denied = command(&root, &home, "export")
        .arg("--output")
        .arg(&forbidden)
        .output()
        .unwrap();
    assert!(!denied.status.success());
    assert!(!forbidden.exists());
    let valid = temp.path().join("graph.json");
    assert!(
        command(&root, &home, "export")
            .arg("--output")
            .arg(&valid)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(valid.exists());
}
#[test]
fn current_commands_pair_matrix() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.js"), "function seed() {}").unwrap();
    let index = command(&root, &home, "index").output().unwrap();
    assert!(index.status.success());
    let pin: Value =
        serde_json::from_slice::<Value>(&index.stdout).unwrap()["publishedRevision"].clone();
    let status: Value =
        serde_json::from_slice(&command(&root, &home, "status").output().unwrap().stdout).unwrap();
    assert_eq!(
        status["revision"]["indexGeneration"],
        pin["indexGeneration"]
    );
    assert_eq!(status["revision"]["indexRevision"], 2);
    let symbols: Value =
        serde_json::from_slice(&command(&root, &home, "symbols").output().unwrap().stdout).unwrap();
    assert_eq!(
        symbols["revision"]["indexGeneration"],
        pin["indexGeneration"]
    );
    assert_eq!(symbols["revision"]["indexRevision"], 3);
    let seed = symbols["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "seed")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let query: Value = serde_json::from_slice(
        &command(&root, &home, "query")
            .arg("--seed")
            .arg(seed)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(query["revision"]["indexGeneration"], pin["indexGeneration"]);
    assert_eq!(query["revision"]["indexRevision"], 4);
    let exported: Value =
        serde_json::from_slice(&command(&root, &home, "export").output().unwrap().stdout).unwrap();
    assert_eq!(exported["files"].as_array().unwrap().len(), 1);
    let status: Value =
        serde_json::from_slice(&command(&root, &home, "status").output().unwrap().stdout).unwrap();
    assert_eq!(
        status["revision"]["indexGeneration"],
        pin["indexGeneration"]
    );
    assert_eq!(status["revision"]["indexRevision"], 6);
}

#[test]
fn external_destinations_are_rejected_before_git_marker_creation() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join(".git")).unwrap();
    let token = root.join("token");
    let serve = isolated_command(&home)
        .arg("serve")
        .arg("--workspace")
        .arg(&root)
        .arg("--token-file")
        .arg(&token)
        .output()
        .unwrap();
    assert!(!serve.status.success());
    assert!(!root.join(".git/baleyg/workspace-id").exists());
    assert!(!token.exists());
    let output = root.join("graph.json");
    let export = command(&root, &home, "export")
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();
    assert!(!export.status.success());
    assert!(!output.exists());
    assert!(!root.join(".git/baleyg/workspace-id").exists());
}

#[test]
fn overlapping_git_workspace_refuses_before_marker_or_managed_entries() {
    for fixed in ["cache", "data"] {
        for sub in ["status", "index", "symbols", "query", "export", "serve"] {
            let temp = TempDir::new().unwrap();
            let home = temp.path().join("home");
            let (cache, data) = if cfg!(target_os = "macos") {
                (
                    home.join("Library/Caches/dev.odin.baleyg"),
                    home.join("Library/Application Support/dev.odin.baleyg"),
                )
            } else {
                (home.join(".cache/baleyg"), home.join(".local/share/baleyg"))
            };
            let workspace = if fixed == "cache" { &cache } else { &data };
            fs::create_dir_all(workspace.join(".git")).unwrap();
            let mut cmd = command(workspace, &home, sub);
            cmd.env("XDG_CACHE_HOME", home.join(".cache"))
                .env("XDG_DATA_HOME", home.join(".local/share"));
            if sub == "query" {
                cmd.arg("--seed").arg("a");
            }
            let output = cmd.output().unwrap();
            assert!(!output.status.success(), "{fixed} {sub}");
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("workspace root overlaps fixed topology"),
                "{fixed} {sub}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                !workspace.join(".git/baleyg/workspace-id").exists(),
                "{fixed} {sub}"
            );
            assert!(!cache.join("indexes").exists(), "{fixed} {sub}");
            assert!(!data.join("workspaces").exists(), "{fixed} {sub}");
            assert!(!home.join("token").exists(), "{fixed} {sub}");
        }
    }
}

#[test]
fn gc_report_without_workspace_does_not_create_state() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let output = isolated_command(&home)
        .args(["gc", "--report"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let inventory: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(inventory["derived"], serde_json::json!([]));
    assert_eq!(inventory["records"], serde_json::json!([]));
    assert!(!home.exists());
    let denied = isolated_command(&home).arg("gc").output().unwrap();
    assert!(!denied.status.success());
    assert!(!home.exists());
    let root = temp.path().join("work");
    fs::create_dir(&root).unwrap();
    assert!(
        command(&root, &home, "index")
            .output()
            .unwrap()
            .status
            .success()
    );
    let output = isolated_command(&home)
        .args(["gc", "--report"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let inventory: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(inventory["derived"].as_array().unwrap().len(), 1);
    assert_eq!(inventory["derived"][0]["status"], "unknown");
    assert_eq!(inventory["derived"][0]["reason"], "recent_open");
}

#[test]
fn gc_cli_reports_multiple_indexes_in_sorted_order() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let mut workspaces = (0..3)
        .map(|n| {
            let root = temp.path().join(format!("sorted-index-{n}"));
            fs::create_dir(&root).unwrap();
            let identity =
                baleyg::store::topology::WorkspaceIdentity::discover(Some(&root), &root).unwrap();
            (identity.root_key, root)
        })
        .collect::<Vec<_>>();
    workspaces.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, root) in &workspaces {
        let status = command(root, &home, "index").output().unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
    }
    let output = isolated_command(&home)
        .args(["gc", "--report"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let actual = report["derived"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["rootKey"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let mut expected = workspaces
        .into_iter()
        .map(|(key, _)| key)
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(actual, expected);
}

#[test]
fn forget_cli_requires_confirmation_and_preserves_unrelated_state() {
    use baleyg::{
        model::SavedView,
        store::topology::{DurableRecords, TopologyRoots, UseGuard, WorkspaceIdentity},
    };
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let root = temp.path().join("work");
    fs::create_dir(&root).unwrap();
    let data = home.join(if cfg!(target_os = "macos") {
        "Library/Application Support/dev.odin.baleyg"
    } else {
        ".local/share/baleyg"
    });
    let roots = TopologyRoots::isolated_for_tests(home.join("cache"), data);
    let identity = WorkspaceIdentity::discover(Some(&root), &root).unwrap();
    let view: SavedView = serde_json::from_str(
        r#"{"id":"view1","title":"A view","query":{"seed":"symbol"},"pins":{}}"#,
    )
    .unwrap();
    let records = DurableRecords::new(&roots, &identity);
    records.put_view(&view).unwrap();
    let id = identity.record_id.as_str();
    let run = |which: &str, yes: bool| {
        let mut cmd = isolated_command(&home);
        cmd.arg("forget").arg(which);
        if yes {
            cmd.arg("--yes");
        }
        cmd.output().unwrap()
    };
    assert!(!run("../work", true).status.success());
    let denied = run(id, false);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("terminal or --yes"));
    assert!(roots.record_db(&identity).exists());
    let shared =
        UseGuard::acquire_existing(&roots.record_use_lock(&identity), false, true).unwrap();
    assert!(!run(id, true).status.success());
    drop(shared);
    let other = home.join("untouched");
    fs::write(&other, "still here").unwrap();
    let result = run(id, true);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("every checkout sharing this UUID"));
    assert!(stderr.contains("Saved views: 1; annotations: 0"));
    assert!(stderr.contains(identity.root.to_str().unwrap()));
    assert!(!roots.record_dir(&identity).exists());
    assert!(!roots.record_use_lock(&identity).exists());
    assert_eq!(fs::read_to_string(other).unwrap(), "still here");
    assert!(!run(id, true).status.success());
}

#[test]
fn forget_yes_refuses_unknown_sqlite_schema_without_removing_state() {
    use baleyg::{
        model::SavedView,
        store::topology::{DurableRecords, TopologyRoots, WorkspaceIdentity},
    };
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let work = temp.path().join("work");
    fs::create_dir(&work).unwrap();
    let data = home.join(if cfg!(target_os = "macos") {
        "Library/Application Support/dev.odin.baleyg"
    } else {
        ".local/share/baleyg"
    });
    let roots = TopologyRoots::isolated_for_tests(home.join("cache"), data);
    let identity = WorkspaceIdentity::discover(Some(&work), &work).unwrap();
    let view: SavedView = serde_json::from_str(
        r#"{"id":"view1","title":"A view","query":{"seed":"symbol"},"pins":{}}"#,
    )
    .unwrap();
    DurableRecords::new(&roots, &identity)
        .put_view(&view)
        .unwrap();
    let path = roots.record_db(&identity);
    let lock = roots.record_use_lock(&identity);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE unknown(id INTEGER)")
        .unwrap();
    drop(db);
    let before = fs::read(&path).unwrap();
    let lock_before = fs::read(&lock).unwrap();
    let unrelated = home.join("unrelated");
    fs::write(&unrelated, "keep").unwrap();
    let result = isolated_command(&home)
        .args(["forget", &identity.record_id, "--yes"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("unexpected SQLite schema"));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::read(&lock).unwrap(), lock_before);
    assert_eq!(fs::read(&unrelated).unwrap(), b"keep");
}

// Read the normalized publication, including every native row, from the real daemon's database.
fn real_index_db(home: &std::path::Path) -> std::path::PathBuf {
    fn find(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == "index.db") {
                return Some(path);
            }
            if path.is_dir()
                && let Some(db) = find(&path)
            {
                return Some(db);
            }
        }
        None
    }
    find(home).expect("published native database")
}
fn real_native_snapshot(home: &std::path::Path) -> Value {
    use rusqlite::types::ValueRef;
    let db = rusqlite::Connection::open(real_index_db(home)).unwrap();
    let source_set: (String, String) = db
        .query_row("SELECT id,root_id FROM native_source_sets", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    let (generation, revision_number, incarnation): (String, i64, String) = db
        .query_row(
            "SELECT index_generation,index_revision,reconciled_incarnation FROM index_metadata",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert!(uuid::Uuid::parse_str(&generation).is_ok());
    assert!(uuid::Uuid::parse_str(&incarnation).is_ok());
    let pin_id = format!("pin:v1:{generation}:{revision_number}");
    let (header_pin, header_incarnation, header_revision): (String, String, i64) = db
        .query_row(
            "SELECT id,reconciled_incarnation,published_index_revision FROM native_revisions WHERE id=?1",
            [&pin_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(header_pin, pin_id);
    assert_eq!(header_incarnation, incarnation);
    assert_eq!(header_revision, revision_number);
    let violations: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
    let revision: (String, String, String, String, String) = db.query_row(
        "SELECT id,source_set_id,toolchain_hash,config_hash,dependency_hash FROM native_revisions WHERE id=?1", [&pin_id],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))
    ).unwrap();
    let mut documents = Vec::new();
    let mut stmt = db.prepare(
        "SELECT v.source_set_id,v.language,m.path,m.revision_id,v.content_hash,v.byte_length,v.source_bytes FROM revision_documents m JOIN document_versions v ON v.id=m.document_version_id WHERE m.revision_id=?1 ORDER BY m.path"
    ).unwrap();
    let rows = stmt
        .query_map([&pin_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, Vec<u8>>(6)?,
            ))
        })
        .unwrap();
    for row in rows {
        let (set, language, path, revision, hash, length, bytes) = row.unwrap();
        documents.push(serde_json::json!({"sourceSetId":set,"language":language,"path":path,
            "revisionId":revision,"contentHash":hash,"byteLength":length,"bytesHex":hex::encode(bytes)}));
    }
    let mut all_rows = serde_json::Map::new();
    let mut names = db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND (name LIKE 'native_%' OR name IN ('document_versions','revision_documents','revision_capture_inputs','graph_projections','graph_nodes','graph_calls','graph_regions','class_projections','classes','class_relations')) ORDER BY name").unwrap();
    for name in names.query_map([], |r| r.get::<_, String>(0)).unwrap() {
        let name = name.unwrap();
        let (predicate, alias) = match name.as_str() {
            "native_revisions" => ("id=?1", ""),
            "revision_capture_inputs" | "revision_documents" => ("revision_id=?1", ""),
            "native_source_sets" => (
                "id=(SELECT source_set_id FROM native_revisions WHERE id=?1)",
                "",
            ),
            "native_source_set_languages" | "native_source_set_dependencies" => (
                "source_set_id=(SELECT source_set_id FROM native_revisions WHERE id=?1)",
                "",
            ),
            "native_producers" => (
                "EXISTS(SELECT 1 FROM document_versions v JOIN revision_documents d ON d.document_version_id=v.id WHERE d.revision_id=?1 AND v.producer_id=selected.id AND v.producer_version=selected.version)",
                " selected",
            ),
            "native_producer_languages" | "native_producer_inputs" => (
                "EXISTS(SELECT 1 FROM document_versions v JOIN revision_documents d ON d.document_version_id=v.id WHERE d.revision_id=?1 AND v.producer_id=selected.producer_id AND v.producer_version=selected.producer_version)",
                " selected",
            ),
            "document_versions" => (
                "id IN (SELECT document_version_id FROM revision_documents WHERE revision_id=?1)",
                "",
            ),
            "graph_projections" => (
                "id IN (SELECT graph_projection_id FROM revision_documents WHERE revision_id=?1)",
                "",
            ),
            "graph_nodes" | "graph_calls" | "graph_regions" => (
                "projection_id IN (SELECT graph_projection_id FROM revision_documents WHERE revision_id=?1)",
                "",
            ),
            "class_projections" => (
                "id IN (SELECT class_projection_id FROM revision_documents WHERE revision_id=?1)",
                "",
            ),
            "classes" | "class_relations" => (
                "projection_id IN (SELECT class_projection_id FROM revision_documents WHERE revision_id=?1)",
                "",
            ),
            table if table.starts_with("native_version_") => (
                "version_id IN (SELECT document_version_id FROM revision_documents WHERE revision_id=?1)",
                "",
            ),
            _ => panic!("unexpected v8 evidence table: {name}"),
        };
        let mut table = db
            .prepare(&format!(
                "SELECT * FROM \"{name}\"{alias} WHERE {predicate} ORDER BY rowid"
            ))
            .unwrap();
        let columns = table.column_count();
        let records = table
            .query_map([&pin_id], |row| {
                let mut cells = Vec::new();
                for i in 0..columns {
                    let cell = match row.get_ref(i)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(n) => serde_json::json!(n),
                        ValueRef::Real(n) => serde_json::json!(n),
                        ValueRef::Text(bytes) => serde_json::json!(String::from_utf8_lossy(bytes)),
                        ValueRef::Blob(bytes) => serde_json::json!({"blobHex":hex::encode(bytes)}),
                    };
                    cells.push(cell);
                }
                Ok(cells)
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        all_rows.insert(name, serde_json::json!(records));
    }
    assert_eq!(
        all_rows.len(),
        28,
        "retain every v8 evidence table in raw snapshot"
    );
    serde_json::json!({"sourceSet":{"id":source_set.0,"rootId":source_set.1},
        "revision":{"id":revision.0,"sourceSetId":revision.1,"toolchainHash":revision.2,
            "configHash":revision.3,"dependencyHash":revision.4},
        "documents":documents,"allRows":all_rows})
}

// Only successful same-byte reindex may ignore per-publication pin cells.
// All failed/no-op comparisons must keep real_native_snapshot entirely raw.
fn same_bytes_evidence(snapshot: &Value) -> Value {
    let pin = snapshot["revision"]["id"].as_str().unwrap().to_owned();
    let header = snapshot["allRows"]["native_revisions"].as_array().unwrap();
    assert_eq!(header.len(), 1);
    let header = header[0].as_array().unwrap();
    assert_eq!(header[0], pin);
    assert!(uuid::Uuid::parse_str(header[8].as_str().unwrap()).is_ok());
    assert!(header[14].as_i64().is_some_and(|revision| revision > 0));
    for table in ["revision_capture_inputs", "revision_documents"] {
        for row in snapshot["allRows"][table].as_array().unwrap() {
            assert_eq!(row[0], pin, "{table} must reference the admitted pin");
        }
    }
    for doc in snapshot["documents"].as_array().unwrap() {
        assert_eq!(doc["revisionId"], pin);
    }
    let mut evidence = snapshot.clone();
    evidence["revision"]["id"] = serde_json::json!("<current-pin>");
    for doc in evidence["documents"].as_array_mut().unwrap() {
        doc["revisionId"] = serde_json::json!("<current-pin>");
    }
    let header = evidence["allRows"]["native_revisions"]
        .as_array_mut()
        .unwrap()[0]
        .as_array_mut()
        .unwrap();
    header[0] = serde_json::json!("<current-pin>");
    header[8] = serde_json::json!("<leader-incarnation>");
    header[14] = serde_json::json!("<current-revision>");
    for table in ["revision_capture_inputs", "revision_documents"] {
        for row in evidence["allRows"][table].as_array_mut().unwrap() {
            row[0] = serde_json::json!("<current-pin>");
        }
    }
    evidence
}

fn real_export(root: &std::path::Path, home: &std::path::Path) -> Value {
    let output = command(root, home, "export").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
async fn real_api(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    method: reqwest::Method,
    route: &str,
    body: Option<Value>,
) -> (reqwest::StatusCode, Value) {
    let mut request = client
        .request(method, format!("{url}{route}"))
        .header("Origin", url)
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    (status, response.json().await.unwrap())
}

struct SavedItemServer(std::process::Child);
impl Drop for SavedItemServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn start_saved_item_server(
    temp: &std::path::Path,
    root: &std::path::Path,
    home: &std::path::Path,
    token: &str,
) -> (SavedItemServer, reqwest::Client, String) {
    use std::{io::Read, os::unix::fs::PermissionsExt, time::Duration};
    let token_file = temp.join("saved-item-token");
    fs::write(&token_file, token).unwrap();
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let stderr_path = temp.join(format!("saved-item-daemon-{port}.stderr"));
    let mut server = SavedItemServer(
        isolated_command(home)
            .arg("serve")
            .arg("--workspace")
            .arg(root)
            .arg("--bind")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--token-file")
            .arg(&token_file)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(
                fs::File::create(&stderr_path).unwrap(),
            ))
            .spawn()
            .unwrap(),
    );
    let url = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if client
                .get(format!("{url}/healthz"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if ready.is_err() {
        let status = server.0.try_wait().unwrap();
        let mut diagnostic = String::new();
        fs::File::open(&stderr_path)
            .unwrap()
            .take(4096)
            .read_to_string(&mut diagnostic)
            .unwrap();
        panic!(
            "saved-item daemon did not start: status={status:?}, stderr={}",
            diagnostic.replace(token, "[redacted]")
        );
    }
    (server, client, url)
}

async fn real_index_job(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    expected: &Value,
) -> Value {
    use std::time::Duration;
    let (status, accepted) = real_api(
        client,
        url,
        token,
        reqwest::Method::POST,
        "/api/index",
        Some(serde_json::json!({"expectedRevision":expected})),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED, "{accepted}");
    let id = accepted["id"].as_str().unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let (status, job) = real_api(
                client,
                url,
                token,
                reqwest::Method::GET,
                &format!("/api/jobs/{id}"),
                None,
            )
            .await;
            assert_eq!(status, reqwest::StatusCode::OK, "{job}");
            if !job["finishedAt"].is_null() {
                break job;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("saved-item index job");
    assert_eq!(completed["state"], "done", "{completed}");
    completed["revision"].clone()
}

fn saved_pin_route(route: &str, pin: &Value) -> String {
    let separator = if route.contains('?') { '&' } else { '?' };
    format!(
        "{route}{separator}indexGeneration={}&indexRevision={}",
        pin["indexGeneration"].as_str().unwrap(),
        pin["indexRevision"].as_u64().unwrap()
    )
}

fn find_workspace_db(home: &std::path::Path) -> Option<std::path::PathBuf> {
    fn find(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == "workspace.db") {
                return Some(path);
            }
            if path.is_dir()
                && let Some(db) = find(&path)
            {
                return Some(db);
            }
        }
        None
    }
    find(home)
}

fn stored_payload(db_path: &std::path::Path, table: &str, id: &str) -> String {
    assert!(matches!(table, "views" | "annotations"));
    let db = rusqlite::Connection::open(db_path).unwrap();
    db.query_row(
        &format!("SELECT payload FROM {table} WHERE id=?1"),
        [id],
        |row| row.get(0),
    )
    .unwrap()
}

fn raw_anchor(payload: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        #[serde(default)]
        anchor: Option<Box<serde_json::value::RawValue>>,
    }
    serde_json::from_str::<Envelope>(payload)
        .unwrap()
        .anchor
        .map(|raw| raw.get().to_owned())
}

// Reverse the lossless Jev table encoding to check the real cached export against
// the separately read graph and preview, not only its HTTP status or packet ID.
fn decoded_jev_rows(export: &Value, table: &str) -> Vec<Value> {
    fn identity(cell: &Value, identities: &[Value]) -> Value {
        match cell {
            Value::Null => Value::Null,
            Value::Number(n) => identities[n.as_u64().unwrap() as usize].clone(),
            Value::Array(items) => {
                Value::Array(items.iter().map(|v| identity(v, identities)).collect())
            }
            _ => panic!("unexpected Jev identity cell: {cell}"),
        }
    }
    let state = &export["state"];
    let encoded = &state["packet"]["context"][table];
    let identities = state["identities"].as_array().unwrap();
    let columns = encoded["columns"].as_array().unwrap();
    let identity_columns = encoded["identityColumns"].as_array().unwrap();
    let range_columns = state["rangeColumns"].as_array().unwrap();
    encoded["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let cells = row.as_array().unwrap();
            assert_eq!(cells.len(), columns.len());
            let mut result = serde_json::Map::new();
            for (column, cell) in columns.iter().zip(cells) {
                let name = column.as_str().unwrap();
                let decoded = if identity_columns.iter().any(|id| id.as_str() == Some(name)) {
                    identity(cell, identities)
                } else if name == "range" {
                    let values = cell.as_array().unwrap();
                    assert_eq!(values.len(), range_columns.len());
                    Value::Object(
                        range_columns
                            .iter()
                            .zip(values)
                            .map(|(key, value)| (key.as_str().unwrap().to_owned(), value.clone()))
                            .collect(),
                    )
                } else {
                    cell.clone()
                };
                result.insert(name.to_owned(), decoded);
            }
            Value::Object(result)
        })
        .collect()
}

fn assert_native_version_witness(native: &Value, version_id: &Value, path: &str, language: &str) {
    let versions: Vec<_> = native["allRows"]["document_versions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row[0] == *version_id)
        .collect();
    assert_eq!(
        versions.len(),
        1,
        "one authenticated document version for {path}"
    );
    assert_eq!(versions[0][1], native["sourceSet"]["id"]);
    assert_eq!(versions[0][2], language);
    assert_eq!(versions[0][3], path);
    let manifests: Vec<_> = native["allRows"]["revision_documents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row[4] == *version_id && row[3] == path)
        .collect();
    assert_eq!(
        manifests.len(),
        1,
        "one admitted revision document for {path}"
    );
    assert_eq!(manifests[0][0], native["revision"]["id"]);
    assert_eq!(manifests[0][1], native["sourceSet"]["id"]);
    assert_eq!(manifests[0][2], language);
}

fn assert_fixture_declaration(
    native: &Value,
    symbol: &Value,
    path: &str,
    name: &str,
    kind: &str,
    owner: Option<&str>,
    source: &str,
) {
    let matching: Vec<_> = native["allRows"]["native_version_declarations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row[1] == symbol["id"])
        .collect();
    assert_eq!(matching.len(), 1, "one native declaration for {name}");
    let row = matching[0];
    assert_eq!(symbol["name"], name);
    assert_eq!(symbol["path"], path);
    assert_eq!(symbol["parent"], serde_json::json!(owner));
    let documents: Vec<_> = native["documents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|doc| doc["path"] == path)
        .collect();
    assert_eq!(documents.len(), 1, "one captured source for {path}");
    let language = documents[0]["language"].as_str().unwrap();
    assert_native_version_witness(native, &row[0], path, language);
    assert_eq!(row[2], serde_json::json!(owner), "{name}: native owner");
    assert_eq!(row[3], kind, "{name}: native declaration kind");
    assert_eq!(row[4], name, "{name}: native measured name");
    assert_eq!(
        row[10], symbol["range"]["startByte"],
        "{name}: native start"
    );
    assert_eq!(row[11], symbol["range"]["endByte"], "{name}: native end");
    let start = row[10].as_u64().unwrap() as usize;
    let end = row[11].as_u64().unwrap() as usize;
    assert!(
        source
            .get(start..end)
            .is_some_and(|text| text.contains(name)),
        "{name}: captured bytes must witness declaration"
    );
}

// Exercise the actual executable on both sides of the coordinator, not an in-process router.
#[tokio::test]
async fn real_cli_and_authenticated_daemon_share_native_pair_for_every_language_and_empty_root() {
    use std::{io::Read, os::unix::fs::PermissionsExt, time::Duration};
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for (name, file, source) in [
        (
            "java",
            "Flow.java",
            "class Flow { void seed() { sink(); } void sink() {} }
",
        ),
        (
            "javascript",
            "flow.js",
            "function seed() { sink(); } function sink() {}
",
        ),
        (
            "python",
            "flow.py",
            "def seed():
    sink()
def sink():
    pass
",
        ),
        (
            "rust",
            "flow.rs",
            "fn seed() { sink(); } fn sink() {}
",
        ),
        ("empty", "", ""),
    ] {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("workspace");
        let home = tmp.path().join("home");
        fs::create_dir(&root).unwrap();
        if !file.is_empty() {
            fs::write(root.join(file), source).unwrap();
        }
        let indexed = command(&root, &home, "index").output().unwrap();
        assert!(
            indexed.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&indexed.stderr)
        );
        let first: Value = serde_json::from_slice(&indexed.stdout).unwrap();
        let old_pin = first["publishedRevision"].clone();
        assert_eq!(first["status"]["revision"], old_pin, "{name}");
        let native_before = real_native_snapshot(&home);
        let graph_before = real_export(&root, &home);
        assert_eq!(
            native_before["documents"].as_array().unwrap().len(),
            usize::from(!file.is_empty()),
            "{name}"
        );

        let token_file = tmp.path().join("token");
        fs::write(&token_file, TOKEN).unwrap();
        fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let stderr_path = tmp.path().join("daemon-stderr");
        let mut server = Server(
            isolated_command(&home)
                .arg("serve")
                .arg("--workspace")
                .arg(&root)
                .arg("--bind")
                .arg(format!("127.0.0.1:{port}"))
                .arg("--token-file")
                .arg(&token_file)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::from(
                    fs::File::create(&stderr_path).unwrap(),
                ))
                .spawn()
                .unwrap(),
        );
        let url = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::new();
        let mut ready = false;
        for _ in 0..100 {
            if client
                .get(format!("{url}/healthz"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if !ready {
            let status = server.0.try_wait().unwrap();
            let mut diagnostic = String::new();
            fs::File::open(&stderr_path)
                .unwrap()
                .take(2048)
                .read_to_string(&mut diagnostic)
                .unwrap();
            let diagnostic = diagnostic.replace(TOKEN, "[redacted]");
            panic!("{name}: daemon did not start: status={status:?}, stderr={diagnostic}");
        }
        let post_start_status: Value = client
            .get(format!("{url}/api/status"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let post_start_pin = post_start_status["revision"].clone();
        assert_eq!(
            post_start_pin["indexGeneration"],
            old_pin["indexGeneration"]
        );
        assert!(
            post_start_pin["indexRevision"].as_u64().unwrap()
                > old_pin["indexRevision"].as_u64().unwrap()
        );
        let native_as_leader = real_native_snapshot(&home);
        let follower_status: Value =
            serde_json::from_slice(&command(&root, &home, "status").output().unwrap().stdout)
                .unwrap();
        assert_eq!(follower_status["revision"], post_start_pin, "{name}");
        let follower_symbols = command(&root, &home, "symbols").output().unwrap();
        assert!(follower_symbols.status.success(), "{name}");
        let follower_symbols: Value = serde_json::from_slice(&follower_symbols.stdout).unwrap();
        assert_eq!(follower_symbols["revision"], post_start_pin, "{name}");
        if let Some(follower_seed) = follower_symbols["items"]
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item["id"].as_str())
        {
            let follower_query = command(&root, &home, "query")
                .arg("--seed")
                .arg(follower_seed)
                .output()
                .unwrap();
            assert!(follower_query.status.success(), "{name}");
            let follower_query: Value = serde_json::from_slice(&follower_query.stdout).unwrap();
            assert_eq!(follower_query["revision"], post_start_pin, "{name}");
        }
        let follower_export = command(&root, &home, "export").output().unwrap();
        assert!(follower_export.status.success(), "{name}");
        assert_eq!(real_native_snapshot(&home), native_as_leader, "{name}");

        let follower_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let follower_port = follower_listener.local_addr().unwrap().port();
        drop(follower_listener);
        let follower_stderr = tmp.path().join("follower-daemon-stderr");
        let mut follower_server = Server(
            isolated_command(&home)
                .arg("serve")
                .arg("--workspace")
                .arg(&root)
                .arg("--bind")
                .arg(format!("127.0.0.1:{follower_port}"))
                .arg("--token-file")
                .arg(&token_file)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::from(
                    fs::File::create(&follower_stderr).unwrap(),
                ))
                .spawn()
                .unwrap(),
        );
        let follower_url = format!("http://127.0.0.1:{follower_port}");
        let mut follower_ready = false;
        for _ in 0..100 {
            if client
                .get(format!("{follower_url}/healthz"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                follower_ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(follower_ready, "{name}: follower daemon did not start");
        let follower_http_status: Value = client
            .get(format!("{follower_url}/api/status"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(follower_http_status["revision"], post_start_pin, "{name}");
        let follower_accepted = client
            .post(format!("{follower_url}/api/index"))
            .header("Origin", &follower_url)
            .bearer_auth(TOKEN)
            .json(&serde_json::json!({"expectedRevision":post_start_pin.clone()}))
            .send()
            .await
            .unwrap();
        assert_eq!(follower_accepted.status(), 202, "{name}");
        let follower_accepted: Value = follower_accepted.json().await.unwrap();
        assert_eq!(follower_accepted["state"], "queued", "{name}");
        assert!(follower_accepted["startedAt"].is_null(), "{name}");
        let follower_id = follower_accepted["id"].as_str().unwrap();
        let follower_done = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let job: Value = client
                    .get(format!("{follower_url}/api/jobs/{follower_id}"))
                    .bearer_auth(TOKEN)
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if !job["finishedAt"].is_null() {
                    break job;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(follower_done["state"], "done", "{name}: {follower_done}");
        let follower_pin = follower_done["revision"].clone();
        assert_eq!(
            follower_pin["indexRevision"],
            post_start_pin["indexRevision"].as_u64().unwrap() + 1
        );
        assert!(follower_server.0.try_wait().unwrap().is_none());
        let request = || {
            client
                .post(format!("{url}/api/index"))
                .header("Origin", &url)
                .json(&serde_json::json!({"expectedRevision":follower_pin.clone()}))
        };
        let denied = request().bearer_auth("incorrect").send().await.unwrap();
        assert_eq!(denied.status(), 401, "{name}");
        let invalid_origin = client
            .post(format!("{url}/api/index"))
            .header("Origin", "https://evil.example")
            .bearer_auth(TOKEN)
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(invalid_origin.status(), 403, "{name}");
        let current: Value = client
            .get(format!("{url}/api/jobs/current"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            current["id"], follower_id,
            "{name}: rejected auth/origin must not add work"
        );
        let accepted = request().bearer_auth(TOKEN).send().await.unwrap();
        assert_eq!(accepted.status(), 202, "{name}");
        let started: Value = accepted.json().await.unwrap();
        let id = started["id"].as_str().unwrap();
        let completed = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let job: Value = client
                    .get(format!("{url}/api/jobs/{id}"))
                    .bearer_auth(TOKEN)
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if !job["finishedAt"].is_null() {
                    break job;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(completed["state"], "done", "{name}: {completed}");
        let pin = completed["revision"].clone();
        assert_eq!(
            pin["indexGeneration"], post_start_pin["indexGeneration"],
            "{name}"
        );
        assert_eq!(
            pin["indexRevision"].as_u64().unwrap(),
            follower_pin["indexRevision"].as_u64().unwrap() + 1,
            "{name}"
        );
        let status: Value = client
            .get(format!("{url}/api/status"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(status["revision"], pin, "{name}");
        assert_eq!(
            status["stats"]["files"],
            usize::from(!file.is_empty()),
            "{name}"
        );
        use sha2::{Digest, Sha256};
        let native_after = real_native_snapshot(&home);
        let graph_after = real_export(&root, &home);
        let before_header = &native_before["allRows"]["native_revisions"][0];
        let leader_header = &native_as_leader["allRows"]["native_revisions"][0];
        let after_header = &native_after["allRows"]["native_revisions"][0];
        assert_ne!(
            before_header[0], after_header[0],
            "{name}: publication pin rotates"
        );
        assert_ne!(
            before_header[8], after_header[8],
            "{name}: daemon takeover rotates leader"
        );
        assert_eq!(
            leader_header[8], after_header[8],
            "{name}: active leader remains stable"
        );
        assert_ne!(
            before_header[14], after_header[14],
            "{name}: revision advances"
        );
        assert_eq!(after_header[14], pin["indexRevision"], "{name}");
        assert_eq!(
            after_header[0],
            format!(
                "pin:v1:{}:{}",
                pin["indexGeneration"].as_str().unwrap(),
                pin["indexRevision"].as_u64().unwrap()
            ),
            "{name}: header pin must match public status"
        );
        assert_eq!(
            same_bytes_evidence(&native_after),
            same_bytes_evidence(&native_before),
            "{name}: all 28 evidence tables and documents must match after only publication identity normalization"
        );
        assert_eq!(
            graph_after, graph_before,
            "{name}: identical bytes must reproduce graph"
        );
        let root_id =
            baleyg::store::topology::WorkspaceIdentity::discover_unattached(Some(&root), &root)
                .unwrap()
                .record_id;
        assert_eq!(native_after["sourceSet"]["rootId"], root_id, "{name}");
        assert_eq!(
            native_after["revision"]["sourceSetId"], native_after["sourceSet"]["id"],
            "{name}"
        );
        for field in ["toolchainHash", "configHash", "dependencyHash"] {
            assert_eq!(
                native_after["revision"][field].as_str().unwrap().len(),
                64,
                "{name}: {field}"
            );
        }
        let documents = native_after["documents"].as_array().unwrap();
        let graph_files = graph_after["files"].as_array().unwrap();
        assert_eq!(
            documents.len(),
            graph_files.len(),
            "{name}: every graph source needs native bytes"
        );
        for doc in documents {
            let path = doc["path"].as_str().unwrap();
            let graph_file = graph_files.iter().find(|row| row["path"] == path).unwrap();
            let bytes = graph_file["text"].as_str().unwrap().as_bytes();
            let digest = hex::encode(Sha256::digest(bytes));
            assert_eq!(
                doc["sourceSetId"], native_after["sourceSet"]["id"],
                "{name}"
            );
            assert_eq!(doc["revisionId"], native_after["revision"]["id"], "{name}");
            assert_eq!(doc["language"], graph_file["language"], "{name}");
            assert_eq!(doc["contentHash"], digest, "{name}");
            assert_eq!(doc["contentHash"], graph_file["hash"], "{name}");
            assert_eq!(doc["byteLength"], bytes.len(), "{name}");
            assert_eq!(doc["bytesHex"], hex::encode(bytes), "{name}");
            let route = format!(
                "/api/source?path={path}&indexGeneration={}&indexRevision={}",
                pin["indexGeneration"].as_str().unwrap(),
                pin["indexRevision"].as_u64().unwrap()
            );
            let (code, source_at) =
                real_api(&client, &url, TOKEN, reqwest::Method::GET, &route, None).await;
            assert_eq!(code, 200, "{name}: {source_at}");
            assert_eq!(source_at["revision"], pin, "{name}");
            assert_eq!(
                source_at["file"], *graph_file,
                "{name}: pinned source equals graph and native bytes"
            );
        }
        if !file.is_empty() {
            assert_eq!(documents[0]["path"], file, "{name}");
            assert_eq!(graph_files[0]["text"], source, "{name}");
            let pinned = format!(
                "indexGeneration={}&indexRevision={}",
                pin["indexGeneration"].as_str().unwrap(),
                pin["indexRevision"].as_u64().unwrap()
            );
            let measured = graph_after["nodes"].as_array().unwrap();
            let (code, classes) = real_api(
                &client,
                &url,
                TOKEN,
                reqwest::Method::GET,
                &format!("/api/classes?{pinned}"),
                None,
            )
            .await;
            assert_eq!(code, 200, "{name}: {classes}");
            assert_eq!(classes["revision"], pin, "{name}");
            let measured_classes: Vec<_> = measured
                .iter()
                .filter(|node| node["kind"] == "class")
                .collect();
            assert_eq!(
                measured_classes.len(),
                usize::from(name == "java"),
                "{name}: expected independently measured class set"
            );
            let class_owner = if name == "java" {
                Some(measured_classes[0]["id"].as_str().unwrap())
            } else {
                None
            };
            let returned_classes = classes["items"].as_array().unwrap();
            assert_eq!(
                returned_classes.len(),
                measured_classes.len(),
                "{name}: truthful class catalog"
            );
            for class in returned_classes {
                let symbol = &class["symbol"];
                assert!(
                    measured_classes.contains(&symbol),
                    "{name}: class must be a captured declaration: {class}"
                );
                assert_eq!(
                    symbol["name"], "Flow",
                    "{name}: only Java has a measured class"
                );
                assert_fixture_declaration(
                    &native_after,
                    symbol,
                    file,
                    "Flow",
                    "type",
                    None,
                    source,
                );
            }
            assert_eq!(
                returned_classes.len(),
                usize::from(name == "java"),
                "{name}"
            );
            let (code, symbols) = real_api(
                &client,
                &url,
                TOKEN,
                reqwest::Method::GET,
                "/api/symbols?q=seed",
                None,
            )
            .await;
            assert_eq!(code, 200, "{name}: {symbols}");
            assert_eq!(symbols["revision"], pin, "{name}");
            let seed = symbols["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["name"] == "seed")
                .expect("measured seed declaration")["id"]
                .as_str()
                .unwrap();
            let measured_seed = measured.iter().find(|node| node["id"] == seed).unwrap();
            assert_eq!(measured_seed["path"], file, "{name}");
            let callable_kind = if name == "java" { "method" } else { "function" };
            assert_fixture_declaration(
                &native_after,
                measured_seed,
                file,
                "seed",
                callable_kind,
                class_owner,
                source,
            );
            let (code, navigation) = real_api(
                &client,
                &url,
                TOKEN,
                reqwest::Method::POST,
                "/api/navigation",
                Some(serde_json::json!({"expectedRevision":pin,"path":file,"line":1})),
            )
            .await;
            assert_eq!(code, 200, "{name}: {navigation}");
            assert_eq!(navigation["revision"], pin, "{name}");
            let targets = navigation["targets"].as_array().unwrap();
            assert!(
                targets.iter().any(|target| target["symbol"]["id"] == seed),
                "{name}: source line must navigate to measured seed: {navigation}"
            );
            for target in targets {
                let symbol = &target["symbol"];
                assert_eq!(symbol["path"], file, "{name}");
                assert_eq!(target["matchKind"], "measured", "{name}");
                assert!(
                    measured.iter().any(|node| node == symbol),
                    "{name}: navigation only to captured symbol: {target}"
                );
                let target_name = symbol["name"].as_str().unwrap();
                match target_name {
                    "Flow" if name == "java" => assert_fixture_declaration(
                        &native_after,
                        symbol,
                        file,
                        "Flow",
                        "type",
                        None,
                        source,
                    ),
                    "seed" | "sink" => assert_fixture_declaration(
                        &native_after,
                        symbol,
                        file,
                        target_name,
                        callable_kind,
                        class_owner,
                        source,
                    ),
                    _ => panic!("{name}: unexpected navigation declaration {target}"),
                }
            }
            let (code, sequence) = real_api(
                &client,
                &url,
                TOKEN,
                reqwest::Method::POST,
                "/api/sequence",
                Some(serde_json::json!({"expectedRevision":pin,"seed":seed})),
            )
            .await;
            assert_eq!(code, 200, "{name}: {sequence}");
            assert_eq!(sequence["revision"], pin, "{name}");
            assert_eq!(
                sequence["seed"], *measured_seed,
                "{name}: sequence selected wrong seed"
            );
            let graph_calls = graph_after["calls"].as_array().unwrap();
            let native_calls = native_after["allRows"]["native_version_calls"]
                .as_array()
                .unwrap();
            assert_eq!(
                graph_calls.len(),
                1,
                "{name}: fixture has exactly one measured sink call"
            );
            assert_eq!(
                native_calls.len(),
                1,
                "{name}: exactly one native sink call"
            );
            let call = &graph_calls[0];
            let native_call = &native_calls[0];
            let expected_start = source.find("sink()").unwrap();
            let expected_end = expected_start + "sink()".len();
            assert_eq!(source.get(expected_start..expected_end), Some("sink()"));
            assert_eq!(call["caller"], seed, "{name}: measured call owner");
            assert_eq!(call["path"], file, "{name}: measured call path");
            assert_eq!(call["calleeText"], "sink", "{name}: measured call spelling");
            assert_eq!(call["range"]["startByte"], expected_start, "{name}");
            assert_eq!(call["range"]["endByte"], expected_end, "{name}");
            assert_native_version_witness(&native_after, &native_call[0], file, name);
            assert_eq!(
                native_call[1], call["id"],
                "{name}: same measured native call ID"
            );
            assert_eq!(native_call[2], seed, "{name}: native call owner");
            assert_eq!(native_call[4], expected_start, "{name}: native start byte");
            assert_eq!(native_call[5], expected_end, "{name}: native end byte");
            assert_eq!(native_call[8], "sink", "{name}: native callee spelling");
            fn measured_steps(
                steps: &[Value],
                file: &str,
                text: &str,
                calls: &[Value],
                native: &[Value],
                seen: &mut Vec<String>,
                flattened: &mut Vec<Value>,
            ) {
                for step in steps {
                    flattened.push(step.clone());
                    assert_eq!(step["path"], file, "sequence step path: {step}");
                    let start = step["range"]["startByte"].as_u64().unwrap() as usize;
                    let end = step["range"]["endByte"].as_u64().unwrap() as usize;
                    assert!(
                        start <= end && end <= text.len(),
                        "step outside captured source: {step}"
                    );
                    assert!(
                        step.get("target").is_none() && step.get("resolution").is_none(),
                        "syntax must never infer dispatch: {step}"
                    );
                    if let Some(id) = step["callId"].as_str() {
                        let call = calls
                            .iter()
                            .find(|call| call["id"] == id)
                            .expect("measured graph call");
                        assert_eq!(step["path"], call["path"]);
                        assert_eq!(step["range"], call["range"]);
                        assert!(
                            native.iter().any(|row| row[1] == id),
                            "native call ID absent: {id}"
                        );
                        seen.push(id.to_owned());
                    }
                    for branch in ["children", "alternate"] {
                        measured_steps(
                            step[branch].as_array().unwrap(),
                            file,
                            text,
                            calls,
                            native,
                            seen,
                            flattened,
                        );
                    }
                }
            }
            let mut seen = Vec::new();
            let mut flattened = Vec::new();
            measured_steps(
                sequence["steps"].as_array().unwrap(),
                file,
                source,
                graph_calls,
                native_calls,
                &mut seen,
                &mut flattened,
            );
            assert_eq!(
                seen,
                vec![call["id"].as_str().unwrap().to_owned()],
                "{name}: exactly one source-witnessed sink call and no duplicate/extra call IDs"
            );
            assert_eq!(
                flattened.len(),
                1,
                "{name}: no invented null-call/control steps: {sequence}"
            );
            let step = &flattened[0];
            assert_eq!(step["kind"], "call", "{name}");
            assert_eq!(step["callId"], call["id"], "{name}");
            assert_eq!(step["label"], "sink", "{name}");
            assert_eq!(step["path"], file, "{name}");
            assert_eq!(step["range"], call["range"], "{name}");
            assert_eq!(source.get(expected_start..expected_end), Some("sink()"));
            let expected_seed: baleyg::model::Symbol =
                serde_json::from_value(measured_seed.clone()).unwrap();
            let expected_file: baleyg::model::SourceFile =
                serde_json::from_value(graph_files[0].clone()).unwrap();
            let expected_calls: Vec<baleyg::model::CallSite> = graph_calls
                .iter()
                .map(|call| serde_json::from_value(call.clone()).unwrap())
                .collect();
            let expected_pin: baleyg::model::IndexPin =
                serde_json::from_value(pin.clone()).unwrap();
            let expected_sequence = baleyg::behavior::build_sequence(
                expected_pin,
                &expected_seed,
                &expected_file,
                &expected_calls,
                false,
            )
            .unwrap();
            assert_eq!(
                sequence["steps"],
                serde_json::json!(expected_sequence.steps),
                "{name}: no extra invented or null-call steps beyond captured source projection"
            );
            let (code, preview) = real_api(
                &client,
                &url,
                TOKEN,
                reqwest::Method::POST,
                "/api/questions/preview",
                Some(serde_json::json!({
                    "seed":seed,"question":"what happens?","expectedRevision":pin
                })),
            )
            .await;
            assert_eq!(code, 200, "{name}: {preview}");
            let preview_packet = &preview["packet"];
            assert_eq!(preview_packet["revision"], pin, "{name}");
            assert_eq!(preview_packet["request"]["expectedRevision"], pin, "{name}");
            assert_eq!(preview_packet["request"]["seed"], seed, "{name}");
            assert_eq!(preview_packet["context"]["revision"], pin, "{name}");
            assert_eq!(preview_packet["context"]["query"]["seed"], seed, "{name}");
            assert_eq!(
                preview_packet["context"]["nodes"],
                serde_json::json!([measured_seed]),
                "{name}: packet must contain the exact selected graph/native seed"
            );
            assert_eq!(
                preview_packet["sourceFiles"],
                serde_json::json!([graph_files[0]]),
                "{name}: packet must cite exact selected captured source"
            );
            let selected_calls = preview_packet["context"]["calls"].as_array().unwrap();
            assert_eq!(
                selected_calls.len(),
                1,
                "{name}: exactly one preview sink call"
            );
            assert_eq!(
                selected_calls[0], *call,
                "{name}: preview must cite the full measured native/graph call"
            );
            assert!(
                graph_after["regions"].as_array().unwrap().is_empty(),
                "{name}: simple seed-to-sink fixture has no control region"
            );
            assert_eq!(
                preview_packet["context"]["regions"], graph_after["regions"],
                "{name}: no fabricated preview control regions"
            );
            assert!(!preview.to_string().contains("lexical-guess"), "{name}");
            let packet = preview_packet["packetId"].as_str().unwrap();
            let (code, export) = real_api(
                &client,
                &url,
                TOKEN,
                reqwest::Method::GET,
                &format!("/api/questions/{packet}/jev-request"),
                None,
            )
            .await;
            assert_eq!(code, 200, "{name}: {export}");
            assert_eq!(
                export["state"]["identityPaths"],
                serde_json::json!(["/request/seed", "/context/query/seed"]),
                "{name}: canonical identity paths"
            );
            assert_eq!(
                export["state"]["rangeColumns"],
                serde_json::json!([
                    "startByte",
                    "endByte",
                    "startLine",
                    "startColumn",
                    "endLine",
                    "endColumn"
                ]),
                "{name}: canonical source range columns"
            );
            for (table, columns) in [
                ("nodes", serde_json::json!(["id", "parent"])),
                ("calls", serde_json::json!(["id", "caller", "regions"])),
                ("regions", serde_json::json!(["id", "parent", "owner"])),
            ] {
                assert_eq!(
                    export["state"]["packet"]["context"][table]["identityColumns"], columns,
                    "{name}: canonical {table} identity columns"
                );
            }
            let wire = &export["state"]["packet"];
            assert_eq!(
                wire["packetId"], packet,
                "{name}: cached export packet identity"
            );
            assert_eq!(wire["revision"], pin, "{name}");
            assert_eq!(wire["request"]["expectedRevision"], pin, "{name}");
            assert_eq!(wire["context"]["revision"], pin, "{name}");
            assert_eq!(
                wire["sourceFiles"], preview_packet["sourceFiles"],
                "{name}: cached source witness"
            );
            let identities = export["state"]["identities"].as_array().unwrap();
            for index in [
                wire["request"]["seed"].as_u64().unwrap(),
                wire["context"]["query"]["seed"].as_u64().unwrap(),
            ] {
                assert_eq!(
                    identities[index as usize], seed,
                    "{name}: encoded seed identity"
                );
            }
            let mut decoded = wire.clone();
            decoded["request"]["seed"] = serde_json::json!(seed);
            decoded["context"]["query"]["seed"] = serde_json::json!(seed);
            for table in ["nodes", "calls", "regions"] {
                let rows = decoded_jev_rows(&export, table);
                if table == "nodes" || table == "calls" {
                    assert!(
                        !rows.is_empty(),
                        "{name}: cached {table} witness cannot be empty"
                    );
                }
                decoded["context"][table] = serde_json::json!(rows);
            }
            assert_eq!(
                decoded, *preview_packet,
                "{name}: lossless cached export must equal the entire selected measured packet"
            );
            let question_keys: Vec<_> = export["questions"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            let mut expected_keys: Vec<_> = (0..selected_calls.len())
                .map(|i| format!("c{i}_{packet}"))
                .collect();
            expected_keys.sort();
            assert!(
                !expected_keys.is_empty(),
                "{name}: expected measured seed call questions"
            );
            assert_eq!(
                question_keys, expected_keys,
                "{name}: cached questions bind selected packet/calls"
            );
            assert!(!export.to_string().contains("lexical-guess"), "{name}");
        }
        let stale = request().bearer_auth(TOKEN).send().await.unwrap();
        assert_eq!(
            stale.status(),
            202,
            "{name}: stale request durably accepted"
        );
        let stale: Value = stale.json().await.unwrap();
        assert_eq!(
            stale["state"], "queued",
            "{name}: accepted before claim-time CAS"
        );
        let stale_id = stale["id"].as_str().unwrap();
        let failed = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let row: Value = client
                    .get(format!("{url}/api/jobs/{stale_id}"))
                    .bearer_auth(TOKEN)
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if !row["finishedAt"].is_null() {
                    break row;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(failed["state"], "failed", "{name}: {failed}");
        assert_eq!(failed["error"]["code"], "revision_conflict", "{name}");
        // The successful same-byte reindex advanced publication identity. A
        // stale request must leave that current, fully raw v8 pair untouched.
        assert_eq!(real_native_snapshot(&home), native_after, "{name}");
        let unchanged_status: Value = client
            .get(format!("{url}/api/status"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            unchanged_status["revision"], pin,
            "{name}: current pin changed"
        );
        drop(server);
    }
}

#[tokio::test]
async fn real_daemon_post_capture_failure_preserves_pair_source_graph_and_cached_packet() {
    use std::{os::unix::fs::PermissionsExt, time::Duration};
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    struct Writer(rusqlite::Connection);
    impl Drop for Writer {
        fn drop(&mut self) {
            let _ = self.0.execute_batch("ROLLBACK");
        }
    }
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("workspace");
    let home = tmp.path().join("home");
    fs::create_dir(&root).unwrap();
    let original = "function seed() { sink(); } function sink() {}
";
    fs::write(root.join("flow.js"), original).unwrap();
    let initial = command(&root, &home, "index").output().unwrap();
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    let token_file = tmp.path().join("token");
    fs::write(&token_file, TOKEN).unwrap();
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let stderr_path = tmp.path().join("daemon-stderr");
    let mut server = Server(
        isolated_command(&home)
            .arg("serve")
            .arg("--workspace")
            .arg(&root)
            .arg("--bind")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--token-file")
            .arg(&token_file)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(
                fs::File::create(&stderr_path).unwrap(),
            ))
            .spawn()
            .unwrap(),
    );
    let url = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap();
    let mut last_healthz = "not_observed".to_owned();
    let readiness = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            last_healthz = "request_in_flight".to_owned();
            match client.get(format!("{url}/healthz")).send().await {
                Ok(response) if response.status().is_success() => break,
                Ok(response) => {
                    last_healthz = format!("http_status_{}", response.status().as_u16())
                }
                Err(error) if error.is_timeout() => last_healthz = "request_timeout".to_owned(),
                Err(error) if error.is_connect() => last_healthz = "connect_error".to_owned(),
                Err(_) => last_healthz = "request_other_error".to_owned(),
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if readiness.is_err()
        && let Some(dir) = std::env::var_os("BALEYG_PRIVATE_DIAGNOSTICS_DIR")
    {
        let diagnostic = write_private_readiness_diagnostic(
            std::path::Path::new(&dir),
            &mut server.0,
            &stderr_path,
            format!("127.0.0.1:{port}").parse().unwrap(),
            &last_healthz,
        );
        eprintln!(
            "private readiness diagnostic {}",
            if diagnostic.is_ok() {
                "recorded"
            } else {
                "unavailable"
            }
        );
    }
    readiness.expect("real daemon readiness");
    let (code, status) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        "/api/status",
        None,
    )
    .await;
    assert_eq!(code, 200, "{status}");
    let pin = status["revision"].clone();
    let native_before = real_native_snapshot(&home);
    let graph_before = real_export(&root, &home);
    let pinned_source = format!(
        "/api/source?path=flow.js&indexGeneration={}&indexRevision={}",
        pin["indexGeneration"].as_str().unwrap(),
        pin["indexRevision"].as_u64().unwrap()
    );
    let (code, _source_before) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        &pinned_source,
        None,
    )
    .await;
    assert_eq!(code, 200, "{_source_before}");
    let (code, symbols) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        "/api/symbols?q=seed",
        None,
    )
    .await;
    assert_eq!(code, 200, "{symbols}");
    let seed = symbols["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "seed")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let (code, preview) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::POST,
        "/api/questions/preview",
        Some(serde_json::json!({"seed":seed,"question":"what happens?","expectedRevision":pin})),
    )
    .await;
    assert_eq!(code, 200, "{preview}");
    let packet_id = preview["packet"]["packetId"].as_str().unwrap();
    let packet_route = format!("/api/questions/{packet_id}/jev-request");
    let (code, _packet_before) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        &packet_route,
        None,
    )
    .await;
    assert_eq!(code, 200, "{_packet_before}");

    // A SQLite RESERVED writer allows HTTP admission and the complete source capture,
    // but denies the publisher its write lock after projection completes.
    let padding = "x".repeat(48 * 1024);
    for i in 0..80 {
        fs::write(
            root.join(format!("extra{i:03}.js")),
            format!("// {padding}\nfunction extra{i}() {{}}\n"),
        )
        .unwrap();
    }
    let writer = Writer(rusqlite::Connection::open(real_index_db(&home)).unwrap());
    writer.0.busy_timeout(Duration::from_secs(1)).unwrap();
    writer.0.execute_batch("BEGIN IMMEDIATE").unwrap();
    let (code, accepted) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::POST,
        "/api/index",
        Some(serde_json::json!({"expectedRevision":pin})),
    )
    .await;
    assert_eq!(code, 202, "{accepted}");
    let id = accepted["id"].as_str().unwrap();
    let terminal = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let (code, job) = real_api(
                &client,
                &url,
                TOKEN,
                reqwest::Method::GET,
                &format!("/api/jobs/{id}"),
                None,
            )
            .await;
            assert_eq!(code, 200, "{job}");
            if !job["finishedAt"].is_null() {
                break job;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("complete capture followed by publication contention");
    assert_eq!(terminal["state"], "failed", "{terminal}");
    assert_eq!(terminal["error"]["code"], "index_failed", "{terminal}");
    assert!(terminal["revision"].is_null(), "{terminal}");
    // Advisory progress is process-local while running; terminal rows use the
    // stable default rather than race a late in-memory update into GET/cancel.
    assert_eq!(
        terminal["progress"],
        serde_json::json!({"phase":"","completed":0,"total":0}),
        "{terminal}"
    );
    drop(writer); // Always rolls back the external writer lock, including on panic.
    let (generation, revision): (String, i64) = rusqlite::Connection::open(real_index_db(&home))
        .unwrap()
        .query_row(
            "SELECT index_generation,index_revision FROM index_metadata WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(generation, pin["indexGeneration"].as_str().unwrap());
    assert_eq!(
        revision,
        i64::try_from(pin["indexRevision"].as_u64().unwrap()).unwrap()
    );
    let (code, current) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        "/api/status",
        None,
    )
    .await;
    assert_eq!(code, 503, "{current}");
    assert_eq!(current["error"]["code"], "index_not_ready");
    assert_eq!(
        real_native_snapshot(&home),
        native_before,
        "all native rows remain unchanged"
    );
    assert_eq!(
        real_export(&root, &home),
        graph_before,
        "all graph rows remain unchanged"
    );
    let (code, source_after) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        &pinned_source,
        None,
    )
    .await;
    assert_eq!(code, 503, "{source_after}");
    assert_eq!(source_after["error"]["code"], "index_not_ready");
    let (code, packet_after) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        &packet_route,
        None,
    )
    .await;
    assert_eq!(code, 503, "{packet_after}");
    assert_eq!(packet_after["error"]["code"], "index_not_ready");
    let (code, retry) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::POST,
        "/api/index",
        Some(serde_json::json!({"expectedRevision":pin})),
    )
    .await;
    assert_eq!(code, 202, "{retry}");
    let retry_id = retry["id"].as_str().unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let (code, job) = real_api(
                &client,
                &url,
                TOKEN,
                reqwest::Method::GET,
                &format!("/api/jobs/{retry_id}"),
                None,
            )
            .await;
            assert_eq!(code, 200, "{job}");
            if !job["finishedAt"].is_null() {
                break job;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("later successful job");
    assert_eq!(completed["state"], "done", "{completed}");
    assert_eq!(
        completed["revision"]["indexGeneration"],
        pin["indexGeneration"]
    );
    assert_eq!(
        completed["revision"]["indexRevision"],
        pin["indexRevision"].as_u64().unwrap() + 1
    );
    use sha2::{Digest, Sha256};
    let new_pin = &completed["revision"];
    let (code, published) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        "/api/status",
        None,
    )
    .await;
    assert_eq!(code, 200, "{published}");
    assert_eq!(
        published["revision"], *new_pin,
        "retry must publish full new pin"
    );
    assert_eq!(published["stats"]["files"], 81);
    let (code, stale_packet) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        &packet_route,
        None,
    )
    .await;
    assert_eq!(code, 404, "{stale_packet}");
    assert_eq!(stale_packet["error"]["code"], "not_found");
    let native_after = real_native_snapshot(&home);
    let graph_after = real_export(&root, &home);
    assert_ne!(
        native_after, native_before,
        "retry must replace normalized evidence rows"
    );
    assert_ne!(graph_after, graph_before, "retry must replace graph rows");
    assert_eq!(
        native_after["sourceSet"], native_before["sourceSet"],
        "source set retains this root/language catalog"
    );
    assert_ne!(
        native_after["revision"]["id"], native_before["revision"]["id"],
        "added source bytes must create a new native revision"
    );
    assert_eq!(
        native_after["revision"]["sourceSetId"],
        native_after["sourceSet"]["id"]
    );
    let root_id =
        baleyg::store::topology::WorkspaceIdentity::discover_unattached(Some(&root), &root)
            .unwrap()
            .record_id;
    assert_eq!(native_after["sourceSet"]["rootId"], root_id);
    let documents = native_after["documents"].as_array().unwrap();
    let files = graph_after["files"].as_array().unwrap();
    assert_eq!(documents.len(), 81);
    assert_eq!(files.len(), 81);
    for doc in documents {
        let path = doc["path"].as_str().unwrap();
        let expected = if path == "flow.js" {
            original.to_owned()
        } else {
            let index = path
                .strip_prefix("extra")
                .and_then(|suffix| suffix.strip_suffix(".js"))
                .unwrap()
                .parse::<usize>()
                .unwrap();
            assert!(index < 80, "unexpected published source path: {path}");
            format!("// {padding}\nfunction extra{index}() {{}}\n")
        };
        let graph = files.iter().find(|file| file["path"] == path).unwrap();
        assert_eq!(graph["text"], expected, "{path}: retry graph source bytes");
        assert_eq!(graph["language"], "javascript", "{path}");
        assert_eq!(doc["language"], graph["language"], "{path}");
        assert_eq!(
            doc["sourceSetId"], native_after["sourceSet"]["id"],
            "{path}"
        );
        assert_eq!(doc["revisionId"], native_after["revision"]["id"], "{path}");
        assert_eq!(doc["byteLength"], expected.len(), "{path}");
        assert_eq!(doc["bytesHex"], hex::encode(expected.as_bytes()), "{path}");
        assert_eq!(
            doc["contentHash"],
            hex::encode(Sha256::digest(expected.as_bytes())),
            "{path}"
        );
        assert_eq!(doc["contentHash"], graph["hash"], "{path}");
        let route = format!(
            "/api/source?path={path}&indexGeneration={}&indexRevision={}",
            new_pin["indexGeneration"].as_str().unwrap(),
            new_pin["indexRevision"].as_u64().unwrap()
        );
        let (code, source) =
            real_api(&client, &url, TOKEN, reqwest::Method::GET, &route, None).await;
        assert_eq!(code, 200, "{path}: {source}");
        assert_eq!(
            source["revision"], *new_pin,
            "{path}: authenticated full pin"
        );
        assert_eq!(
            source["file"], *graph,
            "{path}: pinned source matches paired graph/native"
        );
    }
    for i in 0..80 {
        let path = format!("extra{i:03}.js");
        assert!(
            documents.iter().any(|doc| doc["path"] == path),
            "missing retry document: {path}"
        );
    }
    drop(server);
}

#[cfg(unix)]
#[test]
fn index_process_holds_leader_while_stdout_is_blocked() {
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("flow.js"), "function start() {}\n").unwrap();
    let identity =
        baleyg::store::topology::WorkspaceIdentity::discover(Some(&root), &root).unwrap();
    let index_dir = if cfg!(target_os = "macos") {
        home.join("Library/Caches/dev.odin.baleyg/indexes")
            .join(&identity.root_key)
    } else {
        home.join(".cache/baleyg/indexes").join(&identity.root_key)
    };

    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let mut reader = unsafe { File::from_raw_fd(fds[0]) };
    let mut writer = unsafe { File::from_raw_fd(fds[1]) };
    let flags = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let fill = [b'x'; 4096];
    let mut filler_bytes = 0usize;
    loop {
        match writer.write(&fill) {
            Ok(bytes) => filler_bytes += bytes,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("prefill stdout pipe: {error}"),
        }
    }
    assert!(filler_bytes > 0);
    assert_eq!(
        unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_SETFL, flags) },
        0
    );

    let stderr_path = temp.path().join("blocked-index-stderr");
    let mut child = ChildGuard(
        command(&root, &home, "index")
            .stdout(Stdio::from(writer))
            .stderr(Stdio::from(File::create(&stderr_path).unwrap()))
            .spawn()
            .unwrap(),
    );
    let db_path = index_dir.join("index.db");
    let journal_path = index_dir.join("index.db-journal");
    // SQLite immutable URI connections never take SH locks against the writer.
    // Percent-encode raw path bytes so %, ?, #, spaces and non-UTF8 cannot
    // change the URI query or select another database.
    let mut db_uri = String::from("file:");
    for &byte in db_path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"/._-~".contains(&byte) {
            db_uri.push(char::from(byte));
        } else {
            db_uri.push('%');
            db_uri.push_str(&format!("{byte:02X}"));
        }
    }
    db_uri.push_str("?immutable=1");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            panic!(
                "index exited before publication barrier ({status}): {}",
                fs::read_to_string(&stderr_path).unwrap_or_default()
            );
        }
        if db_path.exists() {
            // A partially initialized file or an unsupported WAL-mode header is
            // never a commit signal. This plain file read takes no SQLite lock.
            let delete_mode = File::open(&db_path)
                .and_then(|mut file| {
                    let mut header = [0u8; 20];
                    file.read_exact(&mut header)?;
                    Ok(&header[..16] == b"SQLite format 3\0" && header[18] == 1 && header[19] == 1)
                })
                .unwrap_or(false);
            if delete_mode
                && let Ok(db) = rusqlite::Connection::open_with_flags(
                    db_uri.as_str(),
                    rusqlite::OpenFlags::SQLITE_OPEN_URI
                        | rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
            {
                let published = db
                    .query_row(
                        "SELECT reconciled_incarnation IS NOT NULL,index_revision FROM index_metadata WHERE singleton=1",
                        [],
                        |row| Ok((row.get::<_, bool>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .is_ok_and(|(reconciled, revision)| reconciled && revision == 1);
                drop(db);
                // In this repository's enforced DELETE mode, removing the
                // rollback journal is the commit point, after database sync.
                // A dangling symlink or metadata error must not count as absent.
                let journal_absent = matches!(
                    fs::symlink_metadata(&journal_path),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound
                );
                if published && journal_absent {
                    break;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "index publication barrier timed out: {}",
            fs::read_to_string(&stderr_path).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "child escaped the full stdout pipe"
    );
    let leader_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(index_dir.join("leader.lock"))
        .unwrap();
    let locked = unsafe { libc::flock(leader_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_ne!(locked, 0, "index released leadership before writing stdout");

    let mut filler = vec![0; filler_bytes];
    reader.read_exact(&mut filler).unwrap();
    assert!(filler.iter().all(|byte| *byte == b'x'));
    let mut stdout = Vec::new();
    reader.read_to_end(&mut stdout).unwrap();
    assert!(child.0.wait().unwrap().success());
    let output: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(output["publishedRevision"]["indexRevision"], 1);
    assert_eq!(output["status"]["revision"]["indexRevision"], 1);
    assert_eq!(output["status"]["stats"]["files"], 1);
    assert_eq!(
        unsafe { libc::flock(leader_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    unsafe { libc::flock(leader_file.as_raw_fd(), libc::LOCK_UN) };
}

#[tokio::test]
async fn saved_items_real_index_matrix() {
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("workspace");
    let home = temp.path().join("home");
    fs::create_dir(&root).unwrap();
    let source_path = root.join("flow.rs");
    fs::write(
        &source_path,
        "fn seed(value: i32) { sink(); }\nfn sink() {}\nfn other() {}\n",
    )
    .unwrap();
    let indexed = command(&root, &home, "index").output().unwrap();
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    let indexed: Value = serde_json::from_slice(&indexed.stdout).unwrap();
    let initial_pin = indexed["status"]["revision"].clone();
    let (server, client, url) = start_saved_item_server(temp.path(), &root, &home, TOKEN).await;
    let (status, serving_status) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        "/api/status",
        None,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{serving_status}");
    let pin = serving_status["revision"].clone();
    assert_eq!(pin["indexGeneration"], initial_pin["indexGeneration"]);
    assert_eq!(
        pin["indexRevision"].as_u64().unwrap(),
        initial_pin["indexRevision"].as_u64().unwrap() + 1
    );

    for route in ["/api/views", "/api/views/absent", "/api/annotations"] {
        let (status, response) =
            real_api(&client, &url, TOKEN, reqwest::Method::GET, route, None).await;
        if route.ends_with("absent") {
            assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "{response}");
        } else {
            assert_eq!(status, reqwest::StatusCode::OK, "{response}");
            assert_eq!(response, serde_json::json!([]));
        }
    }
    assert!(
        find_workspace_db(&home).is_none(),
        "saved reads before first write must not create workspace.db"
    );

    let (status, symbols) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        "/api/symbols?q=",
        None,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{symbols}");
    let seed = symbols["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|symbol| symbol["name"] == "seed")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let other = symbols["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|symbol| symbol["name"] == "other")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let view_body = serde_json::json!({
        "id":"real-view","title":"Original","query":{"seed":seed}
    });
    let (status, saved_view) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::PUT,
        &saved_pin_route("/api/views/real-view", &pin),
        Some(view_body.clone()),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{saved_view}");
    assert_eq!(saved_view["indexGeneration"], pin["indexGeneration"]);
    assert_eq!(saved_view["indexRevision"], pin["indexRevision"]);
    assert_eq!(saved_view["view"]["query"]["seed"], seed);
    assert_eq!(saved_view["view"]["pins"], serde_json::json!({}));
    assert_eq!(saved_view["view"]["hidden"], serde_json::json!([]));
    assert_eq!(saved_view["attachment"]["availability"], "ready");
    assert_eq!(saved_view["attachment"]["result"]["status"], "attached");
    assert_eq!(saved_view["attachment"]["result"]["targetId"], seed);

    let note_body = serde_json::json!({
        "id":"real-note","nodeId":seed,"body":"First body","title":"First title"
    });
    let (status, saved_note) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::PUT,
        &saved_pin_route("/api/annotations/real-note", &pin),
        Some(note_body.clone()),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{saved_note}");
    assert_eq!(saved_note["indexGeneration"], pin["indexGeneration"]);
    assert_eq!(saved_note["indexRevision"], pin["indexRevision"]);
    assert_eq!(saved_note["annotation"]["nodeId"], seed);
    assert_eq!(saved_note["annotation"]["title"], "First title");
    assert_eq!(saved_note["attachment"]["availability"], "ready");
    assert_eq!(saved_note["attachment"]["result"]["targetId"], seed);

    let record_db = find_workspace_db(&home).expect("schema-1 saved record database");
    let record = rusqlite::Connection::open(&record_db).unwrap();
    let metadata: (i64, i64) = record
        .query_row(
            "SELECT schema_version,initialized FROM record_metadata WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(metadata, (1, 1), "saved payload is schema 1");
    let record_version: i64 = record
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(record_version, 1);
    drop(record);
    let original_view_payload = stored_payload(&record_db, "views", "real-view");
    let original_note_payload = stored_payload(&record_db, "annotations", "real-note");
    let original_view_anchor = raw_anchor(&original_view_payload).unwrap();
    let original_note_anchor = raw_anchor(&original_note_payload).unwrap();
    for (payload, target) in [
        (&original_view_payload, &seed),
        (&original_note_payload, &seed),
    ] {
        let value: Value = serde_json::from_str(payload).unwrap();
        let fields = value["anchor"].as_object().unwrap();
        let mut keys = fields.keys().map(String::as_str).collect::<Vec<_>>();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "capturedRevisionId",
                "document",
                "headerHash",
                "identicalHeaderCount",
                "siblingCount",
                "siblingGroupHash",
                "syntaxId",
            ]
        );
        assert_eq!(fields["syntaxId"], target.as_str());
        assert_eq!(fields["headerHash"].as_str().unwrap().len(), 64);
        assert_eq!(fields["siblingGroupHash"].as_str().unwrap().len(), 64);
    }
    let index_version: i64 = rusqlite::Connection::open(real_index_db(&home))
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(index_version, 8, "derived index inspection is separate");

    let edited_view_body = serde_json::json!({
        "id":"real-view","title":"Edited","query":{"seed":seed}
    });
    let (status, edited_view) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::PUT,
        &saved_pin_route("/api/views/real-view", &pin),
        Some(edited_view_body.clone()),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{edited_view}");
    assert_eq!(edited_view["view"]["anchor"], saved_view["view"]["anchor"]);
    let edited_note_body = serde_json::json!({
        "id":"real-note","nodeId":seed,"body":"Edited body"
    });
    let (status, edited_note) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::PUT,
        &saved_pin_route("/api/annotations/real-note", &pin),
        Some(edited_note_body.clone()),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{edited_note}");
    assert_eq!(edited_note["annotation"]["title"], "First title");
    assert_eq!(
        edited_note["annotation"]["anchor"],
        saved_note["annotation"]["anchor"]
    );
    assert_eq!(
        raw_anchor(&stored_payload(&record_db, "views", "real-view")).as_deref(),
        Some(original_view_anchor.as_str())
    );
    assert_eq!(
        raw_anchor(&stored_payload(&record_db, "annotations", "real-note")).as_deref(),
        Some(original_note_anchor.as_str())
    );

    for (route, body) in [
        (
            saved_pin_route("/api/views/real-view", &pin),
            serde_json::json!({
                "id":"real-view","title":"Injected","query":{"seed":seed},
                "anchor":saved_view["view"]["anchor"]
            }),
        ),
        (
            saved_pin_route("/api/annotations/real-note", &pin),
            serde_json::json!({
                "id":"real-note","nodeId":seed,"body":"Injected",
                "attachment":saved_note["attachment"]
            }),
        ),
        (
            saved_pin_route("/api/views/real-view", &pin),
            serde_json::json!({"id":"real-view","title":"Retarget","query":{"seed":other}}),
        ),
        (
            saved_pin_route("/api/annotations/real-note", &pin),
            serde_json::json!({"id":"real-note","nodeId":other,"body":"Retarget"}),
        ),
    ] {
        let (status, error) = real_api(
            &client,
            &url,
            TOKEN,
            reqwest::Method::PUT,
            &route,
            Some(body),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{error}");
    }
    assert_eq!(
        raw_anchor(&stored_payload(&record_db, "views", "real-view")).as_deref(),
        Some(original_view_anchor.as_str())
    );
    assert_eq!(
        raw_anchor(&stored_payload(&record_db, "annotations", "real-note")).as_deref(),
        Some(original_note_anchor.as_str())
    );

    fs::write(
        &source_path,
        "fn seed(value: i64) { sink(); }\nfn sink() {}\nfn other() {}\n",
    )
    .unwrap();
    let next = real_index_job(&client, &url, TOKEN, &pin).await;
    assert_eq!(next["indexGeneration"], pin["indexGeneration"]);
    assert_eq!(
        next["indexRevision"].as_u64().unwrap(),
        pin["indexRevision"].as_u64().unwrap() + 1
    );

    let routes = [
        saved_pin_route("/api/views/real-view", &next),
        saved_pin_route("/api/views", &next),
        saved_pin_route("/api/annotations", &next),
    ];
    for route in routes {
        let (status, response) =
            real_api(&client, &url, TOKEN, reqwest::Method::GET, &route, None).await;
        assert_eq!(status, reqwest::StatusCode::OK, "{route}: {response}");
        let state = if route.contains("/real-view") {
            &response
        } else if route.contains("annotations") {
            &response[0]
        } else {
            response
                .as_array()
                .unwrap()
                .iter()
                .find(|state| state["view"]["id"] == "real-view")
                .unwrap()
        };
        assert_eq!(state["indexGeneration"], next["indexGeneration"]);
        assert_eq!(state["indexRevision"], next["indexRevision"]);
        assert_eq!(state["attachment"]["availability"], "ready");
        assert_eq!(state["attachment"]["result"]["status"], "orphaned");
        assert_eq!(state["attachment"]["result"]["reason"], "headerMismatch");
        assert!(state["attachment"]["result"]["targetId"].is_null());
        if route.contains("annotations") {
            assert_eq!(state["annotation"]["nodeId"], seed);
            assert!(state["orphaned"].as_bool().unwrap());
        } else {
            assert_eq!(state["view"]["query"]["seed"], seed);
            assert!(
                state["orphanedIds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|id| id == &serde_json::json!(seed))
            );
        }
    }
    assert_eq!(
        raw_anchor(&stored_payload(&record_db, "views", "real-view")).as_deref(),
        Some(original_view_anchor.as_str())
    );
    assert_eq!(
        raw_anchor(&stored_payload(&record_db, "annotations", "real-note")).as_deref(),
        Some(original_note_anchor.as_str())
    );

    for route in [
        saved_pin_route("/api/views/real-view", &pin),
        saved_pin_route("/api/views", &pin),
        saved_pin_route("/api/annotations", &pin),
    ] {
        let (status, response) =
            real_api(&client, &url, TOKEN, reqwest::Method::GET, &route, None).await;
        assert_eq!(status, reqwest::StatusCode::OK, "{route}: {response}");
        let selected = if route.contains("/real-view") {
            &response
        } else {
            &response[0]
        };
        assert_eq!(selected["indexGeneration"], pin["indexGeneration"]);
        assert_eq!(selected["indexRevision"], pin["indexRevision"]);
        assert_eq!(selected["attachment"]["result"]["status"], "attached");
        assert_eq!(selected["attachment"]["result"]["targetId"], seed);
    }
    for (route, body) in [
        (
            saved_pin_route("/api/views/real-view", &pin),
            edited_view_body,
        ),
        (
            saved_pin_route("/api/annotations/real-note", &pin),
            edited_note_body,
        ),
    ] {
        let (status, error) = real_api(
            &client,
            &url,
            TOKEN,
            reqwest::Method::PUT,
            &route,
            Some(body),
        )
        .await;
        assert_eq!(status, reqwest::StatusCode::CONFLICT, "{error}");
    }

    let query = serde_json::json!({"seed":seed});
    let (status, stale_query) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::POST,
        &saved_pin_route("/api/query", &pin),
        Some(query.clone()),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{stale_query}");
    assert_eq!(stale_query["revision"], pin, "retained query must select P");
    let (status, current_query) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::POST,
        &saved_pin_route("/api/query", &next),
        Some(query.clone()),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{current_query}");
    assert_eq!(current_query["revision"], next);
    assert_eq!(current_query["nodes"][0]["id"], seed);
    let (status, ordinary_query) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::POST,
        "/api/query",
        Some(query),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{ordinary_query}");
    assert_eq!(ordinary_query["revision"], next);

    let orphan_view_edit = serde_json::json!({
        "id":"real-view","title":"Edited while orphaned","query":{"seed":seed}
    });
    let (status, orphan_view) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::PUT,
        &saved_pin_route("/api/views/real-view", &next),
        Some(orphan_view_edit),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{orphan_view}");
    assert_eq!(
        orphan_view["attachment"]["result"]["reason"],
        "headerMismatch"
    );
    let orphan_note_edit = serde_json::json!({
        "id":"real-note","nodeId":seed,"body":"Edited while orphaned","title":"New title"
    });
    let (status, orphan_note) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::PUT,
        &saved_pin_route("/api/annotations/real-note", &next),
        Some(orphan_note_edit),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{orphan_note}");
    assert_eq!(
        orphan_note["attachment"]["result"]["reason"],
        "headerMismatch"
    );
    assert_eq!(
        raw_anchor(&stored_payload(&record_db, "views", "real-view")).as_deref(),
        Some(original_view_anchor.as_str())
    );
    assert_eq!(
        raw_anchor(&stored_payload(&record_db, "annotations", "real-note")).as_deref(),
        Some(original_note_anchor.as_str())
    );

    drop(server);
    let db = rusqlite::Connection::open(&record_db).unwrap();
    db.execute(
        "INSERT INTO views(id,payload) VALUES(?1,?2)",
        rusqlite::params![
            "legacy-view",
            serde_json::json!({
                "id":"legacy-view","title":"Legacy","query":{"seed":seed},
                "pins":{},"hidden":[]
            })
            .to_string()
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO annotations(id,node_id,payload) VALUES(?1,?2,?3)",
        rusqlite::params![
            "legacy-note",
            seed,
            serde_json::json!({"id":"legacy-note","nodeId":seed,"body":"Legacy body"}).to_string()
        ],
    )
    .unwrap();
    drop(db);

    let (server, client, url) = start_saved_item_server(temp.path(), &root, &home, TOKEN).await;
    let (status, reopened_status) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        "/api/status",
        None,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{reopened_status}");
    let reopened_pin = reopened_status["revision"].clone();
    assert_eq!(reopened_pin["indexGeneration"], next["indexGeneration"]);
    assert_eq!(
        reopened_pin["indexRevision"].as_u64().unwrap(),
        next["indexRevision"].as_u64().unwrap() + 1
    );
    let (status, views) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        &saved_pin_route("/api/views", &reopened_pin),
        None,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{views}");
    let reopened = views
        .as_array()
        .unwrap()
        .iter()
        .find(|state| state["view"]["id"] == "real-view")
        .unwrap();
    assert_eq!(reopened["view"]["title"], "Edited while orphaned");
    assert_eq!(reopened["attachment"]["result"]["reason"], "headerMismatch");
    let legacy_view = views
        .as_array()
        .unwrap()
        .iter()
        .find(|state| state["view"]["id"] == "legacy-view")
        .unwrap();
    assert!(legacy_view["view"].get("anchor").is_none());
    assert_eq!(legacy_view["attachment"]["availability"], "anchorless");
    let (status, notes) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::GET,
        &saved_pin_route("/api/annotations", &reopened_pin),
        None,
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{notes}");
    let reopened_note = notes
        .as_array()
        .unwrap()
        .iter()
        .find(|state| state["annotation"]["id"] == "real-note")
        .unwrap();
    assert_eq!(reopened_note["annotation"]["title"], "New title");
    assert_eq!(reopened_note["annotation"]["body"], "Edited while orphaned");
    let legacy_note = notes
        .as_array()
        .unwrap()
        .iter()
        .find(|state| state["annotation"]["id"] == "legacy-note")
        .unwrap();
    assert!(legacy_note["annotation"].get("anchor").is_none());
    assert!(legacy_note["annotation"].get("title").is_none());
    assert_eq!(legacy_note["attachment"]["availability"], "anchorless");
    assert!(legacy_note["orphaned"].as_bool().unwrap());

    let legacy_view_edit = serde_json::json!({
        "id":"legacy-view","title":"Legacy edited","query":{"seed":seed}
    });
    let (status, legacy_view) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::PUT,
        &saved_pin_route("/api/views/legacy-view", &reopened_pin),
        Some(legacy_view_edit),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{legacy_view}");
    assert!(legacy_view["view"].get("anchor").is_none());
    assert_eq!(legacy_view["attachment"]["availability"], "anchorless");
    let legacy_note_edit = serde_json::json!({
        "id":"legacy-note","nodeId":seed,"body":"Legacy edited","title":"Legacy title"
    });
    let (status, legacy_note) = real_api(
        &client,
        &url,
        TOKEN,
        reqwest::Method::PUT,
        &saved_pin_route("/api/annotations/legacy-note", &reopened_pin),
        Some(legacy_note_edit),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "{legacy_note}");
    assert!(legacy_note["annotation"].get("anchor").is_none());
    assert_eq!(legacy_note["annotation"]["title"], "Legacy title");
    assert_eq!(legacy_note["attachment"]["availability"], "anchorless");
    assert!(raw_anchor(&stored_payload(&record_db, "views", "legacy-view")).is_none());
    assert!(raw_anchor(&stored_payload(&record_db, "annotations", "legacy-note")).is_none());
    drop(server);
}

#[test]
fn optional_captured_scip_changes_presentation_without_native_identity_or_full_rewrite_breakage() {
    use sha2::{Digest, Sha256};
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    let home = temp.path().join("home");
    let ingress = temp.path().join("ingress");
    let other = temp.path().join("other");
    for dir in [&root, &ingress, &other] {
        fs::create_dir(dir).unwrap();
    }
    let source = "function f() {}\nf();\n";
    fs::write(root.join("main.js"), source).unwrap();
    let first = command(&root, &home, "index")
        .current_dir(&ingress)
        .arg("--scip")
        .arg("index.scip")
        .arg("--manifest")
        .arg("manifest.json")
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_pin: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first_pin["publishedRevision"]["indexRevision"], 1);
    let original = command(&root, &home, "export")
        .current_dir(&other)
        .output()
        .unwrap();
    assert!(original.status.success());
    let original: Value = serde_json::from_slice(&original.stdout).unwrap();
    let first_symbol = original["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == "f")
        .unwrap();
    let first_id = first_symbol["id"].clone();
    assert!(first_symbol["displayLabel"].is_null());
    let hash = hex::encode(Sha256::digest(source.as_bytes()));
    write_optional_presentation(&ingress, "scip npm display 1 main.js/f().", &hash);
    // An unrelated cwd never becomes the base of the recorded optional inputs.
    write_optional_presentation(&other, "scip npm display 1 other().", "stale");
    let changed = command(&root, &home, "export")
        .current_dir(&other)
        .output()
        .unwrap();
    assert!(
        changed.status.success(),
        "{}",
        String::from_utf8_lossy(&changed.stderr)
    );
    let changed: Value = serde_json::from_slice(&changed.stdout).unwrap();
    let next = changed["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == "f")
        .unwrap();
    assert_eq!(next["id"], first_id);
    assert_eq!(next["displayLabel"], "scip npm display 1 main.js/f().");
    assert_eq!(
        changed["files"][0]["text"].as_str().unwrap().as_bytes(),
        source.as_bytes()
    );
    let status = command(&root, &home, "status")
        .current_dir(&other)
        .output()
        .unwrap();
    assert!(status.status.success());
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    // Full-rewrite staging publishes per CLI command: index r1, original export r2,
    // changed export r3, this status r4, stale export r5, latest status r6.
    assert_eq!(status["revision"]["indexRevision"], 4);
    assert_eq!(
        status["revision"]["indexGeneration"],
        first_pin["publishedRevision"]["indexGeneration"]
    );
    write_optional_presentation(&ingress, "scip npm display 1 main.js/f().", "bad-hash");
    let stale = command(&root, &home, "export")
        .current_dir(&other)
        .output()
        .unwrap();
    assert!(
        stale.status.success(),
        "{}",
        String::from_utf8_lossy(&stale.stderr)
    );
    let stale: Value = serde_json::from_slice(&stale.stdout).unwrap();
    let symbol = stale["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == "f")
        .unwrap();
    assert_eq!(symbol["id"], first_id);
    assert!(symbol["displayLabel"].is_null());
    let latest = command(&root, &home, "status")
        .current_dir(&other)
        .output()
        .unwrap();
    assert!(latest.status.success());
    let latest: Value = serde_json::from_slice(&latest.stdout).unwrap();
    assert_eq!(latest["revision"]["indexRevision"], 6);
}
