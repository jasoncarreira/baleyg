use serde_json::Value;
use std::{fs, process::Command};
use tempfile::TempDir;
fn command(root: &std::path::Path, state: &std::path::Path, sub: &str) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_baleyg"));
    c.arg(sub)
        .arg("--workspace")
        .arg(root)
        .arg("--state-dir")
        .arg(state);
    c
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
    let output = command(&root, &state, "index").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"]["stats"]["files"], 1);
    assert_eq!(result["status"]["stats"]["semanticState"], "unavailable");
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
    assert_eq!(view["calls"][0]["resolution"], "unresolved");
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
