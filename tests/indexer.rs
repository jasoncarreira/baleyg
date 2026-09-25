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

fn capture_admission(root: &Path) -> baleyg::indexer::CaptureAdmission {
    use baleyg::model::v1::Language;
    let identity =
        baleyg::store::topology::WorkspaceIdentity::discover_unattached(Some(root), root).unwrap();
    write(root, "toolchain.capture", "toolchain-1");
    write(root, "config.capture", "config-1");
    write(root, "dependency.capture", "dependency-1");
    let mut index = scip::types::Index::new();
    let mut meta = scip::types::Metadata::new();
    let mut tool = scip::types::ToolInfo::new();
    tool.name = "scip-typescript".into();
    tool.version = "semantic-1".into();
    meta.tool_info = protobuf::MessageField::some(tool);
    index.metadata = protobuf::MessageField::some(meta);
    let mut doc = scip::types::Document::new();
    doc.relative_path = "z.js".into();
    let mut occurrence = scip::types::Occurrence::new();
    occurrence.range = vec![0, 11, 13];
    occurrence.symbol = "semantic-symbol".into();
    doc.occurrences.push(occurrence);
    index.documents.push(doc);
    fs::write(
        root.join("semantic.artifact"),
        protobuf::Message::write_to_bytes(&index).unwrap(),
    )
    .unwrap();
    baleyg::indexer::CaptureAdmission {
        source_set_id: identity.record_id,
        root_id: identity.root_key,
        languages: vec![Language::Rust, Language::Javascript],
        toolchain: root.join("toolchain.capture"),
        config: root.join("config.capture"),
        dependency: root.join("dependency.capture"),
        dependency_source_sets: vec![],
        producers: vec![baleyg::indexer::ProducerInput {
            id: "S".into(),
            tool_name: "scip-typescript".into(),
            version: "semantic-1".into(),
            position_encoding: "utf16".into(),
            executable: std::env::current_exe().unwrap(),
            artifact: Some(root.join("semantic.artifact")),
        }],
    }
}

fn capture_options(root: &Path) -> IndexOptions {
    let mut options = IndexOptions::new(root.to_owned());
    options.scip_path = Some(root.join("semantic.artifact"));
    options
}

#[test]
fn capture_revision_owns_sorted_bytes_and_independent_producer_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "z.js", "const z = '😀';");
    write(root, "a.rs", "fn a() {}\n");
    let admission = capture_admission(root);
    let options = capture_options(root);
    let capture = || baleyg::indexer::capture_revision(&options, &admission, &cancel()).unwrap();
    let first = capture();
    assert_eq!(first, capture());
    assert_eq!(first.documents.len(), 2);
    assert_eq!(first.documents[0].key.path.as_str(), "a.rs");
    assert_eq!(first.documents[1].key.path.as_str(), "z.js");
    assert_eq!(
        first
            .producers
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["N", "S"]
    );
    assert_eq!(
        first.producers[0].executable_hash,
        hex::encode(Sha256::digest(&first.producers[0].executable_bytes))
    );
    assert_eq!(first.producers[0].artifact_bytes, None);
    assert_eq!(
        first.producers[1].artifact_hash.as_deref(),
        Some(
            hex::encode(Sha256::digest(
                first.producers[1].artifact_bytes.as_ref().unwrap()
            ))
            .as_str()
        )
    );
    assert!(
        first.documents[0]
            .native_candidates
            .iter()
            .any(|candidate| candidate.candidate_kind
                == baleyg::indexer::NativeCandidateKind::Declaration
                && candidate.name_bytes == b"a")
    );
    assert!(
        first.documents[1]
            .native_candidates
            .iter()
            .any(|candidate| candidate.candidate_kind
                == baleyg::indexer::NativeCandidateKind::Occurrence)
    );
    let position = &first.documents[1].semantic_positions[0];
    assert_eq!(position.coordinates, [0, 11, 13]);
    assert_eq!(position.position_encoding, "utf16");
    assert_eq!(position.revision_id, first.revision_id);
    assert_eq!(position.symbol, "semantic-symbol");
    for document in &first.documents {
        assert_eq!(
            document.content_hash,
            hex::encode(Sha256::digest(&document.bytes))
        );
        assert_eq!(document.byte_length, document.bytes.len() as u64);
        assert!(!document.syntax.is_empty());
        for node in &document.syntax {
            assert_eq!(
                node.source_bytes,
                document.bytes[node.start_byte..node.end_byte]
            );
            if let Some(parent) = node.parent_id {
                assert!(parent < node.id);
                assert!(document.syntax[parent].start_byte <= node.start_byte);
                assert!(document.syntax[parent].end_byte >= node.end_byte);
            }
        }
    }
    let old = first.documents[1].bytes.clone();
    write(root, "z.js", "const z = 'changed';");
    let second = capture();
    assert_ne!(first.revision_id, second.revision_id);
    assert_eq!(first.documents[1].bytes, old);
    write(root, "config.capture", "config-2");
    assert_ne!(second.revision_id, capture().revision_id);
    let artifact = fs::read(root.join("semantic.artifact")).unwrap();
    let mut index = scip::types::Index::parse_from_bytes(&artifact).unwrap();
    index.documents[0].occurrences[0].symbol = "changed-symbol".into();
    fs::write(
        root.join("semantic.artifact"),
        protobuf::Message::write_to_bytes(&index).unwrap(),
    )
    .unwrap();
    assert_ne!(
        second.producers[1].artifact_hash,
        capture().producers[1].artifact_hash
    );
}

