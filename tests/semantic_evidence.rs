use baleyg::{
    indexer::{CaptureAdmission, CapturedRevision, IndexOptions, capture_revision},
    model::v1::*,
    semantic_evidence::{EvidenceError, validate_capture, validate_evidence, validate_native},
};
use protobuf::Message;
use sha2::{Digest, Sha256};
use std::{
    fs,
    sync::{Arc, atomic::AtomicBool},
};

fn text(s: &str) -> Text {
    Text::new(s).unwrap()
}
fn hash(bytes: &[u8]) -> Hash {
    Hash::new(hex::encode(Sha256::digest(bytes))).unwrap()
}
fn fixture() -> (tempfile::TempDir, CapturedRevision, Evidence) {
    fixture_with_sources(None, None, None)
}
fn fixture_with_sources(
    javascript: Option<&str>,
    python: Option<&str>,
    rust: Option<&str>,
) -> (tempfile::TempDir, CapturedRevision, Evidence) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("java/src")).unwrap();
    fs::create_dir_all(root.join("rust/src")).unwrap();
    fs::write(
        root.join("java/src/A.java"),
        "public class Child extends Base { public <T> void foo(int x) { if (true) { é(); } } public <T> void foo(int y) {} }
",
    )
    .unwrap();
    fs::write(
        root.join("rust/src/B.rs"),
        rust.unwrap_or("fn b() { let _ = \"😀\"; }\n"),
    )
    .unwrap();
    if let Some(source) = javascript {
        fs::create_dir_all(root.join("javascript/src")).unwrap();
        fs::write(root.join("javascript/src/C.js"), source).unwrap();
    }
    if let Some(source) = python {
        fs::create_dir_all(root.join("python/src")).unwrap();
        fs::write(root.join("python/src/D.py"), source).unwrap();
    }
    let mut languages = vec![Language::Java, Language::Rust];
    if javascript.is_some() {
        languages.push(Language::Javascript);
    }
    if python.is_some() {
        languages.push(Language::Python);
    }
    for name in ["toolchain.capture", "config.capture", "dependency.capture"] {
        fs::write(root.join(name), name).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            root.join("toolchain.capture"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let identity =
        baleyg::store::topology::WorkspaceIdentity::discover_unattached(Some(root), root).unwrap();
    let mut index = scip::types::Index::new();
    let mut metadata = scip::types::Metadata::new();
    let mut tool = scip::types::ToolInfo::new();
    tool.name = "scip-test".into();
    tool.version = "1".into();
    metadata.tool_info = protobuf::MessageField::some(tool);
    index.metadata = protobuf::MessageField::some(metadata);
    fs::write(
        root.join("semantic.artifact"),
        index.write_to_bytes().unwrap(),
    )
    .unwrap();
    let admission = CaptureAdmission {
        source_set_id: identity.record_id,
        root_id: identity.root_key,
        languages: languages.clone(),
        toolchain: root.join("toolchain.capture"),
        config: root.join("config.capture"),
        dependency: root.join("dependency.capture"),
        dependency_source_sets: vec![],
        producers: vec![baleyg::indexer::ProducerInput {
            id: "S".into(),
            tool_name: "scip-test".into(),
            version: "1".into(),
            position_encoding: "utf8".into(),
            executable: root.join("toolchain.capture"),
            artifact: Some(root.join("semantic.artifact")),
        }],
    };
    let mut options = IndexOptions::new(root.to_owned());
    options.scip_path = Some(root.join("semantic.artifact"));
    let capture =
        capture_revision(&options, &admission, &Arc::new(AtomicBool::new(false))).unwrap();
    let producer = |id: &str, kind| Producer {
        id: text(id),
        version: text(
            &capture
                .producers
                .iter()
                .find(|p| p.id == id)
                .unwrap()
                .version,
        ),
        executable_hash: hash(
            &capture
                .producers
                .iter()
                .find(|p| p.id == id)
                .unwrap()
                .executable_bytes,
        ),
        kind,
        languages: languages.clone(),
        position_encoding: PositionEncoding::Utf8,
    };
    let documents = capture
        .documents
        .iter()
        .map(|d| Document {
            key: d.key.clone(),
            revision_id: text(&capture.revision_id),
            content_hash: hash(&d.bytes),
            byte_length: UInt::new(d.bytes.len() as u64).unwrap(),
        })
        .collect();
    let mut coverage = vec![];
    for id in ["N", "S"] {
        for d in &capture.documents {
            coverage.push(Coverage {
                producer_id: text(id),
                language: d.key.language,
                source_set_id: text(&capture.source_set_id),
                document_path: d.key.path.clone(),
                revision_id: text(&capture.revision_id),
                requested: true,
                selected: id == "N",
                state: if id == "N" {
                    CoverageState::Complete
                } else {
                    CoverageState::Omitted
                },
                supported_roles: vec![],
                observed_roles: vec![],
                diagnostic: (id == "S").then(|| text("producer omitted")),
            });
        }
    }
    let evidence = Evidence {
        context: NativeRevisionContext {
            source_set: SourceSet {
                id: text(&capture.source_set_id),
                root_id: text(&capture.root_id),
                languages: languages.clone(),
                dependencies: vec![],
            },
            revision: Revision {
                id: text(&capture.revision_id),
                source_set_id: text(&capture.source_set_id),
                documents,
                toolchain_hash: hash(&capture.toolchain_bytes),
                config_hash: hash(&capture.config_bytes),
                dependency_hash: hash(&capture.dependency_bytes),
            },
            producer: producer("N", ProducerKind::Native),
        },
        native_files: vec![],
        producers: vec![producer("S", ProducerKind::Semantic)],
        coverage,
        provenance: vec![],
        symbols: vec![],
        declaration_bindings: vec![],
        type_relationships: vec![],
        references: vec![],
        call_bindings: vec![],
    };
    (dir, capture, evidence)
}
#[test]
fn producer() {
    let (_d, capture, evidence) = fixture();
    assert!(validate_capture(&capture, &evidence).is_ok());
    assert!(matches!(
        validate_evidence(&capture, &evidence, &capture),
        Err(EvidenceError::NotYetValidated(_))
    ));
    let mut bad = evidence.clone();
    bad.producers[0].executable_hash = hash(b"forged");
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Producer(_))
    ));
    let mut missing = capture.clone();
    missing.producers[1].artifact_bytes = None;
    assert!(matches!(
        validate_evidence(&missing, &evidence, &missing),
        Err(EvidenceError::Producer(_))
    ));
}
#[test]
fn source_set() {
    let (_d, capture, evidence) = fixture();
    assert!(
        validate_capture(&capture, &evidence).is_ok(),
        "{:?}",
        validate_capture(&capture, &evidence)
    );
    let mut bad = evidence.clone();
    bad.context.source_set.dependencies = vec![text("unknown")];
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::SourceSet(_))
    ));
}
#[test]
fn revision() {
    let (_d, capture, evidence) = fixture();
    assert!(
        validate_capture(&capture, &evidence).is_ok(),
        "{:?}",
        validate_capture(&capture, &evidence)
    );
    let mut bad = evidence.clone();
    bad.context.revision.documents[1] = bad.context.revision.documents[0].clone();
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Revision(_) | EvidenceError::Document(_))
    ));
    let mut changed = capture.clone();
    changed.toolchain_bytes.push(b'!');
    changed.toolchain_hash = hex::encode(Sha256::digest(&changed.toolchain_bytes));
    let mut matching = evidence.clone();
    matching.context.revision.toolchain_hash = Hash::new(changed.toolchain_hash.clone()).unwrap();
    assert!(matches!(
        validate_evidence(&changed, &matching, &changed),
        Err(EvidenceError::Revision(_))
    ));
}
#[test]
fn document() {
    let (_d, capture, evidence) = fixture();
    assert!(
        validate_capture(&capture, &evidence).is_ok(),
        "{:?}",
        validate_capture(&capture, &evidence)
    );
    let mut bad = evidence.clone();
    bad.context.revision.documents[0].byte_length = UInt::new(1).unwrap();
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Document(_))
    ));
}
#[test]
fn coverage() {
    let (_d, capture, evidence) = fixture();
    assert!(validate_capture(&capture, &evidence).is_ok());
    assert!(matches!(
        validate_evidence(&capture, &evidence, &capture),
        Err(EvidenceError::NotYetValidated(_))
    ));
    let mut partial = evidence.clone();
    let selected = partial
        .coverage
        .iter_mut()
        .find(|c| c.producer_id.as_str() == "S")
        .unwrap();
    selected.selected = true;
    selected.state = CoverageState::Partial;
    assert!(validate_capture(&capture, &partial).is_ok());
    let mut bad = partial.clone();
    let failed = bad
        .coverage
        .iter_mut()
        .find(|c| c.producer_id.as_str() == "S")
        .unwrap();
    failed.state = CoverageState::Failed;
    failed.diagnostic = None;
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Coverage(_))
    ));
    let mut bad = partial.clone();
    let failed = bad
        .coverage
        .iter_mut()
        .find(|c| c.producer_id.as_str() == "S")
        .unwrap();
    failed.state = CoverageState::Failed;
    failed.selected = false;
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Coverage(_))
    ));
    let mut bad = evidence.clone();
    bad.coverage.pop();
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Coverage(_))
    ));
}

