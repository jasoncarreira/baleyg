use baleyg::{
    indexer::{IndexOptions, index_workspace},
    model::*,
};
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
fn fixture(name: &str) -> IndexOptions {
    IndexOptions::new(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/extraction")
            .join(name),
    )
}
#[test]
fn pure_fixture_and_determinism() {
    let options = fixture("fixture");
    let graph = run(&options);
    assert_eq!(graph, run(&options));
    assert_eq!(graph.stats.semantic_state, SemanticState::Unavailable);
    assert!(graph.nodes.iter().all(|n| n.id.starts_with("sid:v1:")));
    assert!(graph.calls.iter().all(|c| c.id.starts_with("occ:v1:")));
    assert!(graph.regions.iter().all(|r| r.id.starts_with("occ:v1:")));
    assert!(
        graph
            .calls
            .iter()
            .all(|c| c.provenance.semantic == SemanticState::Unavailable)
    );
    assert!(
        graph
            .nodes
            .iter()
            .all(|n| n.provenance.semantic == SemanticState::Unavailable)
    );
    assert!(
        serde_json::to_string(&graph)
            .unwrap()
            .find("candidateSymbols")
            .is_none()
    );
    assert!(
        serde_json::to_string(&graph)
            .unwrap()
            .find("callbackArguments")
            .is_none()
    );
    assert!(
        serde_json::to_string(&graph)
            .unwrap()
            .find("\"target\"")
            .is_none()
    );
    assert!(
        serde_json::to_string(&graph)
            .unwrap()
            .find("\"resolution\"")
            .is_none()
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
        std::os::unix::fs::symlink(p.join("keep.js"), p.join("linked.txt")).unwrap();
        std::os::unix::fs::symlink(p, p.join("linked-dir")).unwrap();
    }
    let g = run(&IndexOptions::new(p.to_owned()));
    assert_eq!(
        g.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        ["keep.cjs", "keep.js", "keep.mjs"]
    );
}
#[test]
fn measured_callees_are_terminal_not_targets() {
    let d = tempfile::tempdir().unwrap();
    let examples = [
        ("T.java", "class T { void go() { obj.foo(); } }", "foo"),
        ("t.js", "function go() { obj.foo(); }", "foo"),
        ("t.py", "def go():\n    obj.foo()\n", "foo"),
        ("t.rs", "fn go() { obj.foo(); }", "foo"),
    ];
    for (path, source, _) in examples {
        write(d.path(), path, source);
    }
    let graph = run(&IndexOptions::new(d.path().to_owned()));
    for (path, source, token) in examples {
        let call = graph.calls.iter().find(|c| c.path == path).unwrap();
        assert_eq!(call.callee_text.as_deref(), Some(token), "{path}");
        let r = call.callee_range.as_ref().expect("measured exact token");
        assert_eq!(&source[r.start_byte..r.end_byte], token, "{path}");
        assert!(graph.nodes.iter().any(|n| n.id == call.caller));
    }
}

#[test]
fn declaration_ids_survive_body_only_change_but_calls_are_revision_local() {
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("main.rs");
    write(d.path(), "main.rs", "fn same() { foo(); }\n");
    let first = run(&IndexOptions::new(d.path().to_owned()));
    write(d.path(), "main.rs", "fn same() { bar(); }\n");
    let second = run(&IndexOptions::new(d.path().to_owned()));
    let id = |graph: &Graph| {
        graph
            .nodes
            .iter()
            .find(|n| n.name == "same")
            .unwrap()
            .id
            .clone()
    };
    assert_eq!(id(&first), id(&second));
    assert_ne!(first.calls[0].id, second.calls[0].id);
    assert_ne!(first.files[0].hash, second.files[0].hash);
    assert_eq!(file.file_name().unwrap(), "main.rs");
}

