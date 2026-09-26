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
    let mut semantic_doc = scip::types::Document::new();
    semantic_doc.relative_path = "java/src/A.java".into();
    let mut child = scip::types::Occurrence::new();
    child.range = vec![0, 13, 18];
    child.symbol = "scip java fixture Child#".into();
    child.symbol_roles = 1;
    semantic_doc.occurrences.push(child);
    let mut base = scip::types::Occurrence::new();
    base.range = vec![0, 27, 31];
    base.symbol = "scip java fixture Base#".into();
    semantic_doc.occurrences.push(base);
    let mut info = scip::types::SymbolInformation::new();
    info.symbol = "scip java fixture Child#".into();
    let mut relation = scip::types::Relationship::new();
    relation.symbol = "scip java fixture Base#".into();
    relation.is_implementation = true;
    info.relationships.push(relation);
    semantic_doc.symbols.push(info);
    let mut method_occurrence = scip::types::Occurrence::new();
    method_occurrence.range = vec![0, 50, 53];
    method_occurrence.symbol = "scip java fixture Child#foo().".into();
    method_occurrence.symbol_roles = 1;
    semantic_doc.occurrences.push(method_occurrence);
    let mut second_method = scip::types::Occurrence::new();
    second_method.range = vec![0, 101, 104];
    second_method.symbol = "scip java fixture Child#foo().".into();
    second_method.symbol_roles = 1;
    semantic_doc.occurrences.push(second_method);
    let mut method_info = scip::types::SymbolInformation::new();
    method_info.symbol = "scip java fixture Child#foo().".into();
    let mut method_relation = scip::types::Relationship::new();
    method_relation.symbol = "scip java fixture Base#foo().".into();
    method_relation.is_implementation = true;
    method_info.relationships.push(method_relation);
    semantic_doc.symbols.push(method_info);
    let mut external_method = scip::types::SymbolInformation::new();
    external_method.symbol = "scip java fixture Base#foo().".into();
    semantic_doc.symbols.push(external_method);
    index.documents.push(semantic_doc);
    if python.is_some_and(|source| source.starts_with("class Child(Base):")) {
        let mut python_doc = scip::types::Document::new();
        python_doc.relative_path = "python/src/D.py".into();
        for (range, symbol, roles) in [
            (vec![0, 6, 11], "scip python fixture Child#", 1),
            (vec![0, 12, 16], "scip python fixture Base#", 0),
        ] {
            let mut occurrence = scip::types::Occurrence::new();
            occurrence.range = range;
            occurrence.symbol = symbol.into();
            occurrence.symbol_roles = roles;
            python_doc.occurrences.push(occurrence);
        }
        let mut info = scip::types::SymbolInformation::new();
        info.symbol = "scip python fixture Child#".into();
        let mut relation = scip::types::Relationship::new();
        relation.symbol = "scip python fixture Base#".into();
        relation.is_implementation = true;
        info.relationships.push(relation);
        python_doc.symbols.push(info);
        index.documents.push(python_doc);
    }
    if python.is_some_and(|source| source.starts_with("from m import x as y")) {
        let mut python_doc = scip::types::Document::new();
        python_doc.relative_path = "python/src/D.py".into();
        let mut occurrence = scip::types::Occurrence::new();
        occurrence.range = vec![0, 19, 20];
        occurrence.symbol = "scip python fixture y.".into();
        occurrence.symbol_roles = 1;
        python_doc.occurrences.push(occurrence);
        index.documents.push(python_doc);
    }
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
        let node = &doc.syntax[witness.node_id];
        let field = |id: usize, label: &str| {
            doc.syntax
                .iter()
                .find(|n| n.parent_id == Some(id) && n.field_name.as_deref() == Some(label))
        };
        let source_text = |n: &baleyg::indexer::CapturedSyntaxNode| {
            text(std::str::from_utf8(&n.source_bytes).unwrap())
        };
        let parameter_nodes =
            field(node.id, "parameters").or_else(|| field(node.id, "formal_parameters"));
        let parameters = parameter_nodes
            .into_iter()
            .flat_map(|n| {
                doc.syntax.iter().filter(move |part| {
                    part.parent_id == Some(n.id) && part.candidate_kind.is_some()
                })
            })
            .map(|part| Parameter {
                name: field(part.id, "name").map(&source_text),
                r#type: field(part.id, "type").map(&source_text),
                variadic: matches!(part.kind.as_str(), "rest_pattern" | "list_splat_pattern"),
            })
            .collect::<Vec<_>>();
        let result_type = field(node.id, "return_type").map(&source_text);
        let modifiers = doc
            .syntax
            .iter()
            .filter(|n| n.parent_id == Some(node.id) && n.kind == "visibility_modifier")
            .map(&source_text)
            .collect::<Vec<_>>();
        let type_parameters = field(node.id, "type_parameters")
            .into_iter()
            .flat_map(|n| {
                doc.syntax.iter().filter(move |part| {
                    part.parent_id == Some(n.id) && part.kind == "type_parameter"
                })
            })
            .map(&source_text)
            .collect::<Vec<_>>();
        let declared_name = field(node.id, "name").map(&source_text);
        let mut enclosing = node.parent_id;
        let mut parent_keys = vec![];
        let mut parent_node_id = None;
        while let Some(id) = enclosing {
            if let Some(parent) = doc.native_candidates.iter().find(|candidate| {
                candidate.node_id == id
                    && candidate.candidate_kind == baleyg::indexer::NativeCandidateKind::Declaration
                    && candidate.stable_id.is_some()
            }) && let Some(row) = file
                .declarations
                .iter()
                .find(|row| parent.stable_id.as_deref() == Some(row.syntax_id.as_str()))
            {
                parent_keys.push(row.key.clone());
                parent_node_id = Some(id);
            }
            enclosing = doc.syntax[id].parent_id;
        }
        parent_keys.reverse();
        let mut function_ancestors =
            if matches!(doc.key.language, Language::Python | Language::Rust) {
                vec![]
            } else {
                vec![module.clone()]
            };
        function_ancestors.extend(parent_keys.iter().cloned());
        let ordinal = doc
            .native_candidates
            .iter()
            .filter(|candidate| {
                candidate.candidate_kind == baleyg::indexer::NativeCandidateKind::Declaration
                    && candidate.node_kind == witness.node_kind
                    && candidate.name_bytes == witness.name_bytes
                    && candidate.start_byte < witness.start_byte
            })
            .filter(|candidate| {
                let mut parent = doc.syntax[candidate.node_id].parent_id;
                while let Some(id) = parent {
                    if doc.native_candidates.iter().any(|p| {
                        p.node_id == id
                            && p.candidate_kind == baleyg::indexer::NativeCandidateKind::Declaration
                            && p.stable_id.is_some()
                    }) {
                        return Some(id) == parent_node_id;
                    }
                    parent = doc.syntax[id].parent_id;
                }
                parent_node_id.is_none()
            })
            .count();
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
                (Language::Javascript, "class_declaration" | "class")
                | (Language::Python, "class_definition") => {
                    let name = text("Child");
                    let key = Key {
                        kind: Kind::Type,
                        name: Some(name.clone()),
                        signature: None,
                        ordinal: UInt::new(0).unwrap(),
                    };
                    (
                        Kind::Type,
                        name.clone(),
                        key,
                        if doc.key.language == Language::Javascript {
                            vec![module.clone()]
                        } else {
                            vec![]
                        },
                        Header {
                            kind: Kind::Type,
                            name: Some(name),
                            modifiers: vec![],
                            type_parameters: vec![],
                            parameters: vec![],
                            result_type: None,
                            bases: vec![text("Base")],
                        },
                    )
                }
                (Language::Javascript, "method_definition")
                | (Language::Python, "function_definition")
                    if parent_keys.last().is_some_and(|p| p.kind == Kind::Type) =>
                {
                    let name = declared_name.clone().unwrap();
                    (
                        Kind::Method,
                        name.clone(),
                        Key {
                            kind: Kind::Method,
                            name: Some(name.clone()),
                            signature: None,
                            ordinal: UInt::new(ordinal as u64).unwrap(),
                        },
                        function_ancestors.clone(),
                        Header {
                            kind: Kind::Method,
                            name: Some(name),
                            modifiers: modifiers.clone(),
                            type_parameters: type_parameters.clone(),
                            parameters: parameters.clone(),
                            result_type: result_type.clone(),
                            bases: vec![],
                        },
                    )
                }
                (Language::Rust, "impl_item") => {
                    let ty = field(node.id, "type").map(&source_text).unwrap();
                    let name = text(&format!("impl {}", ty.as_str()));
                    (
                        Kind::Type,
                        name.clone(),
                        Key {
                            kind: Kind::Type,
                            name: Some(name.clone()),
                            signature: None,
                            ordinal: UInt::new(ordinal as u64).unwrap(),
                        },
                        function_ancestors.clone(),
                        Header {
                            kind: Kind::Type,
                            name: Some(name),
                            modifiers: modifiers.clone(),
                            type_parameters: type_parameters.clone(),
                            parameters: vec![],
                            result_type: None,
                            bases: vec![],
                        },
                    )
                }
                (Language::Rust, "function_item")
                    if parent_keys.last().is_some_and(|p| p.kind == Kind::Type) =>
                {
                    let name = declared_name.clone().unwrap();
                    (
                        Kind::Method,
                        name.clone(),
                        Key {
                            kind: Kind::Method,
                            name: Some(name.clone()),
                            signature: None,
                            ordinal: UInt::new(ordinal as u64).unwrap(),
                        },
                        function_ancestors.clone(),
                        Header {
                            kind: Kind::Method,
                            name: Some(name),
                            modifiers: modifiers.clone(),
                            type_parameters: type_parameters.clone(),
                            parameters: parameters.clone(),
                            result_type: result_type.clone(),
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
                            ordinal: UInt::new(ordinal as u64).unwrap(),
                        },
                        function_ancestors.clone(),
                        Header {
                            kind: Kind::Function,
                            name: Some(name),
                            modifiers: vec![],
                            type_parameters: vec![],
                            parameters: parameters.clone(),
                            result_type: result_type.clone(),
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
                        ordinal: UInt::new(ordinal as u64).unwrap(),
                    },
                    function_ancestors.clone(),
                    Header {
                        kind: Kind::Function,
                        name: Some(text("b")),
                        modifiers: vec![],
                        type_parameters: vec![],
                        parameters: parameters.clone(),
                        result_type: result_type.clone(),
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
        let owner = |w: &baleyg::indexer::CapturedNativeWitness| {
            SyntaxId::new(w.ancestor_ids.last().unwrap().clone()).unwrap()
        };
        let mut regions: Vec<_> = doc
            .native_candidates
            .iter()
            .filter(|w| {
                w.stable_id.is_some()
                    && w.candidate_kind == baleyg::indexer::NativeCandidateKind::ControlRegion
            })
            .collect();
        regions.sort_by_key(|w| (w.start_byte, w.end_byte));
        for region in &regions {
            let ordinal = regions
                .iter()
                .filter(|previous| {
                    previous.ancestor_ids.last() == region.ancestor_ids.last()
                        && (previous.start_byte, previous.end_byte)
                            < (region.start_byte, region.end_byte)
                })
                .count();
            let parent = regions
                .iter()
                .filter(|p| {
                    p.node_id != region.node_id
                        && p.ancestor_ids.last() == region.ancestor_ids.last()
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
                owner_syntax_id: owner(region),
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
        for call in &calls {
            let ordinal = calls
                .iter()
                .filter(|previous| {
                    previous.ancestor_ids.last() == call.ancestor_ids.last()
                        && (previous.start_byte, previous.end_byte)
                            < (call.start_byte, call.end_byte)
                })
                .count();
            let mut enclosing: Vec<_> = file
                .control_regions
                .iter()
                .filter(|r| {
                    r.owner_syntax_id == owner(call)
                        && r.range.start.get() <= call.start_byte as u64
                        && call.end_byte as u64 <= r.range.end.get()
                })
                .collect();
            enclosing.sort_by_key(|r| (r.range.start.get(), std::cmp::Reverse(r.range.end.get())));
            file.calls.push(Call {
                id: OccurrenceId::new(call.stable_id.clone().unwrap()).unwrap(),
                owner_syntax_id: owner(call),
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
fn non_java_class_bases_are_complete_against_source() {
    let (_dir, capture, mut evidence) = fixture_with_sources(
        Some("class Child extends Base {}\n"),
        Some("class Child(Base):\n    pass\n"),
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
        let mut missing_header = evidence.clone();
        missing_header.native_files[index].declarations[0]
            .header
            .bases
            .clear();
        assert!(matches!(
            validate_native(&capture, &missing_header),
            Err(EvidenceError::Native(_))
        ));
        if language == Language::Javascript {
            let mut missing_both = missing_header.clone();
            let mut deleted = capture.clone();
            deleted.documents[index].heritage.clear();
            missing_both.native_files[index].declarations[0]
                .header
                .bases
                .clear();
            assert!(matches!(
                validate_native(&deleted, &missing_both),
                Err(EvidenceError::Native(_))
            ));
        } else {
            let mut retarget = evidence.clone();
            retarget.native_files[index].declarations[0].header.bases[0] = text("Other");
            assert!(matches!(
                validate_native(&capture, &retarget),
                Err(EvidenceError::Native(_))
            ));
        }
    }
}

#[test]
fn javascript_nested_control_parent_is_exact_ast_parent() {
    let (_dir, capture, mut evidence) = fixture_with_sources(
        Some("function f() { if (true) { if (false) { obj.foo(); } } }\n"),
        None,
        None,
    );
    complete_native_evidence(&capture, &mut evidence);
    assert_eq!(validate_native(&capture, &evidence), Ok(()));
    let index = capture
        .documents
        .iter()
        .position(|d| d.key.language == Language::Javascript)
        .unwrap();
    let regions = &evidence.native_files[index].control_regions;
    let mut ifs: Vec<_> = regions
        .iter()
        .filter(|r| r.kind.as_str() == "if_statement")
        .collect();
    ifs.sort_by_key(|r| r.range.start.get());
    assert_eq!(ifs.len(), 2);
    let (outer, nested) = (ifs[0], ifs[1]);
    let doc = &capture.documents[index];
    let measured_parent = doc
        .native_candidates
        .iter()
        .find(|w| {
            w.node_kind == "statement_block"
                && w.stable_id.as_deref() == nested.parent_id.as_ref().map(OccurrenceId::as_str)
        })
        .unwrap();
    let nested_witness = doc
        .native_candidates
        .iter()
        .find(|w| w.stable_id.as_deref() == Some(nested.id.as_str()))
        .unwrap();
    assert_eq!(
        doc.syntax[nested_witness.node_id].parent_id,
        Some(measured_parent.node_id)
    );
    assert_eq!(
        doc.syntax[measured_parent.node_id].field_name.as_deref(),
        Some("consequence")
    );
    assert_eq!(
        nested.parent_id.as_ref().map(OccurrenceId::as_str),
        measured_parent.stable_id.as_deref()
    );
    let arm_region = regions
        .iter()
        .find(|r| r.id.as_str() == measured_parent.stable_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(
        arm_region.arm.as_ref().map(Text::as_str),
        Some("consequence")
    );
    let mut wrong_arm = evidence.clone();
    wrong_arm.native_files[index]
        .control_regions
        .iter_mut()
        .find(|r| r.id == arm_region.id)
        .unwrap()
        .arm = Some(text("alternative"));
    assert!(matches!(
        validate_native(&capture, &wrong_arm),
        Err(EvidenceError::Native(_))
    ));
    assert!(outer.range.start <= nested.range.start && nested.range.end <= outer.range.end);
    assert_ne!(nested.parent_id.as_ref(), Some(&outer.id));
    let mut wrong = evidence.clone();
    wrong.native_files[index]
        .control_regions
        .iter_mut()
        .find(|r| r.id == nested.id)
        .unwrap()
        .parent_id = Some(outer.id.clone());
    assert!(matches!(
        validate_native(&capture, &wrong),
        Err(EvidenceError::Native(_))
    ));
}

#[test]
fn non_java_member_declarations_and_rust_generic_modifiers_are_source_checked() {
    let (_dir, capture, mut evidence) = fixture_with_sources(
        Some("class Child extends Base { foo(a) { return a; } }\n"),
        Some("class Child(Base):\n    def f(self, a: int) -> int:\n        return a\n"),
        Some("impl Child { pub fn b<T>(a: T) -> T { a } }\n"),
    );
    complete_native_evidence(&capture, &mut evidence);
    assert_eq!(validate_native(&capture, &evidence), Ok(()));
    assert!(matches!(
        validate_evidence(&capture, &evidence, &capture),
        Err(EvidenceError::NotYetValidated(_))
    ));
    for language in [Language::Javascript, Language::Python, Language::Rust] {
        let index = capture
            .documents
            .iter()
            .position(|d| d.key.language == language)
            .unwrap();
        let declarations = &evidence.native_files[index].declarations;
        let method = declarations
            .iter()
            .find(|d| d.kind == Kind::Method)
            .unwrap();
        assert_eq!(method.ancestors.last(), Some(&declarations[0].key));
        assert_eq!(method.key.kind, Kind::Method);
        assert_eq!(method.key.name, method.header.name);
        assert!(!method.header.parameters.is_empty());
        let method_id = method.syntax_id.clone();
        let mut wrong = evidence.clone();
        wrong.native_files[index]
            .declarations
            .iter_mut()
            .find(|d| d.syntax_id == method_id)
            .unwrap()
            .header
            .parameters
            .clear();
        assert!(matches!(
            validate_native(&capture, &wrong),
            Err(EvidenceError::Native(_))
        ));
        let mut wrong = evidence.clone();
        wrong.native_files[index]
            .declarations
            .iter_mut()
            .find(|d| d.syntax_id == method_id)
            .unwrap()
            .ancestors
            .clear();
        assert!(matches!(
            validate_native(&capture, &wrong),
            Err(EvidenceError::Native(_))
        ));
        if language == Language::Rust {
            assert_eq!(method.header.modifiers, vec![text("pub")]);
            assert_eq!(method.header.type_parameters, vec![text("T")]);
            let mut wrong = evidence.clone();
            wrong.native_files[index]
                .declarations
                .iter_mut()
                .find(|d| d.syntax_id == method_id)
                .unwrap()
                .header
                .modifiers
                .clear();
            assert!(matches!(
                validate_native(&capture, &wrong),
                Err(EvidenceError::Native(_))
            ));
            let mut wrong = evidence.clone();
            wrong.native_files[index]
                .declarations
                .iter_mut()
                .find(|d| d.syntax_id == method_id)
                .unwrap()
                .header
                .type_parameters
                .clear();
            assert!(matches!(
                validate_native(&capture, &wrong),
                Err(EvidenceError::Native(_))
            ));
        }
    }
}

#[test]
fn non_java_typed_and_parameter_headers_are_source_checked() {
    let (_dir, capture, mut evidence) = fixture_with_sources(
        Some("function f(a, b) { obj.foo(); }\n"),
        Some("def f(a: int) -> int:\n    return a\n"),
        Some("fn b(a: i32) -> i32 { a }\n"),
    );
    complete_native_evidence(&capture, &mut evidence);
    assert_eq!(validate_native(&capture, &evidence), Ok(()));
    assert!(matches!(
        validate_evidence(&capture, &evidence, &capture),
        Err(EvidenceError::NotYetValidated(_))
    ));
    for language in [Language::Javascript, Language::Python, Language::Rust] {
        let index = capture
            .documents
            .iter()
            .position(|d| d.key.language == language)
            .unwrap();
        let header = &evidence.native_files[index].declarations[0].header;
        assert!(!header.parameters.is_empty());
        if language != Language::Javascript {
            assert!(header.result_type.is_some());
        }
        let mut wrong = evidence.clone();
        wrong.native_files[index].declarations[0]
            .header
            .parameters
            .clear();
        assert!(matches!(
            validate_native(&capture, &wrong),
            Err(EvidenceError::Native(_))
        ));
        if language != Language::Javascript {
            let mut wrong = evidence.clone();
            wrong.native_files[index].declarations[0].header.result_type = None;
            assert!(matches!(
                validate_native(&capture, &wrong),
                Err(EvidenceError::Native(_))
            ));
        }
    }
}

#[test]
fn non_member_calls_keep_null_callee_tokens() {
    let (_dir, capture, mut evidence) = fixture_with_sources(
        Some("function f() { foo(); }\n"),
        Some("def f():\n    foo()\n"),
        Some("fn b() { foo(); }\n"),
    );
    complete_native_evidence(&capture, &mut evidence);
    assert_eq!(validate_native(&capture, &evidence), Ok(()));
    for language in [Language::Javascript, Language::Python, Language::Rust] {
        let index = capture
            .documents
            .iter()
            .position(|d| d.key.language == language)
            .unwrap();
        let call = &evidence.native_files[index].calls[0];
        assert!(call.callee_range.is_none());
        let mut forged = capture.clone();
        let witness = forged.documents[index]
            .native_candidates
            .iter_mut()
            .find(|w| w.stable_id.as_deref() == Some(call.id.as_str()))
            .unwrap();
        witness.verified_member_token = true;
        assert!(matches!(
            validate_native(&forged, &evidence),
            Err(EvidenceError::Native(_))
        ));
        let mut invented = evidence.clone();
        invented.native_files[index].calls[0].callee_range = Some(span(0, 1));
        assert!(matches!(
            validate_native(&capture, &invented),
            Err(EvidenceError::Native(_))
        ));
    }
}

#[test]
fn non_java_sibling_ordinals_and_nested_ancestors_are_source_checked() {
    let (_dir, capture, mut evidence) = fixture_with_sources(
        Some("function f() {} function f() {}\n"),
        Some("def f():\n    pass\ndef f():\n    pass\n"),
        Some("fn b() {} fn b() {}\n"),
    );
    complete_native_evidence(&capture, &mut evidence);
    assert_eq!(validate_native(&capture, &evidence), Ok(()));
    for language in [Language::Javascript, Language::Python, Language::Rust] {
        let index = capture
            .documents
            .iter()
            .position(|d| d.key.language == language)
            .unwrap();
        assert_eq!(evidence.native_files[index].declarations.len(), 2);
        assert_eq!(
            evidence.native_files[index].declarations[1]
                .key
                .ordinal
                .get(),
            1
        );
        let mut wrong = evidence.clone();
        wrong.native_files[index].declarations[1].key.ordinal = UInt::new(0).unwrap();
        assert!(matches!(
            validate_native(&capture, &wrong),
            Err(EvidenceError::Native(_))
        ));
    }
    let (_dir, capture, mut evidence) = fixture_with_sources(
        Some("function f() { function f() {} }\n"),
        Some("def f():\n    def f():\n        pass\n"),
        Some("fn b() { fn b() {} }\n"),
    );
    complete_native_evidence(&capture, &mut evidence);
    assert_eq!(validate_native(&capture, &evidence), Ok(()));
    for language in [Language::Javascript, Language::Python, Language::Rust] {
        let index = capture
            .documents
            .iter()
            .position(|d| d.key.language == language)
            .unwrap();
        let nested = &evidence.native_files[index].declarations[1];
        assert_eq!(
            nested.ancestors.last(),
            Some(&evidence.native_files[index].declarations[0].key)
        );
        let mut wrong = evidence.clone();
        wrong.native_files[index].declarations[1].ancestors.pop();
        assert!(matches!(
            validate_native(&capture, &wrong),
            Err(EvidenceError::Native(_))
        ));
    }
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

fn semantic_provenance(
    capture: &CapturedRevision,
    evidence: &Evidence,
    document_index: usize,
) -> Provenance {
    let doc = &capture.documents[document_index];
    let producer = capture.producers.iter().find(|p| p.id == "S").unwrap();
    Provenance {
        id: text(&format!("semantic-{document_index}")),
        producer_id: text("S"),
        document: doc.key.clone(),
        revision_id: text(&capture.revision_id),
        content_hash: hash(&doc.bytes),
        evidence_kind: EvidenceKind::SemanticReference,
        basis: Some(SemanticBasis {
            producer_id: text("S"),
            producer_version: text(&producer.version),
            producer_hash: hash(&producer.executable_bytes),
            artifact_hash: hash(producer.artifact_bytes.as_ref().unwrap()),
            language: doc.key.language,
            source_set_id: evidence.context.source_set.id.clone(),
            revision_id: text(&capture.revision_id),
            source_manifest_hash: hash(&capture.manifest_bytes),
            toolchain_hash: hash(&capture.toolchain_bytes),
            config_hash: hash(&capture.config_bytes),
            dependency_hash: hash(&capture.dependency_bytes),
            lookup_dependencies: capture
                .lookup_dependencies
                .iter()
                .find(|(key, _)| key == &doc.key)
                .unwrap()
                .1
                .iter()
                .map(|key| text(key))
                .collect(),
        }),
        freshness: Freshness::Fresh,
    }
}

#[test]
fn semantic_basis_proves_independent_artifact_and_components() {
    let (_dir, capture, mut evidence) = fixture();
    complete_native_evidence(&capture, &mut evidence);
    let selected = evidence
        .coverage
        .iter_mut()
        .find(|c| c.producer_id.as_str() == "S" && c.document_path == capture.documents[0].key.path)
        .unwrap();
    selected.state = CoverageState::Partial;
    selected.selected = true;
    evidence
        .provenance
        .push(semantic_provenance(&capture, &evidence, 0));
    assert_eq!(
        baleyg::semantic_evidence::validate_semantic_basis(&capture, &evidence, &capture),
        Ok(())
    );
    assert!(matches!(
        validate_evidence(&capture, &evidence, &capture),
        Err(EvidenceError::NotYetValidated(_))
    ));
    let mut wrong = evidence.clone();
    wrong.provenance[0]
        .basis
        .as_mut()
        .unwrap()
        .lookup_dependencies
        .push(text("forged"));
    assert!(matches!(
        validate_evidence(&capture, &wrong, &capture),
        Err(EvidenceError::Basis(_))
    ));
    let mut wrong = evidence.clone();
    wrong.provenance[0].basis.as_mut().unwrap().artifact_hash = hash(b"forged artifact");
    assert!(matches!(
        validate_evidence(&capture, &wrong, &capture),
        Err(EvidenceError::Basis(_))
    ));
    let mut wrong = evidence.clone();
    wrong.provenance[0].basis.as_mut().unwrap().config_hash = hash(b"forged config");
    assert!(matches!(
        validate_evidence(&capture, &wrong, &capture),
        Err(EvidenceError::Basis(_))
    ));
    let mut wrong = evidence.clone();
    wrong.provenance[0].basis.as_mut().unwrap().producer_hash = hash(b"forged executable");
    assert!(matches!(
        validate_evidence(&capture, &wrong, &capture),
        Err(EvidenceError::Basis(_))
    ));
    for component in ["toolchain", "dependency", "manifest"] {
        let mut wrong = evidence.clone();
        let basis = wrong.provenance[0].basis.as_mut().unwrap();
        match component {
            "toolchain" => basis.toolchain_hash = hash(b"forged toolchain"),
            "dependency" => basis.dependency_hash = hash(b"forged dependency"),
            "manifest" => basis.source_manifest_hash = hash(b"forged manifest"),
            _ => unreachable!(),
        }
        assert!(
            matches!(
                validate_evidence(&capture, &wrong, &capture),
                Err(EvidenceError::Basis(_))
            ),
            "{component}"
        );
    }
    let mut wrong = evidence.clone();
    wrong.provenance[0].basis = None;
    assert!(matches!(
        validate_evidence(&capture, &wrong, &capture),
        Err(EvidenceError::Basis(_))
    ));
    let mut wrong = evidence.clone();
    wrong.provenance[0].id = evidence.native_files[0].provenance.id.clone();
    assert!(matches!(
        validate_evidence(&capture, &wrong, &capture),
        Err(EvidenceError::Basis(_))
    ));
}

#[test]
fn semantic_basis_freshness_uses_requested_captured_bytes() {
    let (_dir, capture, mut evidence) = fixture();
    complete_native_evidence(&capture, &mut evidence);
    let selected = evidence
        .coverage
        .iter_mut()
        .find(|c| c.producer_id.as_str() == "S" && c.document_path == capture.documents[0].key.path)
        .unwrap();
    selected.state = CoverageState::Partial;
    selected.selected = true;
    evidence
        .provenance
        .push(semantic_provenance(&capture, &evidence, 0));
    let mut requested = capture.clone();
    requested.config_bytes.push(1);
    assert!(matches!(
        validate_evidence(&capture, &evidence, &requested),
        Err(EvidenceError::Basis(_))
    ));
    evidence.provenance[0].freshness = Freshness::PossiblyStale;
    assert_eq!(
        baleyg::semantic_evidence::validate_semantic_basis(&capture, &evidence, &requested),
        Ok(())
    );
    requested.documents[0].bytes.push(b'!');
    evidence.provenance[0].freshness = Freshness::Stale;
    assert_eq!(
        baleyg::semantic_evidence::validate_semantic_basis(&capture, &evidence, &requested),
        Ok(())
    );
}

#[test]
fn failed_refresh_keeps_old_proof_historical_without_binding_old_occurrence() {
    let (dir, old, mut historical) = selected_semantic_fixture();
    let admission = CaptureAdmission {
        source_set_id: old.source_set_id.clone(),
        root_id: old.root_id.clone(),
        languages: historical.context.source_set.languages.clone(),
        toolchain: dir.path().join("toolchain.capture"),
        config: dir.path().join("config.capture"),
        dependency: dir.path().join("dependency.capture"),
        dependency_source_sets: vec![],
        producers: vec![baleyg::indexer::ProducerInput {
            id: "S".into(),
            tool_name: "scip-test".into(),
            version: "1".into(),
            position_encoding: "utf8".into(),
            executable: dir.path().join("toolchain.capture"),
            artifact: Some(dir.path().join("semantic.artifact")),
        }],
    };
    fs::write(dir.path().join("config.capture"), b"r2 config").unwrap();
    let mut options = IndexOptions::new(dir.path().to_owned());
    options.scip_path = Some(dir.path().join("semantic.artifact"));
    let newer = capture_revision(&options, &admission, &Arc::new(AtomicBool::new(false))).unwrap();
    assert_ne!(old.revision_id, newer.revision_id);
    let mut failed = historical.clone();
    failed.provenance.clear();
    failed.context.revision.id = text(&newer.revision_id);
    failed.context.revision.config_hash = hash(&newer.config_bytes);
    for document in &mut failed.context.revision.documents {
        document.revision_id = text(&newer.revision_id);
    }
    for coverage in &mut failed.coverage {
        coverage.revision_id = text(&newer.revision_id);
        if coverage.producer_id.as_str() == "S"
            && coverage.document_path == newer.documents[0].key.path
        {
            coverage.state = CoverageState::Failed;
            coverage.selected = true;
            coverage.diagnostic = Some(text("refresh failed"));
        }
    }
    failed.native_files.clear();
    complete_native_evidence(&newer, &mut failed);
    assert_eq!(validate_capture(&newer, &failed), Ok(()));
    assert_eq!(
        baleyg::semantic_evidence::validate_semantic_basis(&newer, &failed, &newer),
        Ok(())
    );
    historical.provenance[0].freshness = Freshness::PossiblyStale;
    assert_eq!(
        baleyg::semantic_evidence::validate_semantic_basis(&old, &historical, &newer),
        Ok(())
    );
    // Historical facts belong to r1; the failed r2 producer has no selected proof.
    let mut promoted = failed.clone();
    let mut old_reference = Reference {
        id: baleyg::semantic_identity::occurrence_id(
            &historical.provenance[0].revision_id,
            &historical.native_files[0].declarations[0].syntax_id,
            baleyg::semantic_identity::OccurrenceKind::Reference,
            UInt::new(0).unwrap(),
        )
        .unwrap(),
        owner_syntax_id: historical.native_files[0].declarations[0].syntax_id.clone(),
        ordinal: UInt::new(0).unwrap(),
        document: old.documents[0].key.clone(),
        revision_id: text(&old.revision_id),
        range: span(13, 18),
        spelling: text("Child"),
        lookup_key: text("Child"),
        site: ReferenceSite::Use,
        roles: vec![Role::Read],
        resolution: Resolution::Unresolved,
        declared_target: None,
        candidates: vec![],
        provenance_id: historical.provenance[0].id.clone(),
    };
    promoted.references.push(old_reference.clone());
    assert!(matches!(
        validate_evidence(&newer, &promoted, &newer),
        Err(EvidenceError::Basis(_))
    ));
    old_reference.revision_id = text(&newer.revision_id);
    promoted.references[0] = old_reference;
    assert!(matches!(
        validate_evidence(&newer, &promoted, &newer),
        Err(EvidenceError::Basis(_))
    ));
}

fn selected_semantic_fixture() -> (tempfile::TempDir, CapturedRevision, Evidence) {
    let (dir, capture, mut evidence) = fixture();
    complete_native_evidence(&capture, &mut evidence);
    let selected = evidence
        .coverage
        .iter_mut()
        .find(|c| c.producer_id.as_str() == "S" && c.document_path == capture.documents[0].key.path)
        .unwrap();
    selected.state = CoverageState::Partial;
    selected.selected = true;
    evidence
        .provenance
        .push(semantic_provenance(&capture, &evidence, 0));
    (dir, capture, evidence)
}
fn child_symbol(capture: &CapturedRevision, evidence: &Evidence) -> Symbol {
    let declaration = evidence.native_files[0]
        .declarations
        .iter()
        .find(|d| d.name.as_ref().is_some_and(|n| n.as_str() == "Child"))
        .unwrap();
    Symbol {
        key: SymbolKey {
            scheme: SymbolScheme::Scip,
            symbol: text("scip java fixture Child#"),
            scope: SymbolScope::Global,
            document: None,
        },
        display_name: Some(text("Child")),
        declarations: vec![Target::Internal {
            syntax_id: declaration.syntax_id.clone(),
            document: capture.documents[0].key.clone(),
            revision_id: text(&capture.revision_id),
        }],
        provenance_id: evidence.provenance[0].id.clone(),
    }
}
#[test]
fn symbol_fact_needs_source_definition_and_producer_artifact() {
    let (_dir, capture, mut evidence) = selected_semantic_fixture();
    evidence.symbols.push(child_symbol(&capture, &evidence));
    assert_eq!(
        baleyg::semantic_evidence::validate_semantic_facts(&capture, &evidence),
        Ok(())
    );
    assert!(matches!(
        validate_evidence(&capture, &evidence, &capture),
        Err(EvidenceError::NotYetValidated(_))
    ));
    let mut bad = evidence.clone();
    bad.symbols[0].key.symbol = text("fabricated");
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Basis(_))
    ));
    let mut bad = evidence.clone();
    bad.symbols[0].key.scope = SymbolScope::Document;
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Basis(_))
    ));
    let mut bad = evidence.clone();
    let duplicate = bad.symbols[0].declarations[0].clone();
    bad.symbols[0].declarations.push(duplicate);
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Basis(_))
    ));
}

#[test]
fn reference_fact_requires_exact_original_position_and_source_token() {
    let (_dir, capture, mut evidence) = selected_semantic_fixture();
    let symbol = child_symbol(&capture, &evidence);
    let Target::Internal {
        syntax_id,
        document,
        revision_id,
    } = symbol.declarations[0].clone()
    else {
        unreachable!()
    };
    evidence.symbols.push(symbol);
    evidence.references.push(Reference {
        id: baleyg::semantic_identity::occurrence_id(
            &revision_id,
            &syntax_id,
            baleyg::semantic_identity::OccurrenceKind::Reference,
            UInt::new(0).unwrap(),
        )
        .unwrap(),
        owner_syntax_id: syntax_id.clone(),
        ordinal: UInt::new(0).unwrap(),
        document: document.clone(),
        revision_id: revision_id.clone(),
        range: span(13, 18),
        spelling: text("Child"),
        lookup_key: text("Child"),
        site: ReferenceSite::Declaration,
        roles: vec![Role::Definition],
        resolution: Resolution::Resolved,
        declared_target: Some(Target::Internal {
            syntax_id,
            document,
            revision_id,
        }),
        candidates: vec![],
        provenance_id: evidence.provenance[0].id.clone(),
    });
    assert_eq!(
        baleyg::semantic_evidence::validate_semantic_facts(&capture, &evidence),
        Ok(())
    );
    let mut bad = evidence.clone();
    bad.references[0].resolution = Resolution::Ambiguous;
    bad.references[0].candidates = vec![
        bad.references[0].declared_target.take().unwrap(),
        evidence.references[0].declared_target.clone().unwrap(),
    ];
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Basis(_))
    ));
    let mut bad = evidence.clone();
    bad.references[0].resolution = Resolution::Ambiguous;
    bad.references[0].declared_target = None;
    bad.references[0].candidates = vec![
        Target::External {
            symbol: SymbolKey {
                scheme: SymbolScheme::Scip,
                symbol: text("invented"),
                scope: SymbolScope::Global,
                document: None,
            },
        },
        evidence.references[0].declared_target.clone().unwrap(),
    ];
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Basis(_))
    ));
    let mut bad = evidence.clone();
    bad.references[0].range = span(12, 18);
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Basis(_))
    ));
    let mut bad = evidence.clone();
    bad.references[0].lookup_key = text("child");
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Basis(_))
    ));
    let mut bad = evidence.clone();
    bad.references[0].roles = vec![Role::Read];
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Basis(_))
    ));
}