#[test]
fn capture_only_cannot_publish_full_evidence() {
    let (_dir, capture, evidence) = fixture();
    assert_eq!(validate_capture(&capture, &evidence), Ok(()));
    assert!(matches!(
        validate_evidence(&capture, &evidence, &capture),
        Err(EvidenceError::NotYetValidated(_))
    ));
}

#[test]
fn coverage_six_states_and_role_admission() {
    let (_dir, capture, evidence) = fixture();
    for (state, requested, selected, diagnostic) in [
        (CoverageState::NotRequested, false, false, false),
        (CoverageState::Omitted, true, false, true),
        (CoverageState::Unsupported, true, false, true),
        (CoverageState::Failed, true, true, true),
        (CoverageState::Partial, true, true, true),
        (CoverageState::Complete, true, true, false),
    ] {
        let mut valid = evidence.clone();
        let row = &mut valid.coverage[0];
        row.state = state;
        row.requested = requested;
        row.selected = selected;
        row.diagnostic = diagnostic.then(|| text("reason"));
        row.supported_roles = vec![Role::Definition, Role::Read];
        row.observed_roles = vec![Role::Read];
        assert_eq!(validate_capture(&capture, &valid), Ok(()), "{state:?}");
        let mut wrong_tuple = valid.clone();
        wrong_tuple.coverage[0].selected = !selected;
        assert!(
            matches!(
                validate_capture(&capture, &wrong_tuple),
                Err(EvidenceError::Coverage(_))
            ),
            "{state:?}"
        );
        let mut wrong_diagnostic = valid.clone();
        wrong_diagnostic.coverage[0].diagnostic = (!diagnostic).then(|| text("reason"));
        assert!(
            matches!(
                validate_capture(&capture, &wrong_diagnostic),
                Err(EvidenceError::Coverage(_))
            ),
            "{state:?}"
        );
    }
    let mut bad = evidence.clone();
    bad.coverage[0].supported_roles = vec![Role::Read, Role::Definition];
    assert!(matches!(
        validate_capture(&capture, &bad),
        Err(EvidenceError::Coverage(_))
    ));
    bad.coverage[0].supported_roles = vec![Role::Read, Role::Read];
    assert!(matches!(
        validate_capture(&capture, &bad),
        Err(EvidenceError::Coverage(_))
    ));
    bad.coverage[0].supported_roles = vec![Role::Definition, Role::Read];
    bad.coverage[0].observed_roles = vec![Role::Read, Role::Definition];
    assert!(matches!(
        validate_capture(&capture, &bad),
        Err(EvidenceError::Coverage(_))
    ));
    bad.coverage[0].observed_roles = vec![Role::Write];
    assert!(matches!(
        validate_capture(&capture, &bad),
        Err(EvidenceError::Coverage(_))
    ));
    let java = evidence
        .coverage
        .iter()
        .position(|row| row.language == Language::Java)
        .unwrap();
    for observed in [false, true] {
        let mut bad = evidence.clone();
        bad.coverage[java].supported_roles = vec![Role::Alias];
        if observed {
            bad.coverage[java].observed_roles = vec![Role::Alias];
        }
        assert!(matches!(
            validate_capture(&capture, &bad),
            Err(EvidenceError::Coverage(_))
        ));
    }
    let rust = evidence
        .coverage
        .iter()
        .position(|row| row.language == Language::Rust)
        .unwrap();
    let mut valid = evidence.clone();
    valid.coverage[rust].supported_roles = vec![Role::Alias];
    valid.coverage[rust].observed_roles = vec![Role::Alias];
    assert_eq!(validate_capture(&capture, &valid), Ok(()));
}