#[test]
fn capture_refuses_missing_and_unsafe_required_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "a.rs", "fn a() {}\n");
    let admission = capture_admission(root);
    let options = capture_options(root);
    fs::remove_file(root.join("semantic.artifact")).unwrap();
    assert!(baleyg::indexer::capture_revision(&options, &admission, &cancel()).is_err());
    let _ = capture_admission(root);
    #[cfg(unix)]
    {
        fs::remove_file(root.join("config.capture")).unwrap();
        let external = tempfile::tempdir().unwrap();
        write(external.path(), "outside", "unsafe");
        std::os::unix::fs::symlink(external.path().join("outside"), root.join("config.capture"))
            .unwrap();
        assert!(baleyg::indexer::capture_revision(&options, &admission, &cancel()).is_err());
    }
}

#[test]
fn capture_rejects_unadmitted_scope_and_sources() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "a.rs", "fn a() {}\n");
    let admission = capture_admission(root);
    let options = capture_options(root);
    let first = baleyg::indexer::capture_revision(&options, &admission, &cancel()).unwrap();
    let mut changed = admission.clone();
    changed.languages.reverse();
    assert_ne!(
        first.revision_id,
        baleyg::indexer::capture_revision(&options, &changed, &cancel())
            .unwrap()
            .revision_id
    );
    changed = admission.clone();
    changed
        .dependency_source_sets
        .push(changed.source_set_id.clone());
    assert!(baleyg::indexer::capture_revision(&options, &changed, &cancel()).is_err());
    changed = admission.clone();
    changed.root_id = "unadmitted".into();
    assert!(baleyg::indexer::capture_revision(&options, &changed, &cancel()).is_err());
    changed = admission.clone();
    changed.languages.push(changed.languages[0]);
    assert!(baleyg::indexer::capture_revision(&options, &changed, &cancel()).is_err());
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "else.rs", "fn elsewhere() {}");
        std::os::unix::fs::symlink(outside.path().join("else.rs"), root.join("linked.rs")).unwrap();
        assert!(baleyg::indexer::capture_revision(&options, &admission, &cancel()).is_err());
    }
}

#[test]
fn capture_manifest_uses_contract_escaping_and_language_order() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "a.js", "call();");
    write(root, "z.rs", "fn z() {}");
    #[cfg(unix)]
    write(root, "x\ny.rs", "fn xy() {}");
    let admission = capture_admission(root);
    let captured =
        baleyg::indexer::capture_revision(&capture_options(root), &admission, &cancel()).unwrap();
    assert_eq!(
        captured.documents[0].key.path.as_str(),
        if cfg!(unix) { "x\ny.rs" } else { "z.rs" }
    );
    assert_eq!(captured.documents.last().unwrap().key.path.as_str(), "a.js");
    #[cfg(unix)]
    {
        let manifest = String::from_utf8(captured.manifest_bytes.clone()).unwrap();
        let source_set = &captured.source_set_id;
        let expected = format!(
            "[{{\"contentHash\":\"{}\",\"document\":{{\"language\":\"rust\",\"path\":\"x\\u000ay.rs\",\"sourceSetId\":\"{}\"}}}},{{\"contentHash\":\"{}\",\"document\":{{\"language\":\"rust\",\"path\":\"z.rs\",\"sourceSetId\":\"{}\"}}}},{{\"contentHash\":\"{}\",\"document\":{{\"language\":\"javascript\",\"path\":\"a.js\",\"sourceSetId\":\"{}\"}}}}]",
            hash("fn xy() {}"),
            source_set,
            hash("fn z() {}"),
            source_set,
            hash("call();"),
            source_set,
        );
        assert_eq!(manifest, expected);
        assert_eq!(
            hex::encode(Sha256::digest(&captured.manifest_bytes)),
            hex::encode(Sha256::digest(expected.as_bytes()))
        );
    }
}

#[test]
fn capture_source_discovery_matches_graph_and_tracks_set_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (path, text) in [
        (
            "src/main/java/com/example/build/Build.java",
            "class Build {}",
        ),
        ("build/generated/Generated.java", "class Generated {}"),
        ("a.rs", "fn a() {}"),
    ] {
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }
    let mut admission = capture_admission(root);
    admission.languages.push(baleyg::model::v1::Language::Java);
    let options = capture_options(root);
    let capture = || baleyg::indexer::capture_revision(&options, &admission, &cancel()).unwrap();
    let first = capture();
    assert_eq!(
        first
            .documents
            .iter()
            .map(|d| d.key.path.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        run(&IndexOptions::new(root.to_owned()))
            .files
            .iter()
            .map(|f| f.path.as_str())
            .collect()
    );
    assert_eq!(first.documents.len(), 2);
    write(root, "new.rs", "fn new() {}");
    let added = capture();
    assert_eq!(added.documents.len(), 3);
    assert_ne!(first.revision_id, added.revision_id);
    fs::remove_file(root.join("new.rs")).unwrap();
    assert_eq!(capture().revision_id, first.revision_id);
    write(root, ".gitignore", "a.rs\n");
    let ignored = capture();
    assert_eq!(ignored.documents.len(), 1);
    assert_ne!(ignored.revision_id, first.revision_id);
}