#[test]
fn python_alias_requires_definition_at_measured_alias_site() {
    let (_dir, capture, mut evidence) =
        fixture_with_sources(None, Some("from m import x as y\n"), None);
    complete_native_evidence(&capture, &mut evidence);
    let document_index = capture
        .documents
        .iter()
        .position(|d| d.key.language == Language::Python)
        .unwrap();
    let document = &capture.documents[document_index];
    let coverage = evidence
        .coverage
        .iter_mut()
        .find(|c| c.producer_id.as_str() == "S" && c.document_path == document.key.path)
        .unwrap();
    coverage.state = CoverageState::Partial;
    coverage.selected = true;
    coverage.supported_roles = vec![Role::Definition, Role::Alias];
    coverage.observed_roles = coverage.supported_roles.clone();
    let proof = semantic_provenance(&capture, &evidence, document_index);
    evidence.provenance.push(proof.clone());
    let module_id = baleyg::semantic_identity::syntax_id(
        &document.key.source_set_id,
        &document.key.path,
        document.key.language,
        &[],
        &Key {
            kind: Kind::Module,
            name: None,
            signature: None,
            ordinal: UInt::new(0).unwrap(),
        },
    )
    .unwrap();
    let reference = Reference {
        id: baleyg::semantic_identity::occurrence_id(
            &text(&capture.revision_id),
            &module_id,
            baleyg::semantic_identity::OccurrenceKind::Reference,
            UInt::new(0).unwrap(),
        )
        .unwrap(),
        owner_syntax_id: module_id,
        ordinal: UInt::new(0).unwrap(),
        document: document.key.clone(),
        revision_id: text(&capture.revision_id),
        range: span(19, 20),
        spelling: text("y"),
        lookup_key: text("y"),
        site: ReferenceSite::Declaration,
        roles: vec![Role::Definition, Role::Alias],
        resolution: Resolution::Unresolved,
        declared_target: None,
        candidates: vec![],
        provenance_id: proof.id,
    };
    evidence.references.push(reference);
    assert_eq!(
        baleyg::semantic_evidence::validate_semantic_facts(&capture, &evidence),
        Ok(())
    );
    let mut bad = evidence.clone();
    bad.references[0].site = ReferenceSite::Use;
    bad.references[0].roles = vec![Role::Alias];
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Basis(_))
    ));
}

