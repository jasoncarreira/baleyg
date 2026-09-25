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
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("java/src")).unwrap();
    fs::create_dir_all(root.join("rust/src")).unwrap();
    fs::write(
        root.join("java/src/A.java"),
        "class Child extends Base { void foo() { if (true) { é(); } } }
",
    )
    .unwrap();
    fs::write(
        root.join("rust/src/B.rs"),
        r#"fn b() { let _ = "😀"; }
"#,
    )
    .unwrap();
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
        languages: vec![Language::Java, Language::Rust],
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
        languages: vec![Language::Java, Language::Rust],
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
                languages: vec![Language::Java, Language::Rust],
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
fn span(start: usize, end: usize) -> Range {
    Range {
        start: UInt::new(start as u64).unwrap(),
        end: UInt::new(end as u64).unwrap(),
    }
}

#[test]
fn native_provenance_is_bound_to_each_captured_document() {
    let (_dir, capture, mut evidence) = fixture();
    evidence.native_files = (0..capture.documents.len())
        .map(|index| native_file(&capture, &evidence, index))
        .collect();
    assert_eq!(validate_native(&capture, &evidence), Ok(()));
    let mut bad = evidence.clone();
    bad.native_files[0].provenance.content_hash = hash(b"not the document");
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
    let mut forged_capture = capture.clone();
    forged_capture.documents[0].native_candidates[0].token_bytes = b"forged".to_vec();
    assert!(matches!(
        validate_native(&forged_capture, &evidence),
        Err(EvidenceError::Native(_))
    ));
}

#[test]
fn native_declaration_uses_real_ast_name_and_identity() {
    let (_dir, capture, mut evidence) = fixture();
    let index = capture
        .documents
        .iter()
        .position(|d| d.key.language == Language::Java)
        .unwrap();
    let doc = &capture.documents[index];
    let witness = doc
        .native_candidates
        .iter()
        .find(|w| w.node_kind == "class_declaration" && w.stable_id.is_some())
        .unwrap();
    let module = Key {
        kind: Kind::Module,
        name: None,
        signature: None,
        ordinal: UInt::new(0).unwrap(),
    };
    let name = text(std::str::from_utf8(&witness.name_bytes).unwrap());
    let key = Key {
        kind: Kind::Type,
        name: Some(name.clone()),
        signature: None,
        ordinal: UInt::new(0).unwrap(),
    };
    let row = Declaration {
        syntax_id: SyntaxId::new(witness.stable_id.clone().unwrap()).unwrap(),
        document: doc.key.clone(),
        revision_id: text(&capture.revision_id),
        kind: Kind::Type,
        name: Some(name.clone()),
        lookup_key: Some(name.clone()),
        ancestors: vec![module],
        key,
        range: span(witness.start_byte, witness.end_byte),
        name_range: Some(span(witness.token_start_byte, witness.token_end_byte)),
        header: Header {
            kind: Kind::Type,
            name: Some(name),
            modifiers: vec![],
            type_parameters: vec![],
            parameters: vec![],
            result_type: None,
            bases: vec![text("Base")],
        },
        provenance_id: text(&format!("native-{index}")),
    };
    evidence
        .native_files
        .push(native_file(&capture, &evidence, index));
    evidence.native_files[0].declarations.push(row);
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
    let method = doc
        .native_candidates
        .iter()
        .find(|w| w.node_kind == "method_declaration" && w.stable_id.is_some())
        .unwrap();
    let invocation = doc
        .native_candidates
        .iter()
        .find(|w| w.node_kind == "method_invocation" && w.stable_id.is_some())
        .unwrap();
    let module = Key {
        kind: Kind::Module,
        name: None,
        signature: None,
        ordinal: UInt::new(0).unwrap(),
    };
    let mut file = native_file(&capture, &evidence, index);
    for (w, ancestors, kind, signature) in [
        (class, vec![module.clone()], Kind::Type, None),
        (
            method,
            vec![
                module.clone(),
                Key {
                    kind: Kind::Type,
                    name: Some(text("Child")),
                    signature: None,
                    ordinal: UInt::new(0).unwrap(),
                },
            ],
            Kind::Method,
            Some(Signature {
                parameter_types: vec![],
                type_parameter_count: UInt::new(0).unwrap(),
                variadic: false,
            }),
        ),
    ] {
        let name = text(std::str::from_utf8(&w.name_bytes).unwrap());
        file.declarations.push(Declaration {
            syntax_id: SyntaxId::new(w.stable_id.clone().unwrap()).unwrap(),
            document: doc.key.clone(),
            revision_id: text(&capture.revision_id),
            kind,
            name: Some(name.clone()),
            lookup_key: Some(name.clone()),
            ancestors,
            key: Key {
                kind,
                name: Some(name.clone()),
                signature,
                ordinal: UInt::new(0).unwrap(),
            },
            range: span(w.start_byte, w.end_byte),
            name_range: Some(span(w.token_start_byte, w.token_end_byte)),
            header: Header {
                kind,
                name: Some(name),
                modifiers: vec![],
                type_parameters: vec![],
                parameters: vec![],
                result_type: None,
                bases: if kind == Kind::Type {
                    vec![text("Base")]
                } else {
                    vec![]
                },
            },
            provenance_id: file.provenance.id.clone(),
        });
    }
    let call = Call {
        id: OccurrenceId::new(invocation.stable_id.clone().unwrap()).unwrap(),
        owner_syntax_id: SyntaxId::new(method.stable_id.clone().unwrap()).unwrap(),
        ordinal: UInt::new(0).unwrap(),
        document: doc.key.clone(),
        revision_id: text(&capture.revision_id),
        range: span(invocation.start_byte, invocation.end_byte),
        callee_range: invocation
            .verified_member_token
            .then(|| span(invocation.token_start_byte, invocation.token_end_byte)),
        spelling: invocation.spelling.as_deref().map(text),
        region_ids: vec![],
        provenance_id: file.provenance.id.clone(),
    };
    file.calls.push(call);
    let region = doc
        .native_candidates
        .iter()
        .find(|w| w.node_kind == "if_statement" && w.stable_id.is_some())
        .unwrap();
    let control = ControlRegion {
        id: OccurrenceId::new(region.stable_id.clone().unwrap()).unwrap(),
        owner_syntax_id: SyntaxId::new(method.stable_id.clone().unwrap()).unwrap(),
        ordinal: UInt::new(0).unwrap(),
        document: doc.key.clone(),
        revision_id: text(&capture.revision_id),
        kind: text(&region.node_kind),
        range: span(region.start_byte, region.end_byte),
        parent_id: None,
        arm: None,
        provenance_id: file.provenance.id.clone(),
    };
    file.calls[0].region_ids.push(control.id.clone());
    file.control_regions.push(control);
    evidence.native_files.push(file);
    assert_eq!(validate_native(&capture, &evidence), Ok(()));
    let mut wrong = evidence.clone();
    let own_id = wrong.native_files[0].control_regions[0].id.clone();
    wrong.native_files[0].control_regions[0].parent_id = Some(own_id);
    assert!(matches!(
        validate_native(&capture, &wrong),
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
    let mut wrong = evidence.clone();
    wrong.native_files[0].calls[0].callee_range = Some(span(0, 1));
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
