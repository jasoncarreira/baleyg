use baleyg::native_ids::{IdentityRegistry, canonical};
use serde_json::json;

#[test]
fn identity_excludes_body_and_revision_but_occurrence_is_revision_local() {
    let descriptor = json!({"sourceSet":"core", "path":"src/A.java", "language":"java", "ancestors":[],
        "declaration":{"kind":"method", "name":"run", "signature":{"parameterTypes":[],"typeParameterCount":0,"variadic":false},"ordinal":0}});
    let mut registry = IdentityRegistry::default();
    let syntax = registry.stable(&descriptor).unwrap();
    assert_eq!(syntax, "sid:v1:42d51f619d3e03c37a1629b6a01af34a");
    let first = registry
        .occurrence(&json!({"revisionId":"r1","ownerSyntaxId":syntax,"kind":"call","ordinal":0}))
        .unwrap();
    let second = registry
        .occurrence(&json!({"revisionId":"r2","ownerSyntaxId":syntax,"kind":"call","ordinal":0}))
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(
        first,
        registry
            .occurrence(
                &json!({"revisionId":"r1","ownerSyntaxId":syntax,"kind":"call","ordinal":0})
            )
            .unwrap()
    );
    assert_ne!(
        first,
        registry
            .occurrence(
                &json!({"revisionId":"r1","ownerSyntaxId":syntax,"kind":"control","ordinal":0})
            )
            .unwrap()
    );
    let changed = json!({"sourceSet":"core", "path":"src/A.java", "language":"java", "ancestors":[],
        "declaration":{"kind":"method", "name":"changed", "signature":{"parameterTypes":[],"typeParameterCount":0,"variadic":false},"ordinal":0}});
    assert_ne!(syntax, registry.stable(&changed).unwrap());
}

#[test]
fn canonical_controls_are_six_ascii_bytes_and_unicode_is_not_normalized() {
    let bytes = canonical(&json!({"z":"e\u{301}","a":"é\n\\\"\u{2028}"}));
    assert_eq!(
        std::str::from_utf8(&bytes).unwrap(),
        "{\"a\":\"é\\u000a\\\\\\\"\u{2028}\",\"z\":\"e\u{301}\"}"
    );
    assert_ne!(canonical(&json!("é")), canonical(&json!("e\u{301}")));
}

#[test]
fn four_languages_and_empty_workspace_share_closed_native_memory() {
    use baleyg::{
        indexer::{IndexOptions, index_workspace_with_native},
        model::CancelFlag,
    };
    use std::{
        fs,
        sync::{Arc, atomic::AtomicBool},
    };
    let root = tempfile::tempdir().unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let identity =
        baleyg::store::topology::WorkspaceIdentity::discover(Some(root.path()), root.path())
            .unwrap();
    let (_, empty) = index_workspace_with_native(
        &IndexOptions::new(root.path().into()),
        &identity.record_id,
        &cancel,
        |_| {},
    )
    .unwrap();
    assert_eq!(empty.revision.documents.len(), 0);
    assert!(empty.coverage.is_empty() && empty.provenance.is_empty());
    for (path, source) in [
        ("A.java", "class A { void go() { obj.foo(); } }"),
        ("a.js", "function go() { obj.foo(); }"),
        ("a.py", "def go():\n    obj.foo()\n"),
        ("a.rs", "fn go() { obj.foo(); }"),
    ] {
        fs::write(root.path().join(path), source).unwrap();
    }
    let (graph, native) = index_workspace_with_native(
        &IndexOptions::new(root.path().into()),
        &identity.record_id,
        &cancel,
        |_| {},
    )
    .unwrap();
    assert_eq!(graph.files.len(), 4);
    assert_eq!(native.coverage.len(), 4);
    assert_eq!(native.provenance.len(), 4);
    for lang in ["java", "javascript", "python", "rust"] {
        let call = native
            .calls
            .iter()
            .find(|c| c.document.language == lang && c.spelling.as_deref() == Some("foo"))
            .unwrap_or_else(|| panic!("no measured foo for {lang}: {:?}", native.calls));
        let file = graph
            .files
            .iter()
            .find(|f| f.path == call.document.path)
            .unwrap();
        let callee = call.callee_range.as_ref().unwrap();
        assert_eq!(&file.text[callee.start..callee.end], "foo");
    }
}