#[test]
fn captured_root_document_hash_and_manifest_are_immutable() {
    let (_dir, capture, evidence) = fixture();
    assert_eq!(validate_capture(&capture, &evidence), Ok(()));
    let mut forged = evidence.clone();
    forged.context.source_set.root_id = text("forged-root");
    assert!(matches!(
        validate_capture(&capture, &forged),
        Err(EvidenceError::SourceSet(_))
    ));
    let mut forged = capture.clone();
    forged.root_id = "forged-root".into();
    assert!(matches!(
        validate_capture(&forged, &evidence),
        Err(EvidenceError::SourceSet(_))
    ));
    let mut wrong = evidence.clone();
    wrong.context.revision.documents[0].content_hash = hash(b"different source bytes");
    assert!(matches!(
        validate_capture(&capture, &wrong),
        Err(EvidenceError::Document(_))
    ));
    let mut changed = capture.clone();
    changed.documents[0].bytes.push(b'!');
    assert!(matches!(
        validate_capture(&changed, &evidence),
        Err(EvidenceError::Document(_) | EvidenceError::Revision(_))
    ));
    let mut changed = capture.clone();
    changed.manifest_bytes.push(b' ');
    assert!(matches!(
        validate_capture(&changed, &evidence),
        Err(EvidenceError::Revision(_))
    ));
}

