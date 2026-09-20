use baleyg::{
    indexer::{IndexOptions, index_workspace},
    model::*,
};
use protobuf::Message;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};
fn cancel() -> CancelFlag {
    Arc::new(AtomicBool::new(false))
}
fn run(options: &IndexOptions) -> Graph {
    index_workspace(options, &cancel(), |_| {}).unwrap()
}
fn write(root: &Path, path: &str, text: &str) {
    fs::write(root.join(path), text).unwrap();
}
fn hash(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}
fn fixture(name: &str) -> IndexOptions {
    IndexOptions::new(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/extraction")
            .join(name),
    )
}
#[test]
fn pure_fixture_and_determinism() {
    let o = fixture("fixture");
    let g = run(&o);
    assert_eq!(g.stats.semantic_state, SemanticState::Unavailable);
    assert_eq!(g.calls.len(), 12);
    assert_eq!(g.stats.parse_error_files, 0);
    assert_eq!(g, run(&o));
    assert!(g.calls.iter().all(|c| c.target.is_none()));
    let owner = g.nodes.iter().find(|n| n.name == "repeated").unwrap();
    let calls: Vec<_> = g.calls.iter().filter(|c| c.caller == owner.id).collect();
    assert_eq!(calls.len(), 2);
    assert_ne!(calls[0].id, calls[1].id);
    assert_eq!((calls[0].ordinal, calls[1].ordinal), (1, 2));
    let owner = g.nodes.iter().find(|n| n.name == "branchLoop").unwrap();
    let kinds: Vec<Vec<_>> = g
        .calls
        .iter()
        .filter(|c| c.caller == owner.id)
        .map(|c| {
            c.regions
                .iter()
                .map(|id| {
                    g.regions
                        .iter()
                        .find(|r| &r.id == id)
                        .unwrap()
                        .kind
                        .as_str()
                })
                .collect()
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            vec!["if", "if"],
            vec!["if", "else"],
            vec!["else"],
            vec!["loop"]
        ]
    );
}
#[test]
fn discovery_ignores_secrets_builds_and_symlinks() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    write(p, ".gitignore", "ignored.js\nignored/\n");
    for name in [
        "keep.js",
        "keep.mjs",
        "keep.cjs",
        "ignored.js",
        ".secret.js",
        ".env",
    ] {
        write(p, name, "f()");
    }
    for name in [
        "node_modules",
        "dist",
        "build",
        "target",
        ".baleyg",
        "ignored",
    ] {
        fs::create_dir(p.join(name)).unwrap();
        write(p, &format!("{name}/hidden.js"), "f()");
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(p.join("keep.js"), p.join("linked.js")).unwrap();
        std::os::unix::fs::symlink(p, p.join("linked-dir")).unwrap();
    }
    let g = run(&IndexOptions::new(p.to_owned()));
    assert_eq!(
        g.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        ["keep.cjs", "keep.js", "keep.mjs"]
    );
}
#[test]
fn callback_and_computed_method_boundaries() {
    let g = run(&fixture("edge-fixture"));
    let owner = |id: &str| g.nodes.iter().find(|n| n.id == id).unwrap();
    let key = g.calls.iter().find(|c| c.callee_text == "key").unwrap();
    assert_eq!(owner(&key.caller).kind, SymbolKind::Module);
    let callbacks: Vec<_> = g
        .calls
        .iter()
        .filter(|c| owner(&c.caller).name == "callbacks")
        .collect();
    assert_eq!(callbacks.len(), 2);
    assert_eq!(callbacks[0].callback_arguments.len(), 1);
    assert!(
        g.calls
            .iter()
            .any(|c| c.caller == callbacks[0].callback_arguments[0])
    );
    let branch: Vec<_> = g
        .calls
        .iter()
        .filter(|c| owner(&c.caller).name == "branching")
        .collect();
    let kinds = |c: &CallSite| {
        c.regions
            .iter()
            .map(|id| {
                g.regions
                    .iter()
                    .find(|r| &r.id == id)
                    .unwrap()
                    .kind
                    .as_str()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(kinds(branch[0]), ["conditional-true"]);
    assert_eq!(kinds(branch[1]), ["conditional-false"]);
    assert_eq!(kinds(branch[2]), ["short-circuit"]);
    assert!(branch[3].regions.is_empty());
    assert_eq!(kinds(branch[4]), ["loop"]);
}
fn semantic_fixture() -> (tempfile::TempDir, IndexOptions) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    let text = "function f() {}\nconst emoji = '😀'; f();\n";
    write(p, "main.js", text);
    let mut doc = scip::types::Document::new();
    doc.relative_path = "main.js".into();
    // TS SCIP emits UTF16 offsets, even with UTF8 text_document_encoding.
    for (range, roles) in [(vec![0, 9, 10], 1), (vec![1, 20, 21], 0)] {
        let mut o = scip::types::Occurrence::new();
        o.range = range;
        o.symbol_roles = roles;
        o.symbol = "scip-typescript npm test 1 main.js/f().".into();
        doc.occurrences.push(o);
    }
    let mut index = scip::types::Index::new();
    index.documents.push(doc);
    fs::write(p.join("index.scip"), index.write_to_bytes().unwrap()).unwrap();
    fs::write(
        p.join("index.hashes.json"),
        serde_json::to_vec(&serde_json::json!({"main.js":hash(text)})).unwrap(),
    )
    .unwrap();
    let mut o = IndexOptions::new(p.to_owned());
    o.scip_path = Some(p.join("index.scip"));
    o.manifest_path = Some(p.join("index.hashes.json"));
    (d, o)
}
#[test]
fn utf16_occurrences_match_native_byte_offsets() {
    let (_d, o) = semantic_fixture();
    let g = run(&o);
    assert_eq!(g.stats.semantic_state, SemanticState::Fresh);
    assert_eq!(g.calls.len(), 1);
    assert_eq!(g.calls[0].resolution, Resolution::Internal);
    assert_eq!(
        g.calls[0].target,
        g.nodes.iter().find(|n| n.name == "f").map(|n| n.id.clone())
    );
    assert_eq!(g.calls[0].range.start_column, 23);
}
#[test]
fn semantic_requires_manifest_and_invalidates_on_all_input_drift() {
    for scenario in ["add", "delete", "source", "config", "manifest", "scip"] {
        let (_d, mut o) = semantic_fixture();
        let p = &o.workspace_root;
        match scenario {
            "add" => write(p, "new.mjs", "f()"),
            "delete" => fs::remove_file(p.join("main.js")).unwrap(),
            "source" => write(p, "main.js", "function f() {} f();"),
            "config" => write(p, "package.json", "{}"),
            "manifest" => o.manifest_path = None,
            "scip" => write(p, "index.scip", "invalid binary"),
            _ => unreachable!(),
        }
        let g = run(&o);
        assert_ne!(g.stats.semantic_state, SemanticState::Fresh, "{scenario}");
        assert!(g.calls.iter().all(|c| c.target.is_none()), "{scenario}");
    }
}
#[test]
fn cancellation_and_size_limits() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "large.js", "function f() { g(); }");
    let mut o = IndexOptions::new(d.path().to_owned());
    let c = Arc::new(AtomicBool::new(true));
    assert!(
        index_workspace(&o, &c, |_| {})
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    let c = cancel();
    assert!(
        index_workspace(&o, &c, |p| if p.phase == "scan" {
            c.store(true, std::sync::atomic::Ordering::Relaxed)
        })
        .is_err()
    );
    o.max_file_bytes = 2;
    let g = run(&o);
    assert!(g.files.is_empty());
    assert!(g.diagnostics.iter().any(|d| d.code == "source-skipped"));
}
#[test]
fn checked_local_scip_regression_when_present() {
    // Generated indexes are optional; all required semantic tests above generate their own protobuf.
    for name in ["fixture", "edge-fixture"] {
        let mut o = fixture(name);
        let base = o.workspace_root.parent().unwrap();
        let scip = base.join(format!("{name}.scip"));
        let manifest = base.join(format!("{name}.hashes.json"));
        if !scip.is_file() || !manifest.is_file() {
            continue;
        }
        o.scip_path = Some(scip);
        o.manifest_path = Some(manifest);
        let g = run(&o);
        assert_eq!(g.stats.semantic_state, SemanticState::Fresh);
        if name == "fixture" {
            assert_eq!(g.stats.internal, 11);
            assert_eq!(g.stats.unresolved, 1);
        } else {
            let c = g.calls.iter().find(|c| c.callee_text == "obj.f").unwrap();
            assert_eq!(c.resolution, Resolution::Unresolved);
            let owner = g.nodes.iter().find(|n| n.name == "callbacks").unwrap();
            assert!(
                g.calls
                    .iter()
                    .filter(|c| c.caller == owner.id)
                    .all(|c| c.callback_arguments.len() == 1)
            );
        }
    }
}

#[test]
fn nested_calls_have_unique_ids_and_parse_errors_are_visible() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "nested.js", "factory()(); new (factory())();\n");
    write(d.path(), "broken.js", "function broken( {\n");
    let g = run(&IndexOptions::new(d.path().to_owned()));
    let ids: std::collections::BTreeSet<_> = g.calls.iter().map(|c| &c.id).collect();
    assert_eq!(ids.len(), g.calls.len());
    assert_eq!(g.calls.len(), 4);
    assert_eq!(g.stats.parse_error_files, 1);
    assert!(g.diagnostics.iter().any(|d| d.code == "parse-error"));
}
#[test]
fn semantic_candidates_distinguish_external_ambiguous_and_noncall_refs() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    let text = "function f() {}\nf(); external(); maybe(); f;\n";
    write(p, "main.js", text);
    let mut doc = scip::types::Document::new();
    doc.relative_path = "main.js".into();
    for (range, roles, symbol) in [
        (vec![0, 9, 10], 1, "scip npm x 1 main.js/f()."),
        (vec![1, 0, 1], 0, "scip npm x 1 main.js/f()."),
        (vec![1, 5, 13], 0, "scip npm lib 1 external()."),
        (vec![1, 17, 22], 0, "scip npm x 1 main.js/f()."),
        (vec![1, 17, 22], 0, "scip npm lib 1 other()."),
        (vec![1, 26, 27], 0, "scip npm x 1 main.js/f()."),
    ] {
        let mut o = scip::types::Occurrence::new();
        o.range = range;
        o.symbol_roles = roles;
        o.symbol = symbol.into();
        doc.occurrences.push(o);
    }
    let mut index = scip::types::Index::new();
    index.documents.push(doc);
    fs::write(p.join("index.scip"), index.write_to_bytes().unwrap()).unwrap();
    fs::write(
        p.join("index.hashes.json"),
        serde_json::to_vec(&serde_json::json!({"main.js":hash(text)})).unwrap(),
    )
    .unwrap();
    let mut o = IndexOptions::new(p.to_owned());
    o.scip_path = Some(p.join("index.scip"));
    o.manifest_path = Some(p.join("index.hashes.json"));
    let g = run(&o);
    assert_eq!(g.calls.len(), 3);
    assert_eq!(
        g.calls.iter().map(|c| c.resolution).collect::<Vec<_>>(),
        [
            Resolution::Internal,
            Resolution::External,
            Resolution::Ambiguous
        ]
    );
    assert!(g.calls[2].target.is_none());
}