#[test]
fn capture_preserves_typed_scip_coordinates_without_join_conversion() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "z.js", "const z = '😀';");
    let admission = capture_admission(root);
    let artifact = root.join("semantic.artifact");
    let mut index = scip::types::Index::parse_from_bytes(&fs::read(&artifact).unwrap()).unwrap();
    let mut typed = scip::types::SingleLineRange::new();
    typed.line = 0;
    typed.start_character = 11;
    typed.end_character = 13;
    index.documents[0].occurrences[0].range.clear();
    index.documents[0].occurrences[0].typed_range =
        Some(scip::types::occurrence::Typed_range::SingleLineRange(typed));
    fs::write(&artifact, index.write_to_bytes().unwrap()).unwrap();
    let revision =
        baleyg::indexer::capture_revision(&capture_options(root), &admission, &cancel()).unwrap();
    let position = &revision.documents[0].semantic_positions[0];
    assert_eq!(position.coordinates, [0, 11, 13]);
    assert_eq!(position.position_encoding, "utf16");
    assert_eq!(
        position.artifact_hash,
        revision.producers[1]
            .artifact_hash
            .as_ref()
            .unwrap()
            .as_str()
    );
}

#[test]
fn capture_native_only_and_rejects_unmatched_semantic_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "a.js", "const a = '😀';");
    let mut admission = capture_admission(root);
    let mut options = capture_options(root);
    let first = baleyg::indexer::capture_revision(&options, &admission, &cancel()).unwrap();
    admission.producers[0].artifact = Some(root.join("other.scip"));
    assert!(baleyg::indexer::capture_revision(&options, &admission, &cancel()).is_err());
    options.scip_path = None;
    admission.producers.clear();
    let native = baleyg::indexer::capture_revision(&options, &admission, &cancel()).unwrap();
    assert_eq!(native.producers.len(), 1);
    assert!(native.documents[0].semantic_positions.is_empty());
    assert_ne!(first.revision_id, native.revision_id);
}

#[test]
fn capture_does_not_claim_graph_or_semantic_completeness() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.js", "call();");
    let admission = capture_admission(dir.path());
    let options = capture_options(dir.path());
    let captured = baleyg::indexer::capture_revision(&options, &admission, &cancel()).unwrap();
    assert_eq!(captured.producers.len(), 2);
    assert_eq!(
        run(&IndexOptions::new(dir.path().to_owned()))
            .stats
            .semantic_state,
        SemanticState::Unavailable
    );
}

#[test]
fn capture_native_witnesses_preserve_token_header_and_typed_regions() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "a.rs",
        "fn greet() { if ready() { target(); } }
",
    );
    write(
        root,
        "b.js",
        "function greet() { if (ready()) target(); }
",
    );
    let admission = capture_admission(root);
    let options = capture_options(root);
    let first = baleyg::indexer::capture_revision(&options, &admission, &cancel()).unwrap();
    for document in &first.documents {
        let name = document
            .native_candidates
            .iter()
            .find(|w| {
                w.candidate_kind == baleyg::indexer::NativeCandidateKind::Declaration
                    && w.name_bytes == b"greet"
            })
            .unwrap();
        assert_eq!(
            &document.bytes[name.token_start_byte..name.token_end_byte],
            b"greet"
        );
        assert_eq!(name.token_bytes, b"greet");
        assert!(
            !name
                .header_bytes
                .windows(6)
                .any(|window| window == b"target")
        );
        for kind in [
            baleyg::indexer::NativeCandidateKind::Invocation,
            baleyg::indexer::NativeCandidateKind::ControlRegion,
        ] {
            let witnesses: Vec<_> = document
                .native_candidates
                .iter()
                .filter(|w| w.candidate_kind == kind)
                .collect();
            assert!(
                !witnesses.is_empty(),
                "missing {kind:?} in {}",
                document.key.path.as_str()
            );
            for witness in witnesses {
                assert!(witness.start_byte < witness.end_byte);
                assert!(witness.token_start_byte >= witness.start_byte);
                assert!(witness.token_end_byte <= witness.end_byte);
                assert!(witness.owner_id < document.syntax.len());
            }
        }
    }
    write(
        root,
        "a.rs",
        "fn greet() { if ready() { changed(); } }
",
    );
    write(
        root,
        "b.js",
        "function greet() { if (ready()) changed(); }
",
    );
    let second = baleyg::indexer::capture_revision(&options, &admission, &cancel()).unwrap();
    assert_ne!(first.revision_id, second.revision_id);
    for (before, after) in first.documents.iter().zip(second.documents.iter()) {
        let header = |doc: &baleyg::indexer::CapturedDocument| {
            doc.native_candidates
                .iter()
                .find(|w| {
                    w.candidate_kind == baleyg::indexer::NativeCandidateKind::Declaration
                        && w.name_bytes == b"greet"
                })
                .unwrap()
                .header_bytes
                .clone()
        };
        assert_eq!(header(before), header(after));
    }
}