fn native_file(
    capture: &CapturedRevision,
    evidence: &Evidence,
    index: usize,
) -> NativeFileEvidence {
    let doc = &capture.documents[index];
    let admitted = evidence
        .context
        .revision
        .documents
        .iter()
        .find(|d| d.key == doc.key)
        .unwrap();
    let coverage = evidence
        .coverage
        .iter()
        .find(|c| c.producer_id.as_str() == "N" && c.document_path == doc.key.path)
        .unwrap();
    NativeFileEvidence {
        document: admitted.clone(),
        coverage: coverage.clone(),
        provenance: Provenance {
            id: text(&format!("native-{}", index)),
            producer_id: text("N"),
            document: doc.key.clone(),
            revision_id: text(&capture.revision_id),
            content_hash: admitted.content_hash.clone(),
            evidence_kind: EvidenceKind::MeasuredSyntax,
            basis: None,
            freshness: Freshness::Fresh,
        },
        declarations: vec![],
        calls: vec![],
        control_regions: vec![],
        diagnostics: vec![],
    }
}
fn complete_native_file(
    capture: &CapturedRevision,
    evidence: &Evidence,
    index: usize,
) -> NativeFileEvidence {
    let mut file = native_file(capture, evidence, index);
    let doc = &capture.documents[index];
    let module = Key {
        kind: Kind::Module,
        name: None,
        signature: None,
        ordinal: UInt::new(0).unwrap(),
    };
    let class_key = Key {
        kind: Kind::Type,
        name: Some(text("Child")),
        signature: None,
        ordinal: UInt::new(0).unwrap(),
    };
    let method_signature = Signature {
        parameter_types: vec![text("int")],
        type_parameter_count: UInt::new(1).unwrap(),
        variadic: false,
    };
    let declaration_witnesses: Vec<_> = doc
        .native_candidates
        .iter()
        .filter(|w| {
            w.stable_id.is_some()
                && w.candidate_kind == baleyg::indexer::NativeCandidateKind::Declaration
        })
        .collect();
    for witness in declaration_witnesses {
        let (kind, name, key, ancestors, header) =
            match (doc.key.language, witness.node_kind.as_str()) {
                (Language::Java, "class_declaration") => (
                    Kind::Type,
                    text("Child"),
                    class_key.clone(),
                    vec![module.clone()],
                    Header {
                        kind: Kind::Type,
                        name: Some(text("Child")),
                        modifiers: vec![text("public")],
                        type_parameters: vec![],
                        parameters: vec![],
                        result_type: None,
                        bases: vec![text("Base")],
                    },
                ),
                (Language::Java, "method_declaration") => {
                    let ordinal = (witness.start_byte > 84) as u64;
                    let parameter = if ordinal == 0 { "x" } else { "y" };
                    (
                        Kind::Method,
                        text("foo"),
                        Key {
                            kind: Kind::Method,
                            name: Some(text("foo")),
                            signature: Some(method_signature.clone()),
                            ordinal: UInt::new(ordinal).unwrap(),
                        },
                        vec![module.clone(), class_key.clone()],
                        Header {
                            kind: Kind::Method,
                            name: Some(text("foo")),
                            modifiers: vec![text("public")],
                            type_parameters: vec![text("T")],
                            parameters: vec![Parameter {
                                name: Some(text(parameter)),
                                r#type: Some(text("int")),
                                variadic: false,
                            }],
                            result_type: Some(text("void")),
                            bases: vec![],
                        },
                    )
                }
                (Language::Javascript, "function_declaration")
                | (Language::Python, "function_definition") => {
                    let name = text("f");
                    (
                        Kind::Function,
                        name.clone(),
                        Key {
                            kind: Kind::Function,
                            name: Some(name.clone()),
                            signature: None,
                            ordinal: UInt::new(0).unwrap(),
                        },
                        if doc.key.language == Language::Javascript {
                            vec![module.clone()]
                        } else {
                            vec![]
                        },
                        Header {
                            kind: Kind::Function,
                            name: Some(name),
                            modifiers: vec![],
                            type_parameters: vec![],
                            parameters: vec![],
                            result_type: None,
                            bases: vec![],
                        },
                    )
                }
                (Language::Rust, "function_item") => (
                    Kind::Function,
                    text("b"),
                    Key {
                        kind: Kind::Function,
                        name: Some(text("b")),
                        signature: None,
                        ordinal: UInt::new(0).unwrap(),
                    },
                    vec![],
                    Header {
                        kind: Kind::Function,
                        name: Some(text("b")),
                        modifiers: vec![],
                        type_parameters: vec![],
                        parameters: vec![],
                        result_type: None,
                        bases: vec![],
                    },
                ),
                _ => panic!("unsupported fixture declaration: {}", witness.node_kind),
            };
        file.declarations.push(Declaration {
            syntax_id: SyntaxId::new(witness.stable_id.clone().unwrap()).unwrap(),
            document: doc.key.clone(),
            revision_id: text(&capture.revision_id),
            kind,
            name: Some(name.clone()),
            lookup_key: Some(name),
            ancestors,
            key,
            range: span(witness.start_byte, witness.end_byte),
            name_range: Some(span(witness.token_start_byte, witness.token_end_byte)),
            header,
            provenance_id: file.provenance.id.clone(),
        });
    }
    if doc.key.language == Language::Java {
        let method = file
            .declarations
            .iter()
            .find(|d| d.kind == Kind::Method && d.key.ordinal.get() == 0)
            .unwrap()
            .syntax_id
            .clone();
        let region = doc
            .native_candidates
            .iter()
            .find(|w| w.node_kind == "if_statement" && w.stable_id.is_some())
            .unwrap();
        let invocation = doc
            .native_candidates
            .iter()
            .find(|w| w.node_kind == "method_invocation" && w.stable_id.is_some())
            .unwrap();
        let region_id = OccurrenceId::new(region.stable_id.clone().unwrap()).unwrap();
        file.control_regions.push(ControlRegion {
            id: region_id.clone(),
            owner_syntax_id: method.clone(),
            ordinal: UInt::new(0).unwrap(),
            document: doc.key.clone(),
            revision_id: text(&capture.revision_id),
            kind: text(&region.node_kind),
            range: span(region.start_byte, region.end_byte),
            parent_id: None,
            arm: None,
            provenance_id: file.provenance.id.clone(),
        });
        file.calls.push(Call {
            id: OccurrenceId::new(invocation.stable_id.clone().unwrap()).unwrap(),
            owner_syntax_id: method,
            ordinal: UInt::new(0).unwrap(),
            document: doc.key.clone(),
            revision_id: text(&capture.revision_id),
            range: span(invocation.start_byte, invocation.end_byte),
            callee_range: invocation
                .verified_member_token
                .then(|| span(invocation.token_start_byte, invocation.token_end_byte)),
            spelling: invocation.spelling.as_deref().map(text),
            region_ids: vec![region_id],
            provenance_id: file.provenance.id.clone(),
        });
    }
    if matches!(
        doc.key.language,
        Language::Javascript | Language::Python | Language::Rust
    ) {
        let owner = file.declarations[0].syntax_id.clone();
        let mut regions: Vec<_> = doc
            .native_candidates
            .iter()
            .filter(|w| {
                w.stable_id.is_some()
                    && w.candidate_kind == baleyg::indexer::NativeCandidateKind::ControlRegion
            })
            .collect();
        regions.sort_by_key(|w| (w.start_byte, w.end_byte));
        for (ordinal, region) in regions.iter().enumerate() {
            let parent = regions
                .iter()
                .filter(|p| {
                    p.node_id != region.node_id
                        && p.start_byte <= region.start_byte
                        && region.end_byte <= p.end_byte
                })
                .min_by_key(|p| p.end_byte - p.start_byte);
            let field = doc.syntax[region.node_id]
                .field_name
                .as_deref()
                .filter(|f| matches!(*f, "consequence" | "alternative" | "right" | "value"));
            file.control_regions.push(ControlRegion {
                id: OccurrenceId::new(region.stable_id.clone().unwrap()).unwrap(),
                owner_syntax_id: owner.clone(),
                ordinal: UInt::new(ordinal as u64).unwrap(),
                document: doc.key.clone(),
                revision_id: text(&capture.revision_id),
                kind: text(&region.node_kind),
                range: span(region.start_byte, region.end_byte),
                parent_id: parent.map(|w| OccurrenceId::new(w.stable_id.clone().unwrap()).unwrap()),
                arm: field.map(text),
                provenance_id: file.provenance.id.clone(),
            });
        }
        let mut calls: Vec<_> = doc
            .native_candidates
            .iter()
            .filter(|w| {
                w.stable_id.is_some()
                    && w.candidate_kind == baleyg::indexer::NativeCandidateKind::Invocation
            })
            .collect();
        calls.sort_by_key(|w| (w.start_byte, w.end_byte));
        for (ordinal, call) in calls.iter().enumerate() {
            let mut enclosing: Vec<_> = file
                .control_regions
                .iter()
                .filter(|r| {
                    r.range.start.get() <= call.start_byte as u64
                        && call.end_byte as u64 <= r.range.end.get()
                })
                .collect();
            enclosing.sort_by_key(|r| (r.range.start.get(), std::cmp::Reverse(r.range.end.get())));
            file.calls.push(Call {
                id: OccurrenceId::new(call.stable_id.clone().unwrap()).unwrap(),
                owner_syntax_id: owner.clone(),
                ordinal: UInt::new(ordinal as u64).unwrap(),
                document: doc.key.clone(),
                revision_id: text(&capture.revision_id),
                range: span(call.start_byte, call.end_byte),
                callee_range: call
                    .verified_member_token
                    .then(|| span(call.token_start_byte, call.token_end_byte)),
                spelling: call.spelling.as_deref().map(text),
                region_ids: enclosing.iter().map(|r| r.id.clone()).collect(),
                provenance_id: file.provenance.id.clone(),
            });
        }
    }
    file
}

