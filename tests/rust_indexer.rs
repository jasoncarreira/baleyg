mod common;
use baleyg::{
    indexer::{IndexOptions, index_workspace},
    model::*,
};
use protobuf::Message;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    sync::{Arc, atomic::AtomicBool},
};
fn run(o: &IndexOptions) -> Graph {
    index_workspace(o, &Arc::new(AtomicBool::new(false)), |_| {}).unwrap()
}
fn hash(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}
#[test]
fn mixed_discovery_exact_ranges_owners_macros_and_utf8() {
    let d = tempfile::tempdir().unwrap();
    let source = r#"mod child { pub fn café() { outer(inner()); } }
struct Thing;
trait Work { fn signature(&self); fn defaulted(&self) { fallback(); } }
impl Work for Thing { fn signature(&self) { invoke((|x| closure_call(x))); self.next(); } }
macro_rules! generated { () => { fn invisible() { hidden(); } } }
#[cfg(feature = "unknown")]
fn configured() { opaque!(not_a_call()); let c = || separate(); after(); }
"#;
    fs::write(d.path().join("lib.rs"), source).unwrap();
    fs::write(d.path().join("script.js"), "function js() { jsCall(); }").unwrap();
    fs::create_dir(d.path().join("target")).unwrap();
    fs::write(d.path().join("target/ignored.rs"), "fn ignored() {}").unwrap();
    let o = IndexOptions::new(d.path().to_owned());
    let g = run(&o);
    assert_eq!(g, run(&o));
    assert_eq!(g.stats.files, 2);
    assert_eq!(g.stats.parse_error_files, 0);
    assert_eq!(
        g.files
            .iter()
            .find(|f| f.path == "lib.rs")
            .unwrap()
            .language,
        "rust"
    );
    let cafe = g.nodes.iter().find(|n| n.name == "café").unwrap();
    assert_eq!(
        &source[cafe.range.start_byte..cafe.range.end_byte],
        "pub fn café() { outer(inner()); }"
    );
    assert_eq!(cafe.kind, SymbolKind::Function);
    let calls: Vec<_> = g.calls.iter().filter(|c| c.caller == cafe.id).collect();
    assert_eq!(
        calls
            .iter()
            .map(|c| (c.callee_text.as_str(), c.ordinal))
            .collect::<Vec<_>>(),
        [("outer", 1), ("inner", 2)]
    );
    let methods: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.kind == SymbolKind::Method)
        .collect();
    assert_eq!(methods.len(), 2); // no callable fabricated for bodyless trait signature
    assert_eq!(methods.iter().filter(|n| n.name == "signature").count(), 1);
    let closure_call = g
        .calls
        .iter()
        .find(|c| c.callee_text == "closure_call")
        .unwrap();
    let closure = g
        .nodes
        .iter()
        .find(|n| n.id == closure_call.caller)
        .unwrap();
    assert_eq!(
        &source[closure.range.start_byte..closure.range.end_byte],
        "|x| closure_call(x)"
    );
    let invoke = g.calls.iter().find(|c| c.callee_text == "invoke").unwrap();
    assert_eq!(invoke.callback_arguments, std::slice::from_ref(&closure.id));
    assert_ne!(invoke.caller, closure_call.caller);
    assert!(!g.nodes.iter().any(|n| n.name == "invisible"));
    assert!(
        !g.calls
            .iter()
            .any(|c| matches!(c.callee_text.as_str(), "hidden" | "not_a_call" | "opaque"))
    );
    let separate = g
        .calls
        .iter()
        .find(|c| c.callee_text == "separate")
        .unwrap();
    let after = g.calls.iter().find(|c| c.callee_text == "after").unwrap();
    assert_ne!(separate.caller, after.caller);
    assert!(
        g.calls
            .iter()
            .filter(|c| c.path == "lib.rs")
            .all(|c| c.resolution == Resolution::Unresolved
                && c.provenance.semantic == SemanticState::Unavailable)
    );
    assert!(g.diagnostics.iter().any(|d| d.code == "rust-lexical-only"));
}
#[test]
fn recovered_rust_retains_syntax_and_cached_source() {
    let d = tempfile::tempdir().unwrap();
    let source = "fn healthy() { café(); }\nfn broken( {";
    fs::write(d.path().join("bad.rs"), source).unwrap();
    let g = run(&IndexOptions::new(d.path().to_owned()));
    assert_eq!(g.stats.parse_error_files, 1);
    assert!(g.nodes.iter().any(|n| n.name == "healthy"));
    fs::remove_file(d.path().join("bad.rs")).unwrap();
    assert_eq!(g.files[0].text, source);
    assert!(
        g.calls
            .iter()
            .all(|c| c.provenance.semantic == SemanticState::Unavailable)
    );
}
#[test]
fn cargo_hash_inputs_and_fresh_scip_do_not_resolve_rust() {
    let d = tempfile::tempdir().unwrap();
    let source = "fn run() { external(); }";
    let manifest = "[package]\nname = 'untrusted'\nversion = '0.0.0'\nbuild = 'build.rs'\n";
    let lock = "# lexical hash input only\n";
    let build = "fn main() { panic!(\"must never execute\"); }";
    let inputs = [
        ("lib.rs", source),
        ("Cargo.toml", manifest),
        ("Cargo.lock", lock),
        ("build.rs", build),
    ];
    let mut hashes = BTreeMap::new();
    for (path, text) in inputs {
        fs::write(d.path().join(path), text).unwrap();
        hashes.insert(path, hash(text));
    }
    let mut index = scip::types::Index::new();
    let mut doc = scip::types::Document::new();
    doc.relative_path = "lib.rs".into();
    let mut occurrence = scip::types::Occurrence::new();
    occurrence.range = vec![0, 11, 19];
    occurrence.symbol = "scip fake package 1 external().".into();
    doc.occurrences.push(occurrence);
    index.documents.push(doc);
    fs::write(d.path().join("index.scip"), index.write_to_bytes().unwrap()).unwrap();
    fs::write(
        d.path().join("manifest.json"),
        serde_json::to_vec(&hashes).unwrap(),
    )
    .unwrap();
    let mut o = IndexOptions::new(d.path().to_owned());
    o.scip_path = Some(d.path().join("index.scip"));
    o.manifest_path = Some(d.path().join("manifest.json"));
    let g = run(&o);
    assert_eq!(g.stats.semantic_state, SemanticState::Fresh);
    assert!(g.calls.iter().all(|c| c.candidate_symbols.is_empty()
        && c.target.is_none()
        && c.provenance.semantic == SemanticState::Unavailable));
    assert!(
        g.nodes
            .iter()
            .all(|n| n.provenance.semantic == SemanticState::Unavailable)
    );
    fs::write(d.path().join("Cargo.lock"), "changed").unwrap();
    fs::write(d.path().join("Cargo.toml"), "changed").unwrap();
    let stale = run(&o);
    assert_eq!(stale.stats.semantic_state, SemanticState::Stale);
    assert_eq!(stale.stats.changed_files, ["Cargo.lock", "Cargo.toml"]);
}