#[test]
fn capture_excludes_unmeasured_declarations_without_losing_native_syntax() {
    use baleyg::indexer::NativeCandidateKind;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "a.rs",
        "fn greet() { let item = before(); consume(item); }\n",
    );
    write(
        root,
        "b.js",
        "function greet() { let item = before(); consume(item); }\n",
    );
    let admission = capture_admission(root);
    let options = capture_options(root);
    let first = baleyg::indexer::capture_revision(&options, &admission, &cancel()).unwrap();
    write(
        root,
        "a.rs",
        "fn greet() { let item = changed(); consume(item); }\n",
    );
    write(
        root,
        "b.js",
        "function greet() { let item = changed(); consume(item); }\n",
    );
    let second = baleyg::indexer::capture_revision(&options, &admission, &cancel()).unwrap();
    assert_ne!(first.revision_id, second.revision_id);
    for (before, after) in first.documents.iter().zip(second.documents.iter()) {
        let unsupported = if before.key.path.as_str().ends_with(".rs") {
            "let_declaration"
        } else {
            "lexical_declaration"
        };
        for doc in [before, after] {
            assert!(doc.syntax.iter().any(|node| node.kind == unsupported));
            assert!(!doc.native_candidates.iter().any(|w| {
                w.candidate_kind == NativeCandidateKind::Declaration && w.node_kind == unsupported
            }));
            assert!(doc.native_candidates.iter().any(|w| {
                w.candidate_kind == NativeCandidateKind::Invocation
                    && w.owner_id < doc.syntax.len()
                    && w.token_end_byte <= doc.bytes.len()
            }));
            for witness in doc
                .native_candidates
                .iter()
                .filter(|w| w.candidate_kind == NativeCandidateKind::Declaration)
            {
                assert_eq!(
                    witness.token_bytes,
                    doc.bytes[witness.token_start_byte..witness.token_end_byte]
                );
                assert_eq!(witness.name_bytes, witness.token_bytes);
                assert_ne!(
                    witness.token_bytes,
                    doc.syntax[witness.node_id].source_bytes
                );
            }
        }
        let declarations = |doc: &baleyg::indexer::CapturedDocument| {
            doc.native_candidates
                .iter()
                .filter(|w| w.candidate_kind == NativeCandidateKind::Declaration)
                .map(|w| {
                    (
                        w.node_kind.clone(),
                        w.name_bytes.clone(),
                        w.header_bytes.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(declarations(before), declarations(after));
    }
}

#[test]
fn capture_rejects_mid_acquisition_source_set_and_required_read_changes() {
    for change in ["remove", "add", "content", "ignore"] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "a.rs",
            "fn a() {}
",
        );
        write(
            root,
            "b.rs",
            "fn b() {}
",
        );
        let admission = capture_admission(root);
        let options = capture_options(root);
        let result =
            baleyg::indexer::capture_revision_with_hook(&options, &admission, &cancel(), || {
                match change {
                    "remove" => fs::remove_file(root.join("b.rs")).unwrap(),
                    "add" => write(
                        root,
                        "c.rs",
                        "fn c() {}
",
                    ),
                    "content" => write(
                        root,
                        "b.rs",
                        "fn changed() {}
",
                    ),
                    "ignore" => write(
                        root,
                        ".gitignore",
                        "b.rs
",
                    ),
                    _ => unreachable!(),
                }
            });
        assert!(result.is_err(), "accepted mid-acquisition {change}");
    }
}

#[test]
fn capture_accepts_authentic_scip_tool_name_without_conflating_producer_id() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "z.js", "const z = '😀';");
    let mut admission = capture_admission(root);
    admission.producers[0].id = "semantic:ts".into();
    let artifact = root.join("semantic.artifact");
    let mut index = scip::types::Index::parse_from_bytes(&fs::read(&artifact).unwrap()).unwrap();
    index
        .metadata
        .as_mut()
        .unwrap()
        .tool_info
        .as_mut()
        .unwrap()
        .name = "scip-typescript".into();
    fs::write(&artifact, index.write_to_bytes().unwrap()).unwrap();
    let captured =
        baleyg::indexer::capture_revision(&capture_options(root), &admission, &cancel()).unwrap();
    assert_eq!(captured.producers[1].id, "semantic:ts");
    assert_eq!(captured.producers[1].tool_name, "scip-typescript");
    let mut mismatched = admission.clone();
    mismatched.producers[0].tool_name = "scip-python".into();
    assert!(
        baleyg::indexer::capture_revision(&capture_options(root), &mismatched, &cancel()).is_err()
    );
    assert_eq!(
        captured.documents[0].semantic_positions[0].producer_id,
        "semantic:ts"
    );
    index
        .metadata
        .as_mut()
        .unwrap()
        .tool_info
        .as_mut()
        .unwrap()
        .name
        .clear();
    fs::write(&artifact, index.write_to_bytes().unwrap()).unwrap();
    assert!(
        baleyg::indexer::capture_revision(&capture_options(root), &admission, &cancel()).is_err()
    );
}

