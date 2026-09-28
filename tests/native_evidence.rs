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
    native.validate(&graph.files).unwrap();
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
    let (graph, native) = index_workspace_with_native(
        &IndexOptions::new(root.path().into()),
        &id.record_id,
        &cancel,
        |_| {},
    )
    .unwrap();
    native.validate(&graph.files).unwrap();
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