fn complete_native_evidence(capture: &CapturedRevision, evidence: &mut Evidence) {
    evidence.native_files = (0..capture.documents.len())
        .map(|index| complete_native_file(capture, evidence, index))
        .collect();
}

fn span(start: usize, end: usize) -> Range {
    Range {
        start: UInt::new(start as u64).unwrap(),
        end: UInt::new(end as u64).unwrap(),
    }
}

#[test]
fn native_provenance_is_bound_to_each_captured_document() {
    let (_dir, capture, mut evidence) = fixture();
    complete_native_evidence(&capture, &mut evidence);
    assert_eq!(validate_native(&capture, &evidence), Ok(()));
    assert!(matches!(
        validate_evidence(&capture, &evidence, &capture),
        Err(EvidenceError::NotYetValidated(_))
    ));
    let mut bad = evidence.clone();
    bad.native_files[0].provenance.content_hash = hash(b"not the document");
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Native(_))
    ));
    assert!(matches!(
        validate_native(&capture, &bad),
        Err(EvidenceError::Native(_))
    ));
    let mut bad = evidence.clone();
    bad.native_files[0].provenance.freshness = Freshness::PossiblyStale;
    assert!(matches!(
        validate_native(&capture, &bad),
        Err(EvidenceError::Native(_))
    ));
    let mut bad = evidence.clone();
    bad.native_files.push(bad.native_files[0].clone());
    assert!(matches!(
        validate_native(&capture, &bad),
        Err(EvidenceError::Native(_))
    ));
    let mut missing = evidence.clone();
    missing.native_files.pop();
    assert!(matches!(
        validate_native(&capture, &missing),
        Err(EvidenceError::Native(_))
    ));
    let mut forged_spelling = capture.clone();
    let java = forged_spelling
        .documents
        .iter_mut()
        .find(|d| d.key.language == Language::Java)
        .unwrap();
    let invocation = java
        .native_candidates
        .iter_mut()
        .find(|w| w.node_kind == "method_invocation" && w.stable_id.is_some())
        .unwrap();
    invocation.spelling = Some("invented".into());
    assert_eq!(
        validate_native(&forged_spelling, &evidence),
        Err(EvidenceError::Native(
            "member token or spelling differs from AST"
        ))
    );
    let mut forged_capture = capture.clone();
    forged_capture.documents[0].native_candidates[0].token_bytes = b"forged".to_vec();
    assert!(matches!(
        validate_native(&forged_capture, &evidence),
        Err(EvidenceError::Native(_))
    ));
}