#[test]
fn unicode_lookup_normalizes_without_changing_exact_identity_bytes() {
    use baleyg::native_evidence::lookup;
    assert_eq!(lookup("rust", "e\u{301}").unwrap(), "é");
    assert_eq!(lookup("python", "Ａ").unwrap(), "A");
    assert_eq!(lookup("java", "e\u{301}").unwrap(), "e\u{301}");
    assert_eq!(lookup("javascript", "Ａ").unwrap(), "Ａ");
}

#[test]
fn closed_records_reject_unknown_fields_and_fabricated_semantics() {
    use baleyg::native_evidence::{Call, Coverage, Provenance};
    use serde_json::json;
    assert!(serde_json::from_value::<Call>(json!({"id":"occ:v1:dead","ownerSyntaxId":"sid:v1:dead","ordinal":0,"document":{"sourceSetId":"s","language":"rust","path":"a.rs"},"revisionId":"r","range":{"start":0,"end":1},"calleeRange":null,"spelling":null,"regionIds":[],"provenanceId":"p","target":"guessed"})).is_err());
    assert!(serde_json::from_value::<Provenance>(json!({"id":"p","producerId":"n","document":{"sourceSetId":"s","language":"rust","path":"a.rs"},"revisionId":"r","contentHash":"h","evidenceKind":"measuredSyntax","basis":null,"freshness":"fresh","derivedFrom":null,"receiver":"guessed"})).is_err());
    assert!(serde_json::from_value::<Coverage>(json!({"producerId":"n","language":"rust","sourceSetId":"s","documentPath":"a.rs","revisionId":"r","requested":true,"selected":true,"state":"complete","supportedRoles":["definition","call"],"observedRoles":[],"diagnostic":null,"reference":"inferred"})).is_err());
}

#[test]
fn source_body_only_changes_revision_not_stable_declaration() {
    use baleyg::{
        indexer::{IndexOptions, index_workspace_with_native},
        model::CancelFlag,
    };
    use std::{
        fs,
        sync::{Arc, atomic::AtomicBool},
    };
    let root = tempfile::tempdir().unwrap();
    let id = baleyg::store::topology::WorkspaceIdentity::discover(Some(root.path()), root.path())
        .unwrap();
    let source = root.path().join("a.py");
    fs::write(&source, "def greet():\n    return 1\n").unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let (_, before) = index_workspace_with_native(
        &IndexOptions::new(root.path().into()),
        &id.record_id,
        &cancel,
        |_| {},
    )
    .unwrap();
    fs::write(&source, "def greet():\n    return 2\n").unwrap();
    let (_, after) = index_workspace_with_native(
        &IndexOptions::new(root.path().into()),
        &id.record_id,
        &cancel,
        |_| {},
    )
    .unwrap();
    let declaration = |a: &baleyg::native_evidence::Artifact| {
        a.declarations
            .iter()
            .find(|d| d.name.as_deref() == Some("greet"))
            .unwrap()
            .syntax_id
            .clone()
    };
    assert_eq!(declaration(&before), declaration(&after));
    assert_ne!(before.revision.id, after.revision.id);
    assert_ne!(
        before.revision.documents[0].content_hash,
        after.revision.documents[0].content_hash
    );
}