#[test]
fn lockfile_drift_invalidates_semantics() {
    for lock in [
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "bun.lock",
        "bun.lockb",
    ] {
        let (_d, o) = semantic_fixture();
        write(&o.workspace_root, lock, "changed");
        let g = run(&o);
        assert_eq!(g.stats.semantic_state, SemanticState::Stale);
        assert_eq!(g.stats.changed_files, [lock]);
        assert!(g.calls.iter().all(|c| c.target.is_none()));
    }
}
#[test]
fn recovered_parse_errors_do_not_inherit_semantic_evidence() {
    let (_d, o) = semantic_fixture();
    let p = &o.workspace_root;
    let mut text = fs::read_to_string(p.join("main.js")).unwrap();
    text.push_str("function broken(\n");
    write(p, "main.js", &text);
    fs::write(
        p.join("index.hashes.json"),
        serde_json::to_vec(&serde_json::json!({"main.js":hash(&text)})).unwrap(),
    )
    .unwrap();
    let g = run(&o);
    assert_eq!(g.stats.parse_error_files, 1);
    assert!(g.calls.iter().all(|c| c.target.is_none()
        && c.candidate_symbols.is_empty()
        && c.provenance.semantic == SemanticState::Unavailable));
    assert!(
        g.nodes
            .iter()
            .all(|n| n.provenance.semantic == SemanticState::Unavailable)
    );
}
#[cfg(unix)]
#[test]
fn direct_symlink_artifacts_and_config_are_never_read() {
    let (_d, mut o) = semantic_fixture();
    let p = &o.workspace_root;
    std::os::unix::fs::symlink(p.join("main.js"), p.join("package.json")).unwrap();
    let g = run(&o);
    assert_eq!(g.stats.semantic_state, SemanticState::Stale);
    assert!(g.diagnostics.iter().any(|d| d.code == "config-skipped"));
    std::os::unix::fs::symlink(p.join("index.scip"), p.join("linked.scip")).unwrap();
    o.scip_path = Some(p.join("linked.scip"));
    let g = run(&o);
    assert_eq!(g.stats.semantic_state, SemanticState::Unavailable);
    assert!(g.diagnostics.iter().any(|d| d.code == "scip-unavailable"));
}