#[test]
fn rust_same_span_control_uses_supported_ast_identity() {
    let (_dir, capture, mut evidence) =
        fixture_with_sources(None, None, Some("fn b() { if true { obj.foo(); } }\n"));
    let index = capture
        .documents
        .iter()
        .position(|d| d.key.language == Language::Rust)
        .unwrap();
    let doc = &capture.documents[index];
    let regions: Vec<_> = doc
        .native_candidates
        .iter()
        .filter(|w| w.candidate_kind == baleyg::indexer::NativeCandidateKind::ControlRegion)
        .collect();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].node_kind, "if_expression");
    assert_eq!((regions[0].start_byte, regions[0].end_byte), (9, 31));
    complete_native_evidence(&capture, &mut evidence);
    assert_eq!(validate_native(&capture, &evidence), Ok(()));
    let mut forged = capture.clone();
    forged.documents[index]
        .native_candidates
        .iter_mut()
        .find(|w| w.candidate_kind == baleyg::indexer::NativeCandidateKind::ControlRegion)
        .unwrap()
        .node_kind = "expression_statement".into();
    assert!(matches!(
        validate_native(&forged, &evidence),
        Err(EvidenceError::Native(_))
    ));
    let mut missing = evidence.clone();
    missing.native_files[index].control_regions.clear();
    assert_eq!(
        validate_native(&capture, &missing),
        Err(EvidenceError::Native(
            "selected complete document omits a captured supported candidate"
        ))
    );
}

#[test]
fn rust_complete_call_is_source_checked() {
    let (_dir, capture, mut evidence) =
        fixture_with_sources(None, None, Some("fn b() { let _ = obj.foo(); }\n"));
    complete_native_evidence(&capture, &mut evidence);
    assert_eq!(validate_native(&capture, &evidence), Ok(()));
    let index = capture
        .documents
        .iter()
        .position(|d| d.key.language == Language::Rust)
        .unwrap();
    assert_eq!(evidence.native_files[index].calls.len(), 1);
    let mut wrong = evidence.clone();
    wrong.native_files[index].calls[0].spelling = Some(text("invented"));
    assert!(matches!(
        validate_native(&capture, &wrong),
        Err(EvidenceError::Native(_))
    ));
    let mut forged = capture.clone();
    let original = &evidence.native_files[index].calls[0];
    forged.documents[index]
        .native_candidates
        .iter_mut()
        .find(|w| w.stable_id.as_deref() == Some(original.id.as_str()))
        .unwrap()
        .spelling = Some("invented".into());
    assert_eq!(
        validate_native(&forged, &evidence),
        Err(EvidenceError::Native(
            "member token or spelling differs from AST"
        ))
    );
    let mut missing = evidence.clone();
    missing.native_files[index].calls.clear();
    assert_eq!(
        validate_native(&capture, &missing),
        Err(EvidenceError::Native(
            "selected complete document omits a captured supported candidate"
        ))
    );
}