#[test]
fn python_superclass_uses_measured_heritage_and_directed_semantic_relation() {
    let (_dir, capture, mut evidence) =
        fixture_with_sources(None, Some("class Child(Base):\n    pass\n"), None);
    complete_native_evidence(&capture, &mut evidence);
    let document_index = capture
        .documents
        .iter()
        .position(|d| d.key.language == Language::Python)
        .unwrap();
    let document = &capture.documents[document_index];
    let coverage = evidence
        .coverage
        .iter_mut()
        .find(|c| c.producer_id.as_str() == "S" && c.document_path == document.key.path)
        .unwrap();
    coverage.state = CoverageState::Partial;
    coverage.selected = true;
    let proof = semantic_provenance(&capture, &evidence, document_index);
    let source = evidence
        .native_files
        .iter()
        .find(|file| file.document.key == document.key)
        .unwrap()
        .declarations
        .iter()
        .find(|d| d.name.as_ref().is_some_and(|name| name.as_str() == "Child"))
        .unwrap();
    let target = Target::Internal {
        syntax_id: source.syntax_id.clone(),
        document: document.key.clone(),
        revision_id: text(&capture.revision_id),
    };
    evidence.symbols.push(Symbol {
        key: SymbolKey {
            scheme: SymbolScheme::Scip,
            symbol: text("scip python fixture Child#"),
            scope: SymbolScope::Global,
            document: None,
        },
        display_name: Some(text("Child")),
        declarations: vec![target.clone()],
        provenance_id: proof.id.clone(),
    });
    let mut relation_proof = proof.clone();
    relation_proof.id = text("python-relationship");
    relation_proof.evidence_kind = EvidenceKind::TypeRelationship;
    evidence.provenance.extend([proof, relation_proof.clone()]);
    evidence.type_relationships.push(TypeRelationship {
        kind: RelationshipKind::Extends,
        source: target,
        target: Target::External {
            symbol: SymbolKey {
                scheme: SymbolScheme::Scip,
                symbol: text("scip python fixture Base#"),
                scope: SymbolScope::Global,
                document: None,
            },
        },
        provenance_id: relation_proof.id,
    });
    assert_eq!(
        baleyg::semantic_evidence::validate_semantic_facts(&capture, &evidence),
        Ok(())
    );
    let mut reversed = evidence.clone();
    reversed.type_relationships[0].source = reversed.type_relationships[0].target.clone();
    assert!(matches!(
        validate_evidence(&capture, &reversed, &capture),
        Err(EvidenceError::Basis(_))
    ));
}