#[test]
fn chained_calls_have_unique_range_ids_and_publish() {
    let d = tempfile::tempdir().unwrap();
    let source = "fn run() { receiver().write(inner(first()), second()); factory().method(other()); std::fs::write(path(), data()).unwrap(); }";
    fs::write(d.path().join("lib.rs"), source).unwrap();
    let options = IndexOptions::new(d.path().to_owned());
    let graph = run(&options);
    let ids: std::collections::BTreeSet<_> = graph.calls.iter().map(|c| &c.id).collect();
    assert_eq!(ids.len(), graph.calls.len());
    assert_eq!(graph, run(&options));
    assert!(graph.calls.iter().any(|a| {
        graph.calls.iter().any(|b| {
            a.range.start_byte == b.range.start_byte && a.range.end_byte != b.range.end_byte
        })
    }));
    let state = tempfile::tempdir().unwrap();
    let store = crate::common::open_store(&state.path().join("state"), d.path()).unwrap();
    store
        .publish(
            &graph,
            &store.leader().unwrap(),
            baleyg::model::IndexPin {
                index_generation: store.status().unwrap().revision.index_generation,
                index_revision: 0,
            },
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
}

fn captured(root: &std::path::Path) -> baleyg::indexer::CapturedRevision {
    use baleyg::{
        indexer::{CaptureAdmission, capture_revision},
        model::v1::Language,
    };
    let identity =
        baleyg::store::topology::WorkspaceIdentity::discover_unattached(Some(root), root).unwrap();
    for name in ["toolchain.capture", "config.capture", "dependency.capture"] {
        fs::write(root.join(name), b"fixture").unwrap();
    }
    capture_revision(
        &IndexOptions::new(root.to_owned()),
        &CaptureAdmission {
            source_set_id: identity.record_id,
            root_id: identity.root_key,
            languages: vec![Language::Rust],
            toolchain: root.join("toolchain.capture"),
            config: root.join("config.capture"),
            dependency: root.join("dependency.capture"),
            dependency_source_sets: vec![],
            producers: vec![],
        },
        &Arc::new(AtomicBool::new(false)),
    )
    .unwrap()
}
#[test]
fn rust_member_token_is_measured_not_inferred() {
    use baleyg::indexer::NativeCandidateKind;
    let d = tempfile::tempdir().unwrap();
    let source = "struct Café; impl Café { fn r#type(&self) {} } fn run() { obj.r#type(); (obj.field)(1); factory().write(2); let f = || inner(); }";
    fs::write(d.path().join("lib.rs"), source).unwrap();
    let capture = captured(d.path());
    let doc = &capture.documents[0];
    assert!(doc.native_candidates.iter().any(|w| {
        w.node_kind == "closure_expression"
            && w.candidate_kind == NativeCandidateKind::Declaration
            && w.stable_id
                .as_deref()
                .is_some_and(|id| id.starts_with("sid:v1:"))
    }));
    let methods: Vec<_> = doc
        .native_candidates
        .iter()
        .filter(|w| {
            w.candidate_kind == NativeCandidateKind::Invocation
                && w.node_kind == "call_expression"
                && w.verified_member_token
        })
        .collect();
    assert_eq!(methods.len(), 2);
    for member in methods {
        assert!(member.stable_id.as_deref().unwrap().starts_with("occ:v1:"));
        assert!(member.verified_member_token);
        assert_eq!(
            &source[member.token_start_byte..member.token_end_byte],
            member
                .token_bytes
                .as_slice()
                .iter()
                .map(|&b| b as char)
                .collect::<String>()
        );
        assert_eq!(
            member.spelling.as_deref(),
            Some(if member.token_bytes == b"r#type" {
                "type"
            } else {
                "write"
            })
        );
    }
    let compound = doc
        .native_candidates
        .iter()
        .find(|w| {
            w.candidate_kind == NativeCandidateKind::Invocation
                && source[w.start_byte..w.end_byte].starts_with("(obj.field)(1)")
        })
        .unwrap();
    assert!(!compound.verified_member_token);
    assert!(compound.spelling.is_none());
    let graph = run(&IndexOptions::new(d.path().to_owned()));
    assert!(graph.calls.iter().all(|c| c.id.starts_with("occ:v1:")
        && c.target.is_none()
        && c.candidate_symbols.is_empty()));
    assert!(graph.nodes.iter().all(|n| n.id.starts_with("sid:v1:")));
}

#[test]
fn rust_declarations_keep_ids_across_body_edits_and_exact_names() {
    use baleyg::indexer::NativeCandidateKind;
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("lib.rs");
    let before = "fn café() { first(); } fn cafe\u{301}() { first(); } fn same() { first(); } fn same() { first(); }";
    fs::write(&path, before).unwrap();
    let first = captured(d.path());
    let declarations = |capture: &baleyg::indexer::CapturedRevision| {
        capture.documents[0]
            .native_candidates
            .iter()
            .filter(|w| {
                w.candidate_kind == NativeCandidateKind::Declaration
                    && w.node_kind == "function_item"
            })
            .map(|w| (w.name_bytes.clone(), w.stable_id.clone().unwrap()))
            .collect::<Vec<_>>()
    };
    let a = declarations(&first);
    assert_eq!(a.len(), 4);
    assert_ne!(a[0].1, a[1].1);
    assert_ne!(a[2].1, a[3].1);
    assert!(a.iter().all(|(_, id)| id.starts_with("sid:v1:")));
    let altered = before.replace("first();", "second();");
    fs::write(&path, altered).unwrap();
    let second = captured(d.path());
    assert_eq!(a, declarations(&second));
    let calls = |capture: &baleyg::indexer::CapturedRevision| {
        capture.documents[0]
            .native_candidates
            .iter()
            .filter(|w| w.candidate_kind == NativeCandidateKind::Invocation)
            .map(|w| w.stable_id.clone().unwrap())
            .collect::<Vec<_>>()
    };
    assert_ne!(calls(&first), calls(&second));
    assert_eq!(
        run(&IndexOptions::new(d.path().to_owned()))
            .nodes
            .iter()
            .filter(|n| n.name == "same")
            .count(),
        2
    );
}

#[test]
fn rust_canonical_ancestors_and_lookup_keys_are_measured() {
    use baleyg::{
        indexer::NativeCandidateKind,
        model::v1::{Key, Kind, Language, Path, Text, UInt},
        semantic_identity,
    };
    let d = tempfile::tempdir().unwrap();
    let source = "fn café() {} fn cafe\u{301}() {} fn r#type() {} mod inner { fn café() {} fn café() {} } fn run() { obj.r#type(); }";
    fs::write(d.path().join("lib.rs"), source).unwrap();
    let snapshot = captured(d.path());
    let doc = &snapshot.documents[0];
    let source_set = Text::new(snapshot.source_set_id.clone()).unwrap();
    let path = Path::new("lib.rs".to_owned()).unwrap();
    let key = |name: &str, ordinal: u64| Key {
        kind: Kind::Function,
        name: Some(Text::new(name.to_owned()).unwrap()),
        signature: None,
        ordinal: UInt::new(ordinal).unwrap(),
    };
    let declarations: Vec<_> = doc
        .native_candidates
        .iter()
        .filter(|w| {
            w.candidate_kind == NativeCandidateKind::Declaration && w.node_kind == "function_item"
        })
        .collect();
    let named = |name: &str| {
        declarations
            .iter()
            .filter(|w| w.name_bytes == name.as_bytes())
            .copied()
            .collect::<Vec<_>>()
    };
    let nfc = named("café");
    let nfd = named("cafe\u{301}");
    assert_eq!(nfc.len(), 3);
    assert_eq!(nfd.len(), 1);
    assert_eq!(nfc[0].lookup_key.as_deref(), Some("café"));
    assert_eq!(nfd[0].lookup_key.as_deref(), Some("café"));
    assert_ne!(nfc[0].stable_id, nfd[0].stable_id);
    let top =
        semantic_identity::syntax_id(&source_set, &path, Language::Rust, &[], &key("café", 0))
            .unwrap();
    assert_eq!(nfc[0].stable_id.as_deref(), Some(top.as_str()));
    let module = Key {
        kind: Kind::Module,
        name: None,
        signature: None,
        ordinal: UInt::new(0).unwrap(),
    };
    let synthetic = semantic_identity::syntax_id(
        &source_set,
        &path,
        Language::Rust,
        std::slice::from_ref(&module),
        &key("café", 0),
    )
    .unwrap();
    assert_ne!(top, synthetic);
    let nested_parent = Key {
        kind: Kind::Type,
        name: Some(Text::new("inner".to_owned()).unwrap()),
        signature: None,
        ordinal: UInt::new(0).unwrap(),
    };
    for (ordinal, declaration) in nfc[1..].iter().enumerate() {
        let expected = semantic_identity::syntax_id(
            &source_set,
            &path,
            Language::Rust,
            std::slice::from_ref(&nested_parent),
            &key("café", ordinal as u64),
        )
        .unwrap();
        assert_eq!(declaration.stable_id.as_deref(), Some(expected.as_str()));
        assert_ne!(declaration.stable_id.as_deref(), Some(synthetic.as_str()));
    }
    let digest =
        semantic_identity::syntax_digest(&source_set, &path, Language::Rust, &[], &key("café", 0))
            .unwrap();
    assert!(
        std::str::from_utf8(&digest.input)
            .unwrap()
            .contains("\"ancestors\":[]")
    );
    assert_eq!(top.as_str(), format!("sid:v1:{}", &digest.sha256[..32]));
    let raw = named("r#type");
    assert_eq!(raw[0].lookup_key.as_deref(), Some("type"));
    let member = doc
        .native_candidates
        .iter()
        .find(|w| w.candidate_kind == NativeCandidateKind::Invocation && w.verified_member_token)
        .unwrap();
    assert_eq!(member.token_bytes, b"r#type");
    assert_eq!(member.spelling.as_deref(), Some("type"));
    assert_eq!(member.lookup_key.as_deref(), Some("type"));
    assert!(member.stable_id.as_deref().unwrap().starts_with("occ:v1:"));
}