#[test]
fn javascript_capture_ids_tokens_heritage_and_revision_local_occurrences() {
    use baleyg::indexer::NativeCandidateKind as K;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "b.js",
        "class Child extends Base { foo() { obj.foo(); obj[key](); obj?.foo(); } }\nfunction same() { obj.\\u{0066}oo(); }\nfunction same() { obj.foo(); }\n",
    );
    let admission = capture_admission(root);
    let options = capture_options(root);
    let first = baleyg::indexer::capture_revision(&options, &admission, &cancel()).unwrap();
    let doc = &first.documents[0];
    assert_eq!(doc.heritage.len(), 1);
    let heritage = &doc.heritage[0];
    assert_eq!(&doc.bytes[heritage.base_start..heritage.base_end], b"Base");
    assert_eq!(
        &doc.bytes[heritage.subclass_name_start..heritage.subclass_name_end],
        b"Child"
    );
    let decl: Vec<_> = doc
        .native_candidates
        .iter()
        .filter(|w| w.candidate_kind == K::Declaration && w.name_bytes == b"same")
        .collect();
    assert_eq!(decl.len(), 2);
    assert_ne!(decl[0].stable_id, decl[1].stable_id);
    assert!(
        decl.iter()
            .all(|w| w.stable_id.as_deref().unwrap().starts_with("sid:v1:"))
    );
    let calls: Vec<_> = doc
        .native_candidates
        .iter()
        .filter(|w| w.candidate_kind == K::Invocation)
        .collect();
    assert!(calls.len() >= 5);
    assert!(
        calls
            .iter()
            .all(|w| w.stable_id.as_deref().unwrap().starts_with("occ:v1:")
                && !w.ancestor_ids.is_empty())
    );
    assert!(calls.iter().any(|w| w.verified_member_token
        && w.spelling.as_deref() == Some("foo")
        && &doc.bytes[w.token_start_byte..w.token_end_byte] == b"foo"));
    assert!(calls.iter().any(|w| !w.verified_member_token
        && doc.bytes[w.start_byte..w.end_byte].starts_with(b"obj[key]")));
    assert!(calls.iter().any(|w| !w.verified_member_token
        && doc.bytes[w.start_byte..w.end_byte].starts_with(b"obj?.foo")));
    assert!(calls.iter().any(|w| w.verified_member_token
        && w.spelling.as_deref() == Some("foo")
        && w.token_bytes == b"\\u{0066}oo"));
    write(
        root,
        "b.js",
        "class Child extends Base { foo() { obj.foo(); obj[key](); obj?.foo(); extra(); } }\nfunction same() { obj.foo(); }\nfunction same() { obj.foo(); }\n",
    );
    let second = baleyg::indexer::capture_revision(&options, &admission, &cancel()).unwrap();
    assert_ne!(first.revision_id, second.revision_id);
    let first_child = doc
        .native_candidates
        .iter()
        .find(|w| w.candidate_kind == K::Declaration && w.name_bytes == b"Child")
        .unwrap();
    let second_child = second.documents[0]
        .native_candidates
        .iter()
        .find(|w| w.candidate_kind == K::Declaration && w.name_bytes == b"Child")
        .unwrap();
    assert_eq!(first_child.stable_id, second_child.stable_id);
    let second_dupes: Vec<_> = second.documents[0]
        .native_candidates
        .iter()
        .filter(|w| w.candidate_kind == K::Declaration && w.name_bytes == b"same")
        .collect();
    assert_eq!(decl[0].stable_id, second_dupes[0].stable_id);
    assert_eq!(decl[1].stable_id, second_dupes[1].stable_id);
    assert_ne!(
        calls[0].stable_id,
        second.documents[0]
            .native_candidates
            .iter()
            .find(|w| w.candidate_kind == K::Invocation)
            .unwrap()
            .stable_id
    );
}