#[test]
fn unverifiable_expression_keeps_independent_nullable_callee() {
    use baleyg::{
        indexer::{IndexOptions, index_workspace_with_native},
        model::CancelFlag,
    };
    use std::{
        fs,
        sync::{Arc, atomic::AtomicBool},
    };
    let root = tempfile::tempdir().unwrap();
    let id = baleyg::store::topology::WorkspaceIdentity::discover(Some(root.path()), root.path())
        .unwrap();
    fs::write(root.path().join("a.js"), "function go() { obj[key](); }").unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let (_graph, native) = index_workspace_with_native(
        &IndexOptions::new(root.path().into()),
        &id.record_id,
        &cancel,
        |_| {},
    )
    .unwrap();
    let call = native
        .calls
        .iter()
        .find(|call| call.spelling.as_deref() == Some("obj[key]"))
        .unwrap();
    assert_eq!(call.callee_range, None);
    assert_eq!(call.region_ids.len(), 0);
    assert!(!serde_json::to_string(call).unwrap().contains("target"));
    assert!(!serde_json::to_string(call).unwrap().contains("receiver"));
}

#[test]
fn nested_occurrences_sort_before_hashing_and_unverifiable_callees_remain_nullable() {
    use baleyg::{
        capture::Capture, indexer::IndexOptions, model::CancelFlag, native_evidence::from_capture,
    };
    use std::{
        fs,
        sync::{Arc, atomic::AtomicBool},
    };
    let root = tempfile::tempdir().unwrap();
    let identity =
        baleyg::store::topology::WorkspaceIdentity::discover(Some(root.path()), root.path())
            .unwrap();
    for (path, source) in [
        (
            "A.java",
            "class A { A() { this(1); } A(int n) {} void go() { obj.foo(); new A(); } }",
        ),
        (
            "a.js",
            "function go() { obj.foo(); obj[key](); foo()(); if (true) { foo(); } }",
        ),
        (
            "a.py",
            "def go():\n    obj.foo()\n    (foo())()\n    if True:\n        foo()\n",
        ),
        ("a.rs", "fn go() { obj.foo(); foo()(); if true { foo(); } }"),
    ] {
        fs::write(root.path().join(path), source).unwrap();
    }
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let capture = Capture::admit(&IndexOptions::new(root.path().into()), &cancel, &|_| {}).unwrap();
    let artifact = from_capture(&capture, root.path(), &identity.record_id).unwrap();
    artifact
        .validate(&capture, root.path(), &identity.record_id)
        .unwrap();
    // These are literal source expressions, not records regenerated as the test oracle.
    for (lang, path, unverified_expression, spelling) in [
        ("java", "A.java", "new A()", None),
        ("javascript", "a.js", "obj[key]()", Some("obj[key]")),
        ("python", "a.py", "(foo())()", Some("(foo())")),
        ("rust", "a.rs", "foo()()", Some("foo()")),
    ] {
        let source = &capture
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap()
            .text;
        let owner = artifact
            .declarations
            .iter()
            .find(|d| d.document.language == lang && d.name.as_deref() == Some("go"))
            .unwrap();
        let positive_at = source.find("obj.foo()").unwrap();
        let positive = artifact
            .calls
            .iter()
            .find(|call| call.document.language == lang && call.range.start == positive_at)
            .unwrap();
        assert_eq!(
            (positive.range.start, positive.range.end),
            (positive_at, positive_at + "obj.foo()".len())
        );
        assert_eq!(positive.spelling.as_deref(), Some("foo"));
        let token = positive.callee_range.as_ref().unwrap();
        assert_eq!((token.start, token.end), (positive_at + 4, positive_at + 7));
        assert_eq!(positive.owner_syntax_id, owner.syntax_id);
        let at = source.find(unverified_expression).unwrap();
        let unverified = artifact
            .calls
            .iter()
            .find(|call| {
                call.document.language == lang
                    && call.range.start == at
                    && call.range.end == at + unverified_expression.len()
            })
            .unwrap();
        assert_eq!(
            unverified.spelling.as_deref(),
            spelling,
            "{lang} full captured spelling"
        );
        assert_eq!(unverified.callee_range, None, "{lang} cannot infer a token");
        assert_eq!(
            unverified.owner_syntax_id, owner.syntax_id,
            "{lang} lexical owner"
        );
        assert_eq!(
            &source[unverified.range.start..unverified.range.end],
            unverified_expression
        );
        let wire = serde_json::to_value(unverified).unwrap();
        for field in [
            "receiver",
            "target",
            "hint",
            "candidateSymbols",
            "resolution",
            "binding",
            "symbol",
        ] {
            assert!(
                wire.get(field).is_none(),
                "{lang} fabricated semantic {field}"
            );
        }
    }
    let native_wire = serde_json::to_value(&artifact).unwrap();
    for field in ["references", "symbols", "bindings", "semanticProducer"] {
        assert!(
            native_wire.get(field).is_none(),
            "native artifact fabricated {field}"
        );
    }
    let js: Vec<_> = artifact
        .calls
        .iter()
        .filter(|c| {
            c.document.language == "javascript"
                && c.owner_syntax_id
                    == artifact
                        .declarations
                        .iter()
                        .find(|d| {
                            d.document.language == "javascript" && d.name.as_deref() == Some("go")
                        })
                        .unwrap()
                        .syntax_id
        })
        .collect();
    let nested: Vec<_> = js
        .iter()
        .filter(|c| {
            c.range.start
                == js
                    .iter()
                    .find(|c| c.spelling.as_deref() == Some("foo()"))
                    .unwrap()
                    .range
                    .start
        })
        .collect();
    assert_eq!(nested.len(), 2);
    assert!(nested[0].range.end != nested[1].range.end);
    let a = nested.iter().min_by_key(|c| c.range.end).unwrap();
    let b = nested.iter().max_by_key(|c| c.range.end).unwrap();
    assert!(
        a.ordinal < b.ordinal,
        "inner call must sort before outer at equal start"
    );
}