#[test]
fn java_python_discovery_respects_ignored_environments_and_source_symlinks() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    write(p, ".gitignore", "venv/\ngenerated/\n");
    write(
        p,
        "Example.java",
        "class Example { void run() { work(); } }",
    );
    write(p, "example.py", "def run():\n    work()\n");
    for dir in [
        ".venv",
        "venv",
        "generated",
        "build",
        "target",
        "node_modules",
    ] {
        fs::create_dir(p.join(dir)).unwrap();
        write(p, &format!("{dir}/Ignored.java"), "class Ignored {}");
        write(p, &format!("{dir}/ignored.py"), "def ignored(): pass");
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(p.join("example.py"), p.join("link.py")).unwrap();
        std::os::unix::fs::symlink(p.join("Example.java"), p.join("Link.java")).unwrap();
    }
    let g = run(&IndexOptions::new(p.to_owned()));
    assert_eq!(
        g.files
            .iter()
            .map(|f| (f.path.as_str(), f.language.as_str()))
            .collect::<Vec<_>>(),
        [("Example.java", "java"), ("example.py", "python")]
    );
}

#[test]
fn java_python_never_borrow_fresh_javascript_scip_evidence() {
    let (_d, o) = semantic_fixture();
    let p = &o.workspace_root;
    let java = "class J { void g() {} void f() { g(); } }\n";
    let python = "def g():\n    pass\ndef f():\n    g()\n";
    write(p, "J.java", java);
    write(p, "module.py", python);
    let mut index =
        scip::types::Index::parse_from_bytes(&fs::read(p.join("index.scip")).unwrap()).unwrap();
    for (path, definition, invocation) in [
        (
            "J.java",
            vec![
                0,
                java.find("g()").unwrap() as i32,
                java.find("g()").unwrap() as i32 + 1,
            ],
            vec![
                0,
                java.rfind("g()").unwrap() as i32,
                java.rfind("g()").unwrap() as i32 + 1,
            ],
        ),
        ("module.py", vec![0, 4, 5], vec![3, 4, 5]),
    ] {
        let mut document = scip::types::Document::new();
        document.relative_path = path.into();
        for (range, role) in [(definition, 1), (invocation, 0)] {
            let mut occurrence = scip::types::Occurrence::new();
            occurrence.range = range;
            occurrence.symbol_roles = role;
            occurrence.symbol = "scip-typescript npm unsupported 1 foreign/g().".into();
            document.occurrences.push(occurrence);
        }
        index.documents.push(document);
    }
    fs::write(p.join("index.scip"), index.write_to_bytes().unwrap()).unwrap();
    let js = fs::read_to_string(p.join("main.js")).unwrap();
    fs::write(p.join("index.hashes.json"), serde_json::to_vec(&serde_json::json!({"main.js":hash(&js), "J.java":hash(java), "module.py":hash(python)})).unwrap()).unwrap();
    let g = run(&o);
    assert_eq!(g.stats.semantic_state, SemanticState::Fresh);
    for path in ["J.java", "module.py"] {
        let calls = g
            .calls
            .iter()
            .filter(|c| c.path == path)
            .collect::<Vec<_>>();
        assert!(!calls.is_empty());
        assert!(calls.iter().all(|c| c.resolution == Resolution::Unresolved
            && c.target.is_none()
            && c.candidate_symbols.is_empty()
            && c.provenance.semantic == SemanticState::Unavailable));
        assert!(
            g.nodes
                .iter()
                .filter(|n| n.path == path)
                .all(|n| n.provenance.semantic == SemanticState::Unavailable)
        );
    }
    assert!(
        g.calls
            .iter()
            .any(|c| c.path == "main.js" && c.resolution == Resolution::Internal)
    );
}