#[test]
fn javascript_nested_duplicate_ordinals_inventory_and_graph_ids() {
    use baleyg::indexer::NativeCandidateKind as K;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = "class Same { méthode() { new Thing(); obj.méthode(); } }\nclass Same { méthode() { obj.foo(); (obj.foo)(); obj?.foo(); obj[key](); } }\nfunction blocks() { const f = function named() {}; const g = function* () {}; const h = () => 1; for (const k in obj) { obj[k](); } do { work(); } while (flag); switch(x) { case 1: left(); break; default: right(); } return x && more(); }\n";
    write(root, "b.js", source);
    let admission = capture_admission(root);
    let options = capture_options(root);
    let first = baleyg::indexer::capture_revision(&options, &admission, &cancel()).unwrap();
    let doc = &first.documents[0];
    let classes: Vec<_> = doc
        .native_candidates
        .iter()
        .filter(|w| w.node_kind == "class_declaration" && w.stable_id.is_some())
        .collect();
    let methods: Vec<_> = doc
        .native_candidates
        .iter()
        .filter(|w| w.node_kind == "method_definition" && w.stable_id.is_some())
        .collect();
    assert_eq!((classes.len(), methods.len()), (2, 2));
    assert_ne!(classes[0].stable_id, classes[1].stable_id);
    assert_ne!(methods[0].stable_id, methods[1].stable_id);
    assert_eq!(
        methods[0].ancestor_ids.last(),
        classes[0].stable_id.as_ref()
    );
    assert_eq!(
        methods[1].ancestor_ids.last(),
        classes[1].stable_id.as_ref()
    );
    for kind in [
        "function_expression",
        "generator_function",
        "arrow_function",
    ] {
        assert!(
            doc.native_candidates.iter().any(|w| w.node_kind == kind
                && w.candidate_kind == K::Declaration
                && w.stable_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("sid:v1:"))),
            "{kind}"
        );
    }
    for kind in [
        "new_expression",
        "for_in_statement",
        "do_statement",
        "switch_case",
        "switch_default",
        "binary_expression",
    ] {
        assert!(
            doc.native_candidates.iter().any(|w| w.node_kind == kind
                && w.stable_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("occ:v1:"))),
            "{kind}"
        );
    }
    let calls: Vec<_> = doc
        .native_candidates
        .iter()
        .filter(|w| w.candidate_kind == K::Invocation)
        .collect();
    assert!(calls.iter().any(|w| w.verified_member_token
        && w.token_bytes == "méthode".as_bytes()
        && &doc.bytes[w.token_start_byte..w.token_end_byte] == "méthode".as_bytes()));
    for callee in ["(obj.foo)()", "obj?.foo()", "obj[key]()"] {
        assert!(
            calls.iter().any(|w| !w.verified_member_token
                && doc.bytes[w.start_byte..w.end_byte].starts_with(callee.as_bytes())),
            "{callee}"
        );
    }
    let graph = run(&IndexOptions::new(root.to_owned()));
    assert!(graph.nodes.iter().all(|n| n.id.starts_with("sid:v1:")));
    assert!(graph.calls.iter().all(|c| c.id.starts_with("occ:v1:")));
    assert!(graph.regions.iter().all(|r| r.id.starts_with("occ:v1:")));
    let edited = source
        .replace("new Thing();", "new Other();")
        .replace("obj.foo();", "obj.bar();");
    write(root, "b.js", &edited);
    let second = baleyg::indexer::capture_revision(&options, &admission, &cancel()).unwrap();
    let next: Vec<_> = second.documents[0]
        .native_candidates
        .iter()
        .filter(|w| w.node_kind == "method_definition" && w.stable_id.is_some())
        .collect();
    assert_eq!(
        methods.iter().map(|m| &m.stable_id).collect::<Vec<_>>(),
        next.iter().map(|m| &m.stable_id).collect::<Vec<_>>()
    );
}

#[test]
fn named_class_expression_preserves_instance_initializer_owner() {
    use baleyg::indexer::NativeCandidateKind as K;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "fields.js",
        "function make() { if (ok) return class C { [fieldKey()] = build(); static eager = now(); [key()]() { body(); } }; }\n",
    );
    let admission = capture_admission(root);
    let captured =
        baleyg::indexer::capture_revision(&capture_options(root), &admission, &cancel()).unwrap();
    let class = captured.documents[0]
        .native_candidates
        .iter()
        .find(|w| w.node_kind == "class" && w.candidate_kind == K::Declaration)
        .unwrap();
    assert_eq!(class.name_bytes, b"C");
    assert!(class.stable_id.as_deref().unwrap().starts_with("sid:v1:"));
    let graph = run(&IndexOptions::new(root.to_owned()));
    let owner = |callee: &str| {
        let call = graph
            .calls
            .iter()
            .find(|c| c.callee_text == callee)
            .unwrap();
        graph
            .nodes
            .iter()
            .find(|n| n.id == call.caller)
            .unwrap()
            .name
            .as_str()
    };
    for callee in ["fieldKey", "now", "key"] {
        assert_eq!(owner(callee), "make");
    }
    assert_eq!(owner("build"), "C");
    let build = graph
        .calls
        .iter()
        .find(|c| c.callee_text == "build")
        .unwrap();
    assert!(
        graph
            .regions
            .iter()
            .any(|r| r.kind == "instance-initializer"
                && r.owner == build.caller
                && build.regions.contains(&r.id))
    );
}