#[test]
fn every_nullable_native_field_requires_presence_even_when_null() {
    use baleyg::native_evidence::*;
    use serde::{Serialize, de::DeserializeOwned};
    fn check<T: Serialize + DeserializeOwned>(row: &T, fields: &[&str]) {
        let baseline = serde_json::to_value(row).unwrap();
        assert!(serde_json::from_value::<T>(baseline.clone()).is_ok());
        for field in fields {
            let mut missing = baseline.clone();
            missing.as_object_mut().unwrap().remove(*field).unwrap();
            assert!(
                serde_json::from_value::<T>(missing).is_err(),
                "missing {field} accepted"
            );
            let mut explicit_null = baseline.clone();
            explicit_null[*field] = serde_json::Value::Null;
            assert!(
                serde_json::from_value::<T>(explicit_null).is_ok(),
                "explicit null {field} rejected"
            );
        }
    }
    let doc = DocumentKey {
        source_set_id: "s".into(),
        language: "rust".into(),
        path: "a.rs".into(),
    };
    check(
        &Coverage {
            producer_id: "p".into(),
            language: "rust".into(),
            source_set_id: "s".into(),
            document_path: "a.rs".into(),
            revision_id: "r".into(),
            requested: true,
            selected: true,
            state: "complete".into(),
            supported_roles: vec![],
            observed_roles: vec![],
            diagnostic: None,
        },
        &["diagnostic"],
    );
    check(
        &Provenance {
            id: "proof".into(),
            producer_id: "p".into(),
            document: doc.clone(),
            revision_id: "r".into(),
            content_hash: "h".into(),
            evidence_kind: "measuredSyntax".into(),
            basis: None,
            freshness: "fresh".into(),
            derived_from: None,
        },
        &["basis", "derivedFrom"],
    );
    let key = Key {
        kind: "module".into(),
        name: None,
        signature: None,
        ordinal: 0,
    };
    check(&key, &["name", "signature"]);
    check(
        &Parameter {
            name: None,
            type_name: None,
            variadic: false,
        },
        &["name", "type"],
    );
    let header = Header {
        kind: "module".into(),
        name: None,
        modifiers: vec![],
        type_parameters: vec![],
        parameters: vec![],
        result_type: None,
        bases: vec![],
    };
    check(&header, &["name", "resultType"]);
    check(
        &Declaration {
            syntax_id: "id".into(),
            document: doc.clone(),
            revision_id: "r".into(),
            kind: "module".into(),
            name: None,
            lookup_key: None,
            ancestors: vec![],
            key,
            range: Range { start: 0, end: 0 },
            name_range: None,
            header,
            provenance_id: "proof".into(),
        },
        &["name", "lookupKey", "nameRange"],
    );
    check(
        &Call {
            id: "id".into(),
            owner_syntax_id: "owner".into(),
            ordinal: 0,
            document: doc.clone(),
            revision_id: "r".into(),
            range: Range { start: 0, end: 1 },
            callee_range: None,
            spelling: None,
            region_ids: vec![],
            provenance_id: "proof".into(),
        },
        &["calleeRange", "spelling"],
    );
    check(
        &ControlRegion {
            id: "id".into(),
            owner_syntax_id: "owner".into(),
            ordinal: 0,
            document: doc,
            revision_id: "r".into(),
            kind: "if_statement".into(),
            range: Range { start: 0, end: 1 },
            parent_id: None,
            arm: None,
            provenance_id: "proof".into(),
        },
        &["parentId", "arm"],
    );
}