#[test]
fn javascript_and_python_complete_calls_use_original_member_bytes() {
    let (_dir, capture, mut evidence) = fixture_with_sources(
        Some("function f() { obj.foo(); }\n"),
        Some("def f():\n    obj.foo()\n"),
        None,
    );
    complete_native_evidence(&capture, &mut evidence);
    assert_eq!(validate_native(&capture, &evidence), Ok(()));
    assert!(matches!(
        validate_evidence(&capture, &evidence, &capture),
        Err(EvidenceError::NotYetValidated(_))
    ));
    for language in [Language::Javascript, Language::Python] {
        let index = capture
            .documents
            .iter()
            .position(|d| d.key.language == language)
            .unwrap();
        let call = &evidence.native_files[index].calls[0];
        assert_eq!(call.spelling.as_ref().map(Text::as_str), Some("foo"));
        assert_eq!(
            call.callee_range
                .as_ref()
                .map(|r| &capture.documents[index].bytes
                    [r.start.get() as usize..r.end.get() as usize]),
            Some(b"foo".as_slice())
        );
        let mut forged = capture.clone();
        forged.documents[index]
            .native_candidates
            .iter_mut()
            .find(|w| w.stable_id.as_deref() == Some(call.id.as_str()))
            .unwrap()
            .spelling = Some("forged".into());
        assert_eq!(
            validate_native(&forged, &evidence),
            Err(EvidenceError::Native(
                "member token or spelling differs from AST"
            ))
        );
        let mut omitted = evidence.clone();
        omitted.native_files[index].calls.clear();
        assert_eq!(
            validate_native(&capture, &omitted),
            Err(EvidenceError::Native(
                "selected complete document omits a captured supported candidate"
            ))
        );
        let mut modified = evidence.clone();
        modified.native_files[index].declarations[0].header.name = Some(text("wrong"));
        assert!(matches!(
            validate_native(&capture, &modified),
            Err(EvidenceError::Native(_))
        ));
    }
}

#[test]
fn complete_native_files_reject_missing_supported_candidates() {
    let (_dir, capture, mut evidence) = fixture();
    complete_native_evidence(&capture, &mut evidence);
    assert_eq!(validate_native(&capture, &evidence), Ok(()));
    let java = capture
        .documents
        .iter()
        .position(|d| d.key.language == Language::Java)
        .unwrap();
    let rust = capture
        .documents
        .iter()
        .position(|d| d.key.language == Language::Rust)
        .unwrap();
    for kind in 0..4 {
        let mut omitted = evidence.clone();
        match kind {
            0 => {
                omitted.native_files[java].declarations.pop();
            }
            1 => {
                omitted.native_files[java].calls.pop();
            }
            2 => {
                omitted.native_files[java].control_regions.pop();
            }
            _ => {
                omitted.native_files[rust].declarations.pop();
            }
        }
        assert_eq!(
            validate_native(&capture, &omitted),
            Err(EvidenceError::Native(
                "selected complete document omits a captured supported candidate"
            ))
        );
    }
    let mut absent = evidence.clone();
    absent.native_files.clear();
    assert!(matches!(
        validate_native(&capture, &absent),
        Err(EvidenceError::Native(_))
    ));
}

#[test]
fn native_declaration_uses_real_ast_name_and_identity() {
    let (_dir, capture, mut evidence) = fixture();
    complete_native_evidence(&capture, &mut evidence);
    let index = capture
        .documents
        .iter()
        .position(|d| d.key.language == Language::Java)
        .unwrap();
    assert_eq!(validate_native(&capture, &evidence), Ok(()));
    let mut wrong = evidence.clone();
    wrong.native_files[0].declarations[0].key.ordinal = UInt::new(1).unwrap();
    assert!(matches!(
        validate_native(&capture, &wrong),
        Err(EvidenceError::Native(_))
    ));
    let mut wrong = evidence.clone();
    wrong.native_files[0].declarations[0].name_range = Some(span(1, 2));
    assert!(matches!(
        validate_native(&capture, &wrong),
        Err(EvidenceError::Native(_))
    ));
    let mut wrong_capture = capture.clone();
    let java = &mut wrong_capture.documents[index];
    java.heritage[0].base_bytes = b"Forged".to_vec();
    assert!(matches!(
        validate_native(&wrong_capture, &evidence),
        Err(EvidenceError::Native(_))
    ));
    let mut wrong_capture = capture.clone();
    wrong_capture.documents[index].heritage.clear();
    assert!(matches!(
        validate_native(&wrong_capture, &evidence),
        Err(EvidenceError::Native(_))
    ));
    let mut wrong_capture = capture.clone();
    let class = wrong_capture.documents[index]
        .native_candidates
        .iter_mut()
        .find(|w| w.node_kind == "class_declaration")
        .unwrap();
    class.header_bytes = b"Forged".to_vec();
    assert!(matches!(
        validate_native(&wrong_capture, &evidence),
        Err(EvidenceError::Native(_))
    ));
    let mut wrong = evidence.clone();
    wrong.native_files[0].declarations[0].header.bases = vec![text("Forged")];
    assert!(matches!(
        validate_native(&capture, &wrong),
        Err(EvidenceError::Native(_))
    ));
    let mut wrong = evidence.clone();
    wrong.native_files[0].declarations[0].lookup_key = Some(text("Base"));
    assert!(matches!(
        validate_native(&capture, &wrong),
        Err(EvidenceError::Native(_))
    ));
    let mut wrong = evidence.clone();
    let duplicate = wrong.native_files[0].declarations[0].clone();
    wrong.native_files[0].declarations.push(duplicate);
    assert!(matches!(
        validate_native(&capture, &wrong),
        Err(EvidenceError::Native(_))
    ));
}