#[test]
fn browser_occurrences_follow_complete_source_and_basis_capture() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "main.js", "function keep() { call(); }\n");
    write(root, "sibling.js", "function sibling() {}\n");
    let options = IndexOptions::new(root.to_owned());
    let read = || {
        let graph = run(&options);
        let declaration = graph
            .nodes
            .iter()
            .find(|n| n.name == "keep")
            .unwrap()
            .id
            .clone();
        let occurrence = graph
            .calls
            .iter()
            .find(|c| c.callee_text == "call")
            .unwrap()
            .id
            .clone();
        (declaration, occurrence)
    };
    let first = read();
    write(root, "sibling.js", "function sibling() { changed(); }\n");
    let sibling = read();
    assert_eq!(first.0, sibling.0);
    assert_ne!(first.1, sibling.1);
    write(root, "package.json", "{\"name\":\"one\"}");
    let basis = read();
    assert_eq!(sibling.0, basis.0);
    assert_ne!(sibling.1, basis.1);
    write(root, "package.json", "{\"name\":\"two\"}");
    let changed_basis = read();
    assert_eq!(basis.0, changed_basis.0);
    assert_ne!(basis.1, changed_basis.1);
    write(root, "main.js", "function keep() { call(); other(); }\n");
    let body = read();
    assert_eq!(changed_basis.0, body.0);
    assert_ne!(changed_basis.1, body.1);
}

#[test]
fn java_capture_measures_member_name_without_semantic_promotion() {
    use baleyg::indexer::NativeCandidateKind as K;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "Café.java",
        "class Café { void work() { obj.fóo(); obj.method().next(); new Café(); } }",
    );
    let mut admission = capture_admission(root);
    admission.languages = vec![baleyg::model::v1::Language::Java];
    admission.producers.clear();
    let capture = baleyg::indexer::capture_revision(
        &IndexOptions::new(root.to_owned()),
        &admission,
        &cancel(),
    )
    .unwrap();
    let doc = &capture.documents[0];
    let calls: Vec<_> = doc
        .native_candidates
        .iter()
        .filter(|w| w.candidate_kind == K::Invocation)
        .collect();
    assert!(calls.len() >= 3);
    assert!(calls.iter().all(|w| {
        w.stable_id
            .as_deref()
            .is_some_and(|id| id.starts_with("occ:v1:"))
    }));
    let member = calls
        .iter()
        .find(|w| w.spelling.as_deref() == Some("fóo"))
        .unwrap();
    assert!(member.verified_member_token);
    assert_eq!(
        &doc.bytes[member.token_start_byte..member.token_end_byte],
        "fóo".as_bytes()
    );
    assert!(
        calls.iter().any(|w| !w.verified_member_token
            && doc.bytes[w.start_byte..w.end_byte].starts_with(b"new Caf"))
    );
    let graph = run(&IndexOptions::new(root.to_owned()));
    assert!(
        graph
            .calls
            .iter()
            .filter(|c| c.path.ends_with(".java"))
            .all(|c| c.target.is_none() && c.candidate_symbols.is_empty())
    );
}

#[test]
fn java_anonymous_type_and_lambda_have_distinct_canonical_kinds() {
    use baleyg::model::v1::{Key, Kind, Language, Signature, Text, UInt};
    use baleyg::semantic_identity::syntax_id;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "Kinds.java",
        "class Kinds { void run() { Object a = new Object() { void inside() {} }; Runnable r = () -> work(); } }\n",
    );
    let mut admission = capture_admission(root);
    admission.languages = vec![Language::Java];
    let captured =
        baleyg::indexer::capture_revision(&capture_options(root), &admission, &cancel()).unwrap();
    let doc = &captured.documents[0];
    let key = |kind, name: Option<&str>, signature| Key {
        kind,
        name: name.map(|value| Text::new(value.to_owned()).unwrap()),
        signature,
        ordinal: UInt::new(0).unwrap(),
    };
    let module = key(Kind::Module, None, None);
    let class = key(Kind::Type, Some("Kinds"), None);
    let method = key(
        Kind::Method,
        Some("run"),
        Some(Signature {
            parameter_types: vec![],
            type_parameter_count: UInt::new(0).unwrap(),
            variadic: false,
        }),
    );
    let ancestors = [module, class, method];
    let id = |kind| {
        syntax_id(
            &doc.key.source_set_id,
            &doc.key.path,
            Language::Java,
            &ancestors,
            &key(kind, None, None),
        )
        .unwrap()
        .as_str()
        .to_owned()
    };
    assert!(
        syntax_id(
            &doc.key.source_set_id,
            &doc.key.path,
            Language::Java,
            &ancestors,
            &key(Kind::Type, None, None),
        )
        .is_ok()
    );
    let anonymous = doc
        .native_candidates
        .iter()
        .find(|w| w.node_kind == "class_body" && w.stable_id.is_some())
        .unwrap();
    let lambda = doc
        .native_candidates
        .iter()
        .find(|w| w.node_kind == "lambda_expression" && w.stable_id.is_some())
        .unwrap();
    assert_eq!(
        anonymous.stable_id.as_deref(),
        Some(id(Kind::Type).as_str())
    );
    assert_eq!(
        lambda.stable_id.as_deref(),
        Some(id(Kind::AnonymousFunction).as_str())
    );
    assert_ne!(anonymous.stable_id, lambda.stable_id);
    // A named type remains a separate canonical key; other declaration kinds
    // cannot borrow the anonymous-type exception.
    assert_ne!(
        id(Kind::Type),
        syntax_id(
            &doc.key.source_set_id,
            &doc.key.path,
            Language::Java,
            &ancestors,
            &key(Kind::Type, Some("Named"), None),
        )
        .unwrap()
        .as_str()
    );
    assert!(
        syntax_id(
            &doc.key.source_set_id,
            &doc.key.path,
            Language::Java,
            &ancestors,
            &key(Kind::Method, None, None),
        )
        .is_err()
    );
}