#[test]
fn graph_ids_and_ranges_are_exact_native_artifact_projection() {
    use baleyg::{indexer::index_workspace_with_native, store::topology::WorkspaceIdentity};
    let d = tempfile::tempdir().unwrap();
    for (path, source) in [
        ("T.java", "class T { void go() { obj.foo(); } }"),
        ("t.js", "function go() { obj.foo(); }"),
        ("t.py", "def go():\n    obj.foo()\n"),
        ("t.rs", "fn go() { obj.foo(); }"),
    ] {
        write(d.path(), path, source);
    }
    let identity = WorkspaceIdentity::discover(Some(d.path()), d.path()).unwrap();
    let (graph, native) = index_workspace_with_native(
        &IndexOptions::new(d.path().to_owned()),
        &identity.record_id,
        &cancel(),
        |_| {},
    )
    .unwrap();
    assert_eq!(graph.calls.len(), native.calls.len());
    assert_eq!(graph.regions.len(), native.control_regions.len());
    assert_eq!(graph.nodes.len(), native.declarations.len());
    for call in &graph.calls {
        let native_call = native.calls.iter().find(|c| c.id == call.id).unwrap();
        assert_eq!(call.caller, native_call.owner_syntax_id);
        assert_eq!(call.regions, native_call.region_ids);
        assert_eq!(call.range.start_byte, native_call.range.start);
        assert_eq!(call.range.end_byte, native_call.range.end);
        assert_eq!(call.callee_text.as_deref(), native_call.spelling.as_deref());
        assert_eq!(
            call.callee_range
                .as_ref()
                .map(|r| (r.start_byte, r.end_byte)),
            native_call.callee_range.as_ref().map(|r| (r.start, r.end))
        );
    }
    let graph_text = serde_json::to_string(&graph).unwrap();
    for forbidden in [
        "\"target\"",
        "candidateSymbols",
        "callbackArguments",
        "\"resolution\"",
    ] {
        assert!(
            !graph_text.contains(forbidden),
            "unsafe graph field: {forbidden}"
        );
    }
}

#[test]
fn scip_label_requires_exact_captured_hash_and_name_token_and_never_supplies_identity() {
    use protobuf::Message;
    use sha2::{Digest, Sha256};
    let d = tempfile::tempdir().unwrap();
    let source = "function f() {}\nf();\n";
    write(d.path(), "main.js", source);
    let mut index = scip::types::Index::new();
    let mut document = scip::types::Document::new();
    document.relative_path = "main.js".into();
    let mut occurrence = scip::types::Occurrence::new();
    occurrence.range = vec![0, 9, 10];
    occurrence.symbol_roles = 1; // exact declaration-name token, not a call target
    occurrence.symbol = "scip npm display 1 main.js/f().".into();
    document.occurrences.push(occurrence);
    index.documents.push(document);
    fs::write(d.path().join("index.scip"), index.write_to_bytes().unwrap()).unwrap();
    let source_hash = hex::encode(Sha256::digest(source.as_bytes()));
    fs::write(
        d.path().join("manifest.json"),
        serde_json::to_vec(&serde_json::json!({"main.js":source_hash})).unwrap(),
    )
    .unwrap();
    let base = run(&IndexOptions::new(d.path().to_owned()));
    let mut options = IndexOptions::new(d.path().to_owned());
    options.scip_path = Some(d.path().join("index.scip"));
    options.manifest_path = Some(d.path().join("manifest.json"));
    let with_label = run(&options);
    let declaration = with_label.nodes.iter().find(|n| n.name == "f").unwrap();
    assert_eq!(
        declaration.display_label.as_deref(),
        Some("scip npm display 1 main.js/f().")
    );
    assert_eq!(
        base.nodes.iter().map(|n| &n.id).collect::<Vec<_>>(),
        with_label.nodes.iter().map(|n| &n.id).collect::<Vec<_>>()
    );
    assert_eq!(base.calls, with_label.calls);
    assert_eq!(declaration.provenance.semantic, SemanticState::Unavailable);
    write(d.path(), "manifest.json", "{\"main.js\":\"stale\"}");
    assert!(
        run(&options)
            .nodes
            .iter()
            .all(|n| n.display_label.is_none())
    );
    fs::write(
        d.path().join("manifest.json"),
        serde_json::to_vec(&serde_json::json!({"main.js":source_hash})).unwrap(),
    )
    .unwrap();
    index.documents[0].occurrences[0].range = vec![1, 0, 1]; // call site, not definition
    fs::write(d.path().join("index.scip"), index.write_to_bytes().unwrap()).unwrap();
    assert!(
        run(&options)
            .nodes
            .iter()
            .all(|n| n.display_label.is_none())
    );
    index.documents[0].occurrences[0].range = vec![0, 9, 10];
    index.documents[0].occurrences[0].symbol = "scip npm display 1 other().".into();
    fs::write(d.path().join("index.scip"), index.write_to_bytes().unwrap()).unwrap();
    assert!(
        run(&options)
            .nodes
            .iter()
            .all(|n| n.display_label.is_none())
    );
}