#[test]
fn ambiguous_reference_needs_sorted_distinct_producer_supported_targets() {
    let (_dir, capture, mut evidence) = selected_semantic_fixture();
    let methods: Vec<_> = evidence.native_files[0]
        .declarations
        .iter()
        .filter(|d| d.kind == Kind::Method && d.name.as_ref().is_some_and(|n| n.as_str() == "foo"))
        .collect();
    assert_eq!(methods.len(), 2);
    let mut targets: Vec<_> = methods
        .iter()
        .map(|d| Target::Internal {
            syntax_id: d.syntax_id.clone(),
            document: capture.documents[0].key.clone(),
            revision_id: text(&capture.revision_id),
        })
        .collect();
    targets.sort_by_key(|target| baleyg::semantic_identity::canonical_json(target).unwrap());
    evidence.symbols.push(Symbol {
        key: SymbolKey {
            scheme: SymbolScheme::Scip,
            symbol: text("scip java fixture Child#foo()."),
            scope: SymbolScope::Global,
            document: None,
        },
        display_name: Some(text("foo")),
        declarations: targets.clone(),
        provenance_id: evidence.provenance[0].id.clone(),
    });
    let owner = methods
        .iter()
        .find(|d| d.name_range.as_ref().is_some_and(|r| r.start.get() == 50))
        .unwrap();
    evidence.references.push(Reference {
        id: baleyg::semantic_identity::occurrence_id(
            &text(&capture.revision_id),
            &owner.syntax_id,
            baleyg::semantic_identity::OccurrenceKind::Reference,
            UInt::new(0).unwrap(),
        )
        .unwrap(),
        owner_syntax_id: owner.syntax_id.clone(),
        ordinal: UInt::new(0).unwrap(),
        document: capture.documents[0].key.clone(),
        revision_id: text(&capture.revision_id),
        range: span(50, 53),
        spelling: text("foo"),
        lookup_key: text("foo"),
        site: ReferenceSite::Declaration,
        roles: vec![Role::Definition],
        resolution: Resolution::Ambiguous,
        declared_target: None,
        candidates: targets,
        provenance_id: evidence.provenance[0].id.clone(),
    });
    assert_eq!(
        baleyg::semantic_evidence::validate_semantic_facts(&capture, &evidence),
        Ok(())
    );
    for mutation in 0..4 {
        let mut bad = evidence.clone();
        let candidates = &mut bad.references[0].candidates;
        match mutation {
            0 => candidates[1] = candidates[0].clone(),
            1 => candidates.reverse(),
            2 => {
                if let Target::Internal { syntax_id, .. } = &mut candidates[0] {
                    *syntax_id = SyntaxId::new("sid:v1:00000000000000000000000000000000").unwrap();
                }
            }
            _ => {
                if let Target::Internal { document, .. } = &mut candidates[0] {
                    document.source_set_id = text("foreign");
                }
            }
        }
        assert!(
            matches!(
                validate_evidence(&capture, &bad, &capture),
                Err(EvidenceError::Basis(_))
            ),
            "mutant {mutation}"
        );
    }
}