#[test]
fn every_native_kind_rejects_well_shaped_forgery_against_immutable_capture() {
    use baleyg::{
        capture::Capture,
        indexer::IndexOptions,
        model::CancelFlag,
        native_evidence::{Artifact, from_capture},
    };
    use std::{
        fs,
        sync::{Arc, atomic::AtomicBool},
    };
    let root = tempfile::tempdir().unwrap();
    let id = baleyg::store::topology::WorkspaceIdentity::discover(Some(root.path()), root.path())
        .unwrap();
    fs::write(
        root.path().join("A.java"),
        "class A { void go(int n) { obj.foo(); } }",
    )
    .unwrap();
    fs::write(
        root.path().join("a.js"),
        "function go() { if (true) obj.foo(); }",
    )
    .unwrap();
    fs::write(root.path().join("a.py"), "def go():\n    obj.foo()\n").unwrap();
    fs::write(root.path().join("a.rs"), "fn go() { obj.foo(); }").unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname='fixture'\n",
    )
    .unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let capture = Capture::admit(&IndexOptions::new(root.path().into()), &cancel, &|_| {}).unwrap();
    let artifact = from_capture(&capture, root.path(), &id.record_id).unwrap();
    let bad_hash = "a".repeat(64);
    type Mutation = (&'static str, Box<dyn Fn(&mut Artifact)>);
    let mut mutations: Vec<Mutation> = vec![
        (
            "producer executable hash",
            Box::new({
                let h = bad_hash.clone();
                move |a| a.producer.executable_hash = h.clone()
            }),
        ),
        (
            "producer languages",
            Box::new(|a| a.producer.languages.swap(0, 1)),
        ),
        (
            "source set root",
            Box::new(|a| a.source_set.root_id = "forged".into()),
        ),
        (
            "source set dependencies",
            Box::new(|a| a.source_set.dependencies.push("forged".into())),
        ),
        (
            "revision id",
            Box::new({
                let h = bad_hash.clone();
                move |a| a.revision.id = format!("revision:v1:{h}")
            }),
        ),
        (
            "toolchain hash",
            Box::new({
                let h = bad_hash.clone();
                move |a| a.revision.toolchain_hash = h.clone()
            }),
        ),
        (
            "config hash",
            Box::new({
                let h = bad_hash.clone();
                move |a| a.revision.config_hash = h.clone()
            }),
        ),
        (
            "dependency hash",
            Box::new({
                let h = bad_hash.clone();
                move |a| a.revision.dependency_hash = h.clone()
            }),
        ),
        (
            "document content hash",
            Box::new({
                let h = bad_hash.clone();
                move |a| a.revision.documents[0].content_hash = h.clone()
            }),
        ),
        (
            "document bytes length",
            Box::new(|a| a.revision.documents[0].byte_length += 1),
        ),
        (
            "document order",
            Box::new(|a| a.revision.documents.swap(0, 1)),
        ),
        (
            "coverage complete roles",
            Box::new(|a| a.coverage[0].observed_roles.clear()),
        ),
        (
            "coverage failed with diagnostic",
            Box::new(|a| {
                a.coverage[0].state = "failed".into();
                a.coverage[0].diagnostic = Some("failed".into());
            }),
        ),
        (
            "coverage partial with diagnostic",
            Box::new(|a| {
                a.coverage[0].state = "partial".into();
                a.coverage[0].diagnostic = Some("partial".into());
            }),
        ),
        (
            "provenance proof document",
            Box::new(|a| a.provenance[0].document = a.provenance[1].document.clone()),
        ),
        (
            "provenance content hash",
            Box::new({
                let h = bad_hash.clone();
                move |a| a.provenance[0].content_hash = h.clone()
            }),
        ),
        (
            "provenance basis",
            Box::new(|a| a.provenance[0].basis = Some(serde_json::json!({}))),
        ),
        (
            "declaration modifier",
            Box::new(|a| {
                a.declarations
                    .iter_mut()
                    .find(|d| d.name.as_deref() == Some("go"))
                    .unwrap()
                    .header
                    .modifiers
                    .push("public".into())
            }),
        ),
        (
            "declaration result type",
            Box::new(|a| {
                a.declarations
                    .iter_mut()
                    .find(|d| d.document.language == "java" && d.name.as_deref() == Some("go"))
                    .unwrap()
                    .header
                    .result_type = Some("String".into())
            }),
        ),
        (
            "declaration signature",
            Box::new(|a| {
                a.declarations
                    .iter_mut()
                    .find(|d| d.document.language == "java" && d.name.as_deref() == Some("go"))
                    .unwrap()
                    .key
                    .signature
                    .as_mut()
                    .unwrap()
                    .parameter_types[0] = "String".into()
            }),
        ),
        (
            "declaration ancestor",
            Box::new(|a| {
                a.declarations
                    .iter_mut()
                    .find(|d| d.document.language == "java" && d.name.as_deref() == Some("go"))
                    .unwrap()
                    .ancestors
                    .clear()
            }),
        ),
        (
            "declaration name range",
            Box::new(|a| {
                a.declarations
                    .iter_mut()
                    .find(|d| d.name.as_deref() == Some("go"))
                    .unwrap()
                    .name_range
                    .as_mut()
                    .unwrap()
                    .start += 1
            }),
        ),
        (
            "declaration source range",
            Box::new(|a| {
                a.declarations
                    .iter_mut()
                    .find(|d| d.name.as_deref() == Some("go"))
                    .unwrap()
                    .range
                    .end -= 1
            }),
        ),
        (
            "call unverified spelling",
            Box::new(|a| {
                let c = a
                    .calls
                    .iter_mut()
                    .find(|c| c.document.language == "javascript")
                    .unwrap();
                c.spelling = Some("guessed".into());
                c.callee_range = None;
            }),
        ),
        (
            "call proof from other document",
            Box::new(|a| {
                a.calls
                    .iter_mut()
                    .find(|c| c.document.language == "javascript")
                    .unwrap()
                    .provenance_id = a.provenance[0].id.clone()
            }),
        ),
        ("call ordinal", Box::new(|a| a.calls[0].ordinal += 1)),
        (
            "call region",
            Box::new(|a| a.calls[0].region_ids.push("not-a-region".into())),
        ),
        (
            "control kind",
            Box::new(|a| a.control_regions[0].kind = "for_statement".into()),
        ),
        (
            "control arm",
            Box::new(|a| a.control_regions[0].arm = Some("fabricated".into())),
        ),
        (
            "control owner",
            Box::new(|a| {
                a.control_regions[0].owner_syntax_id = a.declarations[0].syntax_id.clone()
            }),
        ),
        (
            "control range",
            Box::new(|a| a.control_regions[0].range.end += 1),
        ),
    ];
    for (label, mutator) in mutations.drain(..) {
        let mut changed = artifact.clone();
        mutator(&mut changed);
        assert!(
            changed
                .validate(&capture, root.path(), &id.record_id)
                .is_err(),
            "forged {label} accepted"
        );
    }
    artifact
        .validate(&capture, root.path(), &id.record_id)
        .unwrap();
    for ops in capture.source_operations.values() {
        assert_eq!((ops.opens, ops.complete_reads, ops.hashes), (1, 1, 1));
    }
}