#[test]
fn root_dependency_drift_changes_native_revision_not_stable_declaration_ids() {
    use baleyg::{indexer::index_workspace_with_native, store::topology::WorkspaceIdentity};
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "main.js", "function same() { foo(); }");
    let root_id = WorkspaceIdentity::discover(Some(d.path()), d.path())
        .unwrap()
        .record_id;
    let options = IndexOptions::new(d.path().to_owned());
    let (before, first) =
        index_workspace_with_native(&options, &root_id, &cancel(), |_| {}).unwrap();
    write(
        d.path(),
        "Cargo.lock",
        "changed immutable dependency selector",
    );
    let (after, second) =
        index_workspace_with_native(&options, &root_id, &cancel(), |_| {}).unwrap();
    assert_ne!(first.revision.id, second.revision.id);
    assert_eq!(before.nodes, after.nodes);
    assert_ne!(before.calls[0].id, after.calls[0].id);
    assert_eq!(before.files, after.files);
    assert!(
        after
            .calls
            .iter()
            .all(|c| c.provenance.semantic == SemanticState::Unavailable)
    );
}

#[test]
fn python_assignment_rhs_calls_remain_owned_by_enclosing_function() {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        "main.py",
        "def run():\n    agent = make_agent()\n    x: annotation_call() = rhs()\n",
    );
    let graph = run(&IndexOptions::new(d.path().to_owned()));
    let function = graph.nodes.iter().find(|n| n.name == "run").unwrap();
    let variable = graph.nodes.iter().find(|n| n.name == "agent").unwrap();
    assert_eq!(variable.parent.as_deref(), Some(function.id.as_str()));
    for name in ["make_agent", "rhs"] {
        let call = graph
            .calls
            .iter()
            .find(|c| c.callee_text.as_deref() == Some(name))
            .unwrap();
        assert_eq!(
            call.caller, function.id,
            "{name} must execute in defining scope"
        );
    }
}

#[test]
fn rust_impl_and_trait_methods_keep_measured_method_kind_and_free_functions() {
    use baleyg::{indexer::index_workspace_with_native, store::topology::WorkspaceIdentity};
    let d = tempfile::tempdir().unwrap();
    let source = "trait Work { fn required(&self); fn defaulted(&self) { fallback(); } }\nstruct T; impl Work for T { fn required(&self) { helper(); } }\nfn free() { helper(); }\n";
    write(d.path(), "main.rs", source);
    let identity = WorkspaceIdentity::discover(Some(d.path()), d.path()).unwrap();
    let (graph, native) = index_workspace_with_native(
        &IndexOptions::new(d.path().to_owned()),
        &identity.record_id,
        &cancel(),
        |_| {},
    )
    .unwrap();
    let methods: Vec<_> = graph
        .nodes
        .iter()
        .filter(|n| ["required", "defaulted"].contains(&n.name.as_str()))
        .collect();
    assert_eq!(methods.len(), 3);
    assert!(methods.iter().all(|node| node.kind == SymbolKind::Method));
    for node in methods {
        assert_eq!(
            native
                .declarations
                .iter()
                .find(|d| d.syntax_id == node.id)
                .unwrap()
                .kind,
            "method"
        );
        assert!(
            node.parent
                .as_ref()
                .is_some_and(|id| graph.nodes.iter().any(|n| n.id == *id))
        );
    }
    let free = graph.nodes.iter().find(|n| n.name == "free").unwrap();
    assert_eq!(free.kind, SymbolKind::Function);
    assert_eq!(
        native
            .declarations
            .iter()
            .find(|d| d.syntax_id == free.id)
            .unwrap()
            .kind,
        "function"
    );
    for name in ["fallback", "helper"] {
        assert!(
            graph
                .calls
                .iter()
                .any(|c| c.callee_text.as_deref() == Some(name))
        );
    }
}