#[test]
fn java_unannotated_override_uses_measured_owner_heritage_and_directed_producer() {
    let (_dir, capture, mut evidence) = selected_semantic_fixture();
    let source = evidence.native_files[0]
        .declarations
        .iter()
        .find(|d| d.kind == Kind::Method && d.name.as_ref().is_some_and(|n| n.as_str() == "foo"))
        .unwrap();
    let target = Target::Internal {
        syntax_id: source.syntax_id.clone(),
        document: capture.documents[0].key.clone(),
        revision_id: text(&capture.revision_id),
    };
    evidence.symbols.push(Symbol {
        key: SymbolKey {
            scheme: SymbolScheme::Scip,
            symbol: text("scip java fixture Child#foo()."),
            scope: SymbolScope::Global,
            document: None,
        },
        display_name: Some(text("foo")),
        declarations: vec![target.clone()],
        provenance_id: evidence.provenance[0].id.clone(),
    });
    let mut proof = evidence.provenance[0].clone();
    proof.id = text("override-proof");
    proof.evidence_kind = EvidenceKind::TypeRelationship;
    evidence.provenance.push(proof.clone());
    evidence.type_relationships.push(TypeRelationship {
        kind: RelationshipKind::Overrides,
        source: target,
        target: Target::External {
            symbol: SymbolKey {
                scheme: SymbolScheme::Scip,
                symbol: text("scip java fixture Base#foo()."),
                scope: SymbolScope::Global,
                document: None,
            },
        },
        provenance_id: proof.id,
    });
    assert_eq!(
        baleyg::semantic_evidence::validate_semantic_facts(&capture, &evidence),
        Ok(())
    );
    let mut reversed = evidence.clone();
    reversed.type_relationships[0].source = reversed.type_relationships[0].target.clone();
    assert!(matches!(
        validate_evidence(&capture, &reversed, &capture),
        Err(EvidenceError::Basis(_))
    ));
}