#[test]
fn java_control_candidates_match_measured_graph_regions() {
    use baleyg::indexer::NativeCandidateKind as K;
    use baleyg::model::v1::Language;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "Flow.java",
        "class Flow { void run(int x) { ordinary(); { nested(); } if (x > 0) { yes(); } switch (x) { case 1: one(); break; default: other(); } try { risky(); } catch (Exception e) { recover(); } } }\n",
    );
    let mut admission = capture_admission(root);
    admission.languages = vec![Language::Java];
    let captured =
        baleyg::indexer::capture_revision(&capture_options(root), &admission, &cancel()).unwrap();
    let doc = &captured.documents[0];
    let regions: Vec<_> = doc
        .native_candidates
        .iter()
        .filter(|w| w.candidate_kind == K::ControlRegion)
        .collect();
    assert!(!regions.is_empty());
    assert!(regions.iter().any(|w| w.node_kind == "if_statement"));
    assert!(
        regions
            .iter()
            .any(|w| w.node_kind == "switch_block_statement_group")
    );
    assert!(regions.iter().all(|w| {
        w.stable_id
            .as_deref()
            .is_some_and(|id| id.starts_with("occ:v1:"))
    }));
    assert!(!regions.iter().any(|w| matches!(
        w.node_kind.as_str(),
        "statement_block" | "expression_statement" | "switch_statement"
    )));
    let graph = run(&IndexOptions::new(root.to_owned()));
    let graph_regions: std::collections::BTreeSet<_> = graph
        .regions
        .iter()
        .filter(|r| r.path.ends_with("Flow.java"))
        .map(|r| {
            assert!(r.id.starts_with("occ:v1:"));
            (r.kind.as_str(), r.range.start_byte, r.range.end_byte)
        })
        .collect();
    let measured: std::collections::BTreeSet<_> = regions
        .iter()
        .map(|w| (w.node_kind.as_str(), w.start_byte, w.end_byte))
        .collect();
    assert_eq!(measured, graph_regions);
}

#[test]
fn python_capture_proves_only_source_member_tokens() {
    use baleyg::{
        indexer::{CaptureAdmission, NativeCandidateKind, capture_revision},
        model::v1::Language,
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = "def f():\n    obj.éclair()\n    obj.Ａ()\n    obj[key]()\n    (obj.foo if flag else obj.bar)()\n";
    write(root, "a.py", source);
    for input in ["toolchain.capture", "config.capture", "dependency.capture"] {
        write(root, input, input);
    }
    let identity =
        baleyg::store::topology::WorkspaceIdentity::discover_unattached(Some(root), root).unwrap();
    let admission = CaptureAdmission {
        source_set_id: identity.record_id,
        root_id: identity.root_key,
        languages: vec![Language::Python],
        toolchain: root.join("toolchain.capture"),
        config: root.join("config.capture"),
        dependency: root.join("dependency.capture"),
        dependency_source_sets: vec![],
        producers: vec![],
    };
    let capture =
        capture_revision(&IndexOptions::new(root.to_owned()), &admission, &cancel()).unwrap();
    let doc = capture
        .documents
        .iter()
        .find(|d| d.key.path.as_str() == "a.py")
        .unwrap();
    let calls: Vec<_> = doc
        .native_candidates
        .iter()
        .filter(|w| w.candidate_kind == NativeCandidateKind::Invocation)
        .collect();
    assert_eq!(calls.len(), 4);
    assert!(
        calls
            .iter()
            .all(|w| w.stable_id.as_deref().unwrap().starts_with("occ:v1:"))
    );
    let member = calls
        .iter()
        .find(|w| source[w.start_byte..w.end_byte].starts_with("obj.éclair"))
        .unwrap();
    assert_eq!(
        &source[member.token_start_byte..member.token_end_byte],
        "éclair"
    );
    assert_eq!(member.spelling.as_deref(), Some("éclair"));
    assert!(member.verified_member_token);
    let fullwidth = calls
        .iter()
        .find(|w| source[w.start_byte..w.end_byte].starts_with("obj.Ａ"))
        .unwrap();
    assert_eq!(fullwidth.spelling.as_deref(), Some("Ａ"));
    assert_eq!(
        &source[fullwidth.token_start_byte..fullwidth.token_end_byte],
        "Ａ"
    );
    for call in calls.iter().filter(|w| !w.verified_member_token) {
        assert!(call.spelling.is_none() || !call.spelling.as_deref().unwrap().is_empty());
    }
    assert_eq!(calls.iter().filter(|w| w.verified_member_token).count(), 2);
}