#[test]
fn native_call_ordinal_owner_and_member_span_are_source_checked() {
    let (_dir, capture, mut evidence) = fixture();
    complete_native_evidence(&capture, &mut evidence);
    let index = capture
        .documents
        .iter()
        .position(|d| d.key.language == Language::Java)
        .unwrap();
    let doc = &capture.documents[index];
    let class = doc
        .native_candidates
        .iter()
        .find(|w| w.node_kind == "class_declaration" && w.stable_id.is_some())
        .unwrap();
    assert_eq!(validate_native(&capture, &evidence), Ok(()));
    let mut with_sibling = evidence.clone();
    with_sibling.native_files[0].declarations[2].key.ordinal = UInt::new(0).unwrap();
    assert!(matches!(
        validate_native(&capture, &with_sibling),
        Err(EvidenceError::Native(_))
    ));
    let mut wrong = evidence.clone();
    wrong.native_files[0].control_regions[0].arm = Some(text("fabricated"));
    assert!(matches!(
        validate_native(&capture, &wrong),
        Err(EvidenceError::Native(_))
    ));
    let mut wrong = evidence.clone();
    let own_id = wrong.native_files[0].control_regions[0].id.clone();
    wrong.native_files[0].control_regions[0].parent_id = Some(own_id);
    assert!(matches!(
        validate_native(&capture, &wrong),
        Err(EvidenceError::Native(_))
    ));
    for mutation in 0..4 {
        let mut wrong = evidence.clone();
        let header = &mut wrong.native_files[0].declarations[1].header;
        match mutation {
            0 => header.modifiers.clear(),
            1 => header.type_parameters.clear(),
            2 => header.parameters[0].r#type = Some(text("long")),
            _ => header.result_type = None,
        }
        assert!(matches!(
            validate_native(&capture, &wrong),
            Err(EvidenceError::Native(_))
        ));
    }
    let mut wrong_capture = capture.clone();
    let invocation = wrong_capture.documents[index]
        .native_candidates
        .iter_mut()
        .find(|w| w.node_kind == "method_invocation" && w.stable_id.is_some())
        .unwrap();
    invocation.ancestor_ids.pop();
    assert!(matches!(
        validate_native(&wrong_capture, &evidence),
        Err(EvidenceError::Native(_))
    ));
    let mut wrong = evidence.clone();
    wrong.native_files[0].calls[0].region_ids.clear();
    assert!(matches!(
        validate_native(&capture, &wrong),
        Err(EvidenceError::Native(_))
    ));
    let mut wrong = evidence.clone();
    wrong.native_files[0].calls[0].ordinal = UInt::new(1).unwrap();
    assert!(matches!(
        validate_native(&capture, &wrong),
        Err(EvidenceError::Native(_))
    ));
    let mut wrong_capture = capture.clone();
    let invocation = wrong_capture.documents[index]
        .native_candidates
        .iter_mut()
        .find(|w| w.node_kind == "method_invocation" && w.stable_id.is_some())
        .unwrap();
    invocation.spelling = Some("invented".into());
    assert!(matches!(
        validate_native(&wrong_capture, &evidence),
        Err(EvidenceError::Native(_))
    ));
    let mut wrong = evidence.clone();
    wrong.native_files[0].calls[0].callee_range = Some(span(0, 1));
    assert!(matches!(
        validate_native(&capture, &wrong),
        Err(EvidenceError::Native(_))
    ));
    let mut wrong = evidence.clone();
    wrong.native_files[0].declarations[1].ancestors.clear();
    assert!(matches!(
        validate_native(&capture, &wrong),
        Err(EvidenceError::Native(_))
    ));
    let mut wrong = evidence.clone();
    wrong.native_files[0].declarations[1]
        .key
        .signature
        .as_mut()
        .unwrap()
        .parameter_types
        .clear();
    assert!(matches!(
        validate_native(&capture, &wrong),
        Err(EvidenceError::Native(_))
    ));
    let mut wrong = evidence.clone();
    wrong.native_files[0].calls[0].owner_syntax_id =
        SyntaxId::new(class.stable_id.clone().unwrap()).unwrap();
    assert!(matches!(
        validate_native(&capture, &wrong),
        Err(EvidenceError::Native(_))
    ));
}