#[test]
fn initializer_calls_keep_executable_owner_and_control_guard_in_four_languages() {
    use baleyg::{indexer::index_workspace_with_native, store::topology::WorkspaceIdentity};
    let d = tempfile::tempdir().unwrap();
    for (path, source) in [
        (
            "T.java",
            "class T { void run() { if (true) { int x = rhs(); } } }",
        ),
        ("t.js", "function run() { if (true) { let x = rhs(); } }"),
        ("t.py", "def run():\n    if True:\n        x = rhs()\n"),
        ("t.rs", "fn run() { if true { let x = rhs(); } }"),
    ] {
        write(d.path(), path, source);
    }
    let identity = WorkspaceIdentity::discover(Some(d.path()), d.path()).unwrap();
    let (graph, native) = index_workspace_with_native(
        &IndexOptions::new(d.path().to_owned()),
        &identity.record_id,
        &cancel(),
        |_| {},
    )
    .unwrap();
    for path in ["T.java", "t.js", "t.py", "t.rs"] {
        let function = graph
            .nodes
            .iter()
            .find(|n| n.path == path && n.name == "run")
            .unwrap();
        let call = graph
            .calls
            .iter()
            .find(|c| c.path == path && c.callee_text.as_deref() == Some("rhs"))
            .unwrap();
        assert_eq!(
            call.caller, function.id,
            "initializer call owner for {path}"
        );
        let native_call = native.calls.iter().find(|c| c.id == call.id).unwrap();
        assert_eq!(
            native_call.owner_syntax_id, function.id,
            "native owner for {path}"
        );
        assert!(
            !native_call.region_ids.is_empty(),
            "guard region for {path}"
        );
        assert_eq!(call.regions, native_call.region_ids);
        for id in &call.regions {
            assert!(
                graph
                    .regions
                    .iter()
                    .any(|region| region.id == *id && region.owner == function.id)
            );
        }
    }
}

#[test]
fn native_null_callee_spelling_remains_explicit_json_null() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "T.java", "class T { void f() { new T(); } }");
    let graph = run(&IndexOptions::new(d.path().to_owned()));
    let call = graph.calls.iter().find(|c| c.path == "T.java").unwrap();
    assert_eq!(call.callee_text, None);
    assert_eq!(call.callee_range, None);
    let serialized = serde_json::to_value(call).unwrap();
    assert!(serialized.get("calleeText").unwrap().is_null());
    assert!(serialized.get("calleeRange").unwrap().is_null());
    let mut missing = serialized.as_object().unwrap().clone();
    missing.remove("calleeText");
    assert!(serde_json::from_value::<CallSite>(serde_json::Value::Object(missing)).is_err());
}

#[test]
fn old_lexical_callsite_fields_are_rejected_on_deserialization() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "main.js", "function f() { foo(); }");
    let call = run(&IndexOptions::new(d.path().to_owned())).calls.remove(0);
    let mut value = serde_json::to_value(&call).unwrap();
    for unsafe_field in [
        "target",
        "candidateSymbols",
        "resolution",
        "callbackArguments",
    ] {
        value
            .as_object_mut()
            .unwrap()
            .insert(unsafe_field.into(), serde_json::Value::Null);
        assert!(
            serde_json::from_value::<CallSite>(value.clone()).is_err(),
            "accepted {unsafe_field}"
        );
        value.as_object_mut().unwrap().remove(unsafe_field);
    }
}