#[test]
fn parser_recovery_reports_partial_not_false_complete() {
    use baleyg::{
        capture::Capture, indexer::IndexOptions, model::CancelFlag, native_evidence::from_capture,
    };
    use std::{
        fs,
        sync::{Arc, atomic::AtomicBool},
    };
    let root = tempfile::tempdir().unwrap();
    let id = baleyg::store::topology::WorkspaceIdentity::discover(Some(root.path()), root.path())
        .unwrap();
    fs::write(root.path().join("broken.js"), "function broken( { foo(); }").unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let capture = Capture::admit(&IndexOptions::new(root.path().into()), &cancel, &|_| {}).unwrap();
    let artifact = from_capture(&capture, root.path(), &id.record_id).unwrap();
    assert_eq!(artifact.coverage.len(), 1);
    assert_eq!(artifact.coverage[0].state, "partial");
    assert!(artifact.coverage[0].diagnostic.is_some());
    let mut forged = artifact.clone();
    forged.coverage[0].state = "complete".into();
    forged.coverage[0].diagnostic = None;
    assert!(
        forged
            .validate(&capture, root.path(), &id.record_id)
            .is_err()
    );
}

#[test]
fn validation_refuses_cutoff_drift_without_source_reread() {
    use baleyg::{
        capture::Capture, indexer::IndexOptions, model::CancelFlag, native_evidence::from_capture,
    };
    use std::{
        fs,
        sync::{Arc, atomic::AtomicBool},
    };
    let root = tempfile::tempdir().unwrap();
    let id = baleyg::store::topology::WorkspaceIdentity::discover(Some(root.path()), root.path())
        .unwrap();
    let source = root.path().join("a.js");
    fs::write(&source, "function f() { foo(); }").unwrap();
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let capture = Capture::admit(&IndexOptions::new(root.path().into()), &cancel, &|_| {}).unwrap();
    let artifact = from_capture(&capture, root.path(), &id.record_id).unwrap();
    fs::write(&source, "function f() { bar(); }").unwrap();
    assert!(
        artifact
            .validate(&capture, root.path(), &id.record_id)
            .is_err()
    );
    assert_eq!(capture.source_operations["a.js"].complete_reads, 1);
    assert_eq!(capture.source_operations["a.js"].hashes, 1);
}