#[test]
fn relationship_requires_heritage_syntax_and_directed_producer_fact() {
    let (_dir, capture, mut evidence) = selected_semantic_fixture();
    let symbol = child_symbol(&capture, &evidence);
    let source = symbol.declarations[0].clone();
    evidence.symbols.push(symbol);
    let mut relation_proof = evidence.provenance[0].clone();
    relation_proof.id = text("semantic-relationship");
    relation_proof.evidence_kind = EvidenceKind::TypeRelationship;
    evidence.provenance.push(relation_proof);
    evidence.type_relationships.push(TypeRelationship {
        kind: RelationshipKind::Extends,
        source,
        target: Target::External {
            symbol: SymbolKey {
                scheme: SymbolScheme::Scip,
                symbol: text("scip java fixture Base#"),
                scope: SymbolScope::Global,
                document: None,
            },
        },
        provenance_id: text("semantic-relationship"),
    });
    assert_eq!(
        baleyg::semantic_evidence::validate_semantic_facts(&capture, &evidence),
        Ok(())
    );
    let mut bad = evidence.clone();
    bad.type_relationships[0].kind = RelationshipKind::Implements;
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Basis(_))
    ));
    let mut bad = evidence.clone();
    bad.type_relationships[0].source = bad.type_relationships[0].target.clone();
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Basis(_))
    ));
    let mut bad = evidence.clone();
    if let Target::External { symbol } = &mut bad.type_relationships[0].target {
        symbol.symbol = text("scip java fixture Imaginary#");
    }
    assert!(matches!(
        validate_evidence(&capture, &bad, &capture),
        Err(EvidenceError::Basis(_))
    ));
}