#[test]
fn untrusted_display_metadata_never_promotes_graph_edges_or_ids() {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "main.js", "function same() {} same();");
    let base = run(&IndexOptions::new(d.path().to_owned()));
    write(d.path(), "metadata.scip", "not a valid scip artifact");
    write(d.path(), "metadata.hashes", "{\"main.js\":\"forged\"}");
    let mut options = IndexOptions::new(d.path().to_owned());
    options.scip_path = Some(d.path().join("metadata.scip"));
    options.manifest_path = Some(d.path().join("metadata.hashes"));
    let with_metadata = run(&options);
    assert_eq!(base.nodes, with_metadata.nodes);
    assert_eq!(base.calls, with_metadata.calls);
    assert!(
        with_metadata
            .calls
            .iter()
            .all(|c| c.provenance.semantic == SemanticState::Unavailable)
    );
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
    assert!(
        index_workspace(&o, &cancel(), |_| {})
            .unwrap_err()
            .to_string()
            .contains("oversized input")
    );
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
#[cfg(unix)]
#[test]
fn direct_symlink_artifacts_and_config_are_never_read() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    write(p, "main.js", "f();");
    write(p, "index.scip", "captured optional metadata");
    let mut o = IndexOptions::new(p.to_owned());
    std::os::unix::fs::symlink(p.join("main.js"), p.join("package.json")).unwrap();
    assert!(index_workspace(&o, &cancel(), |_| {}).is_err());
    fs::remove_file(p.join("package.json")).unwrap();
    std::os::unix::fs::symlink(p.join("index.scip"), p.join("linked.scip")).unwrap();
    o.scip_path = Some(p.join("linked.scip"));
    assert!(index_workspace(&o, &cancel(), |_| {}).is_err());
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
    assert!(
        index_workspace(&IndexOptions::new(p.to_owned()), &cancel(), |_| {})
            .unwrap_err()
            .to_string()
            .contains("unsafe source")
    );
    fs::remove_file(p.join("link.py")).unwrap();
    fs::remove_file(p.join("Link.java")).unwrap();
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

#[test]
fn capture_rejects_cutoff_source_and_directory_inventory_drift() {
    for scenario in [
        "write",
        "add",
        "delete",
        "rename",
        "add-dir",
        "delete-dir",
        "config",
        "ignore",
        "same-size-mtime",
    ] {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        write(root, "main.js", "f();");
        fs::create_dir(root.join("visited")).unwrap();
        write(root, "visited/other.py", "pass\n");
        let err = index_workspace(&IndexOptions::new(root.to_owned()), &cancel(), |p| {
            if p.phase != "parse" || p.completed != 1 {
                return;
            }
            match scenario {
                "write" => write(root, "main.js", "g();"),
                "add" => write(root, "new.py", "pass"),
                "delete" => fs::remove_file(root.join("main.js")).unwrap(),
                "rename" => fs::rename(root.join("main.js"), root.join("renamed.js")).unwrap(),
                "add-dir" => fs::create_dir(root.join("another")).unwrap(),
                "delete-dir" => {
                    fs::remove_file(root.join("visited/other.py")).unwrap();
                    fs::remove_dir(root.join("visited")).unwrap();
                }
                "config" => write(root, "package.json", "{}"),
                "ignore" => write(root, "visited/.ignore", "*.js\n"),
                "same-size-mtime" => {
                    use std::os::unix::fs::MetadataExt;
                    let path = root.join("main.js");
                    let before = fs::metadata(&path).unwrap();
                    write(root, "main.js", "g();");
                    let times = [
                        libc::timespec {
                            tv_sec: before.atime(),
                            tv_nsec: before.atime_nsec(),
                        },
                        libc::timespec {
                            tv_sec: before.mtime(),
                            tv_nsec: before.mtime_nsec(),
                        },
                    ];
                    use std::os::unix::ffi::OsStrExt;
                    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                    assert_eq!(
                        unsafe {
                            libc::utimensat(libc::AT_FDCWD, name.as_ptr(), times.as_ptr(), 0)
                        },
                        0
                    );
                    assert_eq!(fs::metadata(path).unwrap().mtime(), before.mtime());
                }
                _ => unreachable!(),
            }
        })
        .unwrap_err();
        assert!(err.to_string().contains("drift"), "{scenario}: {err:#}");
    }
}