#[test]
fn source_grounded_four_language_headers_owners_and_control_regions() {
    use baleyg::{
        capture::Capture, indexer::IndexOptions, model::CancelFlag, native_evidence::from_capture,
    };
    use std::{
        fs,
        sync::{Arc, atomic::AtomicBool},
    };
    let root = tempfile::tempdir().unwrap();
    let id = baleyg::store::topology::WorkspaceIdentity::discover(Some(root.path()), root.path())
        .unwrap();
    for (path, source) in [
        (
            "A.java",
            "public class A { public static void go() { if (true) obj.foo(); } }",
        ),
        ("a.js", "async function go() { if (true) obj.foo(); }"),
        ("a.py", "async def go():\n    if True:\n        obj.foo()\n"),
        (
            "a.rs",
            "pub struct S {}\nstruct Plain {}\npub async fn go() { if true { obj.foo(); } }\nfn plain() {}",
        ),
    ] {
        fs::write(root.path().join(path), source).unwrap();
    }
    let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
    let capture = Capture::admit(&IndexOptions::new(root.path().into()), &cancel, &|_| {}).unwrap();
    let artifact = from_capture(&capture, root.path(), &id.record_id).unwrap();
    let rust_struct = artifact
        .declarations
        .iter()
        .find(|d| d.document.language == "rust" && d.name.as_deref() == Some("S"))
        .unwrap();
    assert_eq!(rust_struct.header.modifiers, ["pub"]);
    let rust_plain = artifact
        .declarations
        .iter()
        .find(|d| d.document.language == "rust" && d.name.as_deref() == Some("Plain"))
        .unwrap();
    assert!(
        rust_plain.header.modifiers.is_empty(),
        "private Rust struct must not inherit pub"
    );
    let plain_function = artifact
        .declarations
        .iter()
        .find(|d| d.document.language == "rust" && d.name.as_deref() == Some("plain"))
        .unwrap();
    assert!(
        plain_function.header.modifiers.is_empty(),
        "plain Rust fn must not inherit async/pub"
    );
    let java_type = artifact
        .declarations
        .iter()
        .find(|d| d.document.language == "java" && d.name.as_deref() == Some("A"))
        .unwrap();
    assert_eq!(java_type.header.modifiers, ["public"]);
    for (lang, path, modifiers, control_text, kind) in [
        (
            "java",
            "A.java",
            vec!["public", "static"],
            "if (true) obj.foo();",
            "if_statement",
        ),
        (
            "javascript",
            "a.js",
            vec!["async"],
            "if (true) obj.foo();",
            "if_statement",
        ),
        (
            "python",
            "a.py",
            vec!["async"],
            "if True:\n        obj.foo()",
            "if_statement",
        ),
        (
            "rust",
            "a.rs",
            vec!["pub", "async"],
            "if true { obj.foo(); }",
            "if_expression",
        ),
    ] {
        let source = &capture.files.iter().find(|f| f.path == path).unwrap().text;
        let owner = artifact
            .declarations
            .iter()
            .find(|d| d.document.language == lang && d.name.as_deref() == Some("go"))
            .unwrap();
        assert_eq!(
            owner.header.modifiers, modifiers,
            "{lang} literal header modifiers"
        );
        assert_eq!(
            &source[owner.range.start..owner.range.end].contains("go"),
            &true
        );
        if lang == "java" {
            assert_eq!(owner.header.result_type.as_deref(), Some("void"));
        }
        let begin = source.find(control_text).unwrap();
        let region = artifact
            .control_regions
            .iter()
            .find(|r| r.document.language == lang && r.kind == kind && r.range.start == begin)
            .unwrap_or_else(|| panic!("missing {lang} control: {:?}", artifact.control_regions));
        assert_eq!(
            (region.range.start, region.range.end),
            (begin, begin + control_text.len())
        );
        assert_eq!(region.owner_syntax_id, owner.syntax_id);
        let call_at = source.find("obj.foo()").unwrap();
        let call = artifact
            .calls
            .iter()
            .find(|c| c.document.language == lang && c.range.start == call_at)
            .unwrap();
        assert_eq!(call.owner_syntax_id, owner.syntax_id);
        assert!(call.region_ids.contains(&region.id));
        assert_eq!(call.spelling.as_deref(), Some("foo"));
        assert_eq!(call.callee_range.as_ref().unwrap().start, call_at + 4);
    }
}