#[test]
fn root_java_python_configuration_drift_invalidates_imported_manifest() {
    for config in [
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
        "gradle.properties",
        "pyproject.toml",
        "requirements.txt",
        "uv.lock",
        "poetry.lock",
        "Pipfile",
        "Pipfile.lock",
    ] {
        let (_d, o) = semantic_fixture();
        write(
            &o.workspace_root,
            config,
            "changed; this file is never executed",
        );
        let g = run(&o);
        assert_eq!(g.stats.semantic_state, SemanticState::Stale, "{config}");
        assert_eq!(g.stats.changed_files, [config], "{config}");
    }
}

#[test]
fn conventional_java_source_packages_are_not_confused_with_build_outputs() {
    let d = tempfile::tempdir().unwrap();
    for path in [
        "buildSrc/src/main/java/com/example/build/BuildPlugin.java",
        "src/test/java/com/example/target/TargetTest.java",
        "module/src/testFixtures/java/com/example/dist/Fixture.java",
        "build/generated/src/main/java/com/example/Generated.java",
        "module/build/generated/Generated.java",
    ] {
        let file = d.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "class Example { void run() { work(); } }").unwrap();
    }
    let g = run(&IndexOptions::new(d.path().to_owned()));
    assert_eq!(
        g.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        [
            "buildSrc/src/main/java/com/example/build/BuildPlugin.java",
            "module/src/testFixtures/java/com/example/dist/Fixture.java",
            "src/test/java/com/example/target/TargetTest.java",
        ]
    );
}