#[test]
fn capture_rejects_absent_present_and_cancellation_but_accepts_empty_root() {
    let d = tempfile::tempdir().unwrap();
    let empty = run(&IndexOptions::new(d.path().to_owned()));
    assert!(empty.files.is_empty());
    for initially_present in [false, true] {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        write(root, "main.js", "f();");
        if initially_present {
            write(root, "Cargo.toml", "[package]\n");
        }
        let error = index_workspace(&IndexOptions::new(root.to_owned()), &cancel(), |p| {
            if p.phase == "parse" {
                if initially_present {
                    fs::remove_file(root.join("Cargo.toml")).unwrap();
                } else {
                    write(root, "Cargo.toml", "[package]\n");
                }
            }
        })
        .unwrap_err();
        assert!(error.to_string().contains("drift"), "{error:#}");
    }
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "main.js", "f();");
    let flag = cancel();
    let error = index_workspace(&IndexOptions::new(d.path().to_owned()), &flag, |p| {
        if p.phase == "parse" {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    })
    .unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error:#}");
}

#[test]
fn identity_aliases_and_nonregular_sources_fail_admission() {
    for (alias, field) in [
        ("lexical", "scip"),
        ("hardlink", "scip"),
        ("lexical", "manifest"),
        ("hardlink", "manifest"),
    ] {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "main.js", "f();");
        let path = if alias == "lexical" {
            d.path().join("./main.js")
        } else {
            let alias = d.path().join("display.scip");
            fs::hard_link(d.path().join("main.js"), &alias).unwrap();
            alias
        };
        let mut options = IndexOptions::new(d.path().to_owned());
        if field == "scip" {
            options.scip_path = Some(path);
        } else {
            options.manifest_path = Some(path);
        }
        let error = index_workspace(&options, &cancel(), |_| {}).unwrap_err();
        assert!(
            error.to_string().contains("aliases source"),
            "{alias}/{field}: {error:#}"
        );
    }
    let d = tempfile::tempdir().unwrap();
    let fifo = d.path().join("blocked.js");
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let error =
        index_workspace(&IndexOptions::new(d.path().to_owned()), &cancel(), |_| {}).unwrap_err();
    assert!(error.to_string().contains("unsafe source"), "{error:#}");
}

#[test]
fn stable_capture_counts_one_open_read_hash_per_source_and_verifies_unchanged() {
    use baleyg::capture::{Capture, SourceOperations};
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "one.js", "f();");
    write(d.path(), "two.py", "def f(): pass\n");
    let cancel = cancel();
    let capture =
        Capture::admit(&IndexOptions::new(d.path().to_owned()), &cancel, &|_| {}).unwrap();
    assert_eq!(capture.source_operations.len(), 2);
    for operations in capture.source_operations.values() {
        assert_eq!(
            *operations,
            SourceOperations {
                opens: 1,
                complete_reads: 1,
                hashes: 1
            }
        );
    }
    capture.verify(&cancel).unwrap();
    assert_eq!(run(&IndexOptions::new(d.path().to_owned())).files.len(), 2);
}

#[test]
fn declared_inputs_and_root_refuse_cutoff_drift() {
    for scenario in [
        "root",
        "root-replaced",
        "toolchain",
        "config",
        "ignore",
        "display",
        "manifest",
    ] {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        write(root, "main.js", "f();");
        write(root, "rust-toolchain", "stable\n");
        write(root, "package.json", "{}\n");
        write(root, ".ignore", "*.tmp\n");
        write(root, "display.scip", "invalid-scip");
        write(root, "display.hashes", "{}\n");
        let mut options = IndexOptions::new(root.to_owned());
        options.scip_path = Some(root.join("display.scip"));
        options.manifest_path = Some(root.join("display.hashes"));
        let moved = root.with_extension("moved");
        let error = index_workspace(&options, &cancel(), |progress| {
            if progress.phase != "parse" {
                return;
            }
            match scenario {
                "root" => fs::rename(root, &moved).unwrap(),
                "root-replaced" => {
                    fs::rename(root, &moved).unwrap();
                    fs::create_dir(root).unwrap();
                }
                "toolchain" => write(root, "rust-toolchain", "nightly\n"),
                "config" => write(root, "package.json", "[]\n"),
                "ignore" => write(root, ".ignore", "*.log\n"),
                "display" => write(root, "display.scip", "changed-scip"),
                "manifest" => write(root, "display.hashes", "[]\n"),
                _ => unreachable!(),
            }
        })
        .unwrap_err();
        if scenario == "root" {
            fs::rename(&moved, root).unwrap();
        }
        if scenario == "root-replaced" {
            fs::remove_dir(root).unwrap();
            fs::rename(&moved, root).unwrap();
        }
        assert!(error.to_string().contains("drift"), "{scenario}: {error:#}");
    }
}

#[test]
fn hard_linked_nonsource_inputs_share_immutable_bytes() {
    use baleyg::capture::Capture;
    let d = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(d.path()).unwrap();
    let root = root.as_path();
    write(root, "main.js", "f();");
    write(root, "package.json", "{}\n");
    fs::hard_link(root.join("package.json"), root.join("display.scip")).unwrap();
    let mut options = IndexOptions::new(root.to_owned());
    options.scip_path = Some(root.join("display.scip"));
    let flag = cancel();
    let capture = Capture::admit(&options, &flag, &|_| {}).unwrap();
    assert_eq!(
        capture.bytes(&root.join("package.json")),
        Some(b"{}\n".as_slice())
    );
    assert_eq!(
        capture.bytes(&root.join("display.scip")),
        capture.bytes(&root.join("package.json"))
    );
    capture.verify(&flag).unwrap();
}

#[test]
fn four_language_noncallable_native_declarations_keep_exact_kind_and_are_not_executable() {
    use baleyg::{indexer::index_workspace_with_native, store::topology::WorkspaceIdentity};
    let d = tempfile::tempdir().unwrap();
    let samples = [
        (
            "main.js",
            "class Js { field = 1; method(arg) { const local = arg; return local; } }\nfunction jsFun(arg) { return arg; }\n",
        ),
        (
            "Main.java",
            "class JavaThing { int field; void method(int arg) { int local = arg; } }\n",
        ),
        (
            "main.py",
            "class PythonThing:\n    field = 1\n    def method(self, arg):\n        local = arg\n        return local\n",
        ),
        (
            "main.rs",
            "type Alias = u32;\nstruct RustThing { field: Alias }\nfn rust_fun(arg: Alias) { let local = arg; }\n",
        ),
    ];
    for (path, text) in samples {
        write(d.path(), path, text);
    }
    let root_id = WorkspaceIdentity::discover(Some(d.path()), d.path())
        .unwrap()
        .record_id;
    let (graph, native) = index_workspace_with_native(
        &IndexOptions::new(d.path().to_owned()),
        &root_id,
        &cancel(),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        graph.nodes.len(),
        native.declarations.len(),
        "native declarations must remain visible"
    );
    for declaration in &native.declarations {
        let node = graph
            .nodes
            .iter()
            .find(|n| n.id == declaration.syntax_id)
            .unwrap();
        let source = graph.files.iter().find(|f| f.path == node.path).unwrap();
        assert_eq!(
            (node.range.start_byte, node.range.end_byte),
            (declaration.range.start, declaration.range.end)
        );
        assert_eq!(
            &source.text[node.range.start_byte..node.range.end_byte],
            &source.text[declaration.range.start..declaration.range.end]
        );
        let expected = match declaration.kind.as_str() {
            "module" | "namespace" => SymbolKind::Module,
            "type" | "implementation" => SymbolKind::Class,
            "function" | "anonymousFunction" => SymbolKind::Function,
            "method" | "constructor" => SymbolKind::Method,
            "field" => SymbolKind::Field,
            "variable" => SymbolKind::Variable,
            "parameter" => SymbolKind::Parameter,
            "typeParameter" => SymbolKind::TypeParameter,
            "alias" => SymbolKind::Alias,
            other => panic!("unexpected native declaration kind: {other}"),
        };
        assert_eq!(node.kind, expected, "{} {}", node.path, node.name);
        assert_eq!(node.provenance.semantic, SemanticState::Unavailable);
    }
    // These are witnessed #22 declarations, not names inferred from the graph.
    for (path, expected) in [
        (
            "main.js",
            [
                ("field", SymbolKind::Field),
                ("local", SymbolKind::Variable),
                ("method", SymbolKind::Method),
            ],
        ),
        (
            "Main.java",
            [
                ("field", SymbolKind::Field),
                ("arg", SymbolKind::Parameter),
                ("local", SymbolKind::Variable),
            ],
        ),
        (
            "main.py",
            [
                ("field", SymbolKind::Variable),
                ("local", SymbolKind::Variable),
                ("method", SymbolKind::Function),
            ],
        ),
        (
            "main.rs",
            [
                ("Alias", SymbolKind::Alias),
                ("field", SymbolKind::Field),
                ("arg", SymbolKind::Parameter),
            ],
        ),
    ] {
        for (name, kind) in expected {
            let declaration = native
                .declarations
                .iter()
                .find(|d| d.document.path == path && d.name.as_deref() == Some(name))
                .unwrap();
            let node = graph
                .nodes
                .iter()
                .find(|n| n.id == declaration.syntax_id)
                .unwrap();
            assert_eq!(node.kind, kind, "{path} {name}");
        }
        assert!(
            graph
                .nodes
                .iter()
                .any(|n| n.path == path
                    && matches!(n.kind, SymbolKind::Function | SymbolKind::Method)),
            "{path}: real callable"
        );
    }
    let state = tempfile::tempdir().unwrap();
    let store = baleyg::store::Store::open_for_tests(state.path(), d.path()).unwrap();
    let pin = store
        .publish(
            &graph,
            &store.leader().unwrap(),
            store.index_baseline().unwrap(),
            &cancel(),
        )
        .unwrap();
    let saved = store.graph().unwrap();
    assert_eq!(saved.nodes, graph.nodes);
    assert_eq!(saved.calls, graph.calls);
    for (path, text) in samples {
        let methods = store.methods_at(path, Some(pin)).unwrap().unwrap();
        let items = methods["items"].as_array().unwrap();
        let expected: Vec<_> = graph
            .nodes
            .iter()
            .filter(|n| {
                n.path == path && matches!(n.kind, SymbolKind::Function | SymbolKind::Method)
            })
            .collect();
        assert_eq!(items.len(), expected.len(), "{path}");
        let files = store.files_at(Some(pin), 0, 100).unwrap();
        let file = files["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["path"] == path)
            .unwrap();
        assert_eq!(file["methodCount"], expected.len());
        for n in graph.nodes.iter().filter(|n| {
            n.path == path
                && matches!(
                    n.kind,
                    SymbolKind::Field
                        | SymbolKind::Variable
                        | SymbolKind::Parameter
                        | SymbolKind::Alias
                        | SymbolKind::TypeParameter
                )
        }) {
            assert_eq!(store.symbol(&n.id).unwrap().unwrap().kind, n.kind);
            assert!(
                store
                    .symbols(&n.name, 150)
                    .unwrap()
                    .iter()
                    .any(|candidate| candidate.id == n.id && candidate.kind == n.kind)
            );
            assert!(
                !items.iter().any(|item| item["symbol"]["id"] == n.id),
                "{path}: {} is not a method",
                n.name
            );
            assert!(
                store.sequence_at(&n.id, pin, false).is_err(),
                "{path}: {} is not executable",
                n.name
            );
            let request = serde_json::from_value(serde_json::json!({
                "seed": n.id, "question": "Which exact source declares this name?", "expectedRevision": pin
            })).unwrap();
            let packet = baleyg::planning::prepare(&store, request).unwrap();
            assert_eq!(
                packet.context.nodes,
                vec![n.clone()],
                "{path}: source-only declaration"
            );
            assert!(
                packet.context.calls.is_empty() && packet.context.regions.is_empty(),
                "{path}: no executable call evidence"
            );
            assert_eq!(packet.source_files.len(), 1);
            assert_eq!(packet.source_files[0].text, text);
            assert!(
                packet
                    .warnings
                    .iter()
                    .any(|w| w.contains("source-only") && w.contains("noncallable"))
            );
        }
        assert_eq!(store.source(path).unwrap().unwrap().text, text);
    }
}
