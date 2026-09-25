//! Admission of immutable captured bytes before syntax and semantic validation.
use crate::{
    indexer::{
        CapturedDocument, CapturedNativeWitness, CapturedProducer, CapturedRevision,
        NativeCandidateKind,
    },
    model::v1::*,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceError {
    Capture(&'static str),
    Producer(&'static str),
    SourceSet(&'static str),
    Revision(&'static str),
    Document(&'static str),
    Coverage(&'static str),
    NotYetValidated(&'static str),
    Native(&'static str),
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn language_order(language: Language) -> u8 {
    match language {
        Language::Java => 0,
        Language::Rust => 1,
        Language::Python => 2,
        Language::Javascript => 3,
    }
}
fn encoding(encoding: PositionEncoding) -> &'static str {
    match encoding {
        PositionEncoding::Utf8 => "utf8",
        PositionEncoding::Utf16 => "utf16",
        PositionEncoding::UnicodeScalar => "unicodeScalar",
    }
}
fn nonempty_unique<T: Ord + Copy>(items: &[T]) -> bool {
    !items.is_empty() && items.iter().copied().collect::<BTreeSet<_>>().len() == items.len()
}
fn captured_producer<'a>(
    capture: &'a CapturedRevision,
    descriptor: &Producer,
) -> Result<&'a CapturedProducer, EvidenceError> {
    let producer = capture
        .producers
        .iter()
        .find(|p| p.id == descriptor.id.as_str())
        .ok_or(EvidenceError::Producer("unknown captured producer"))?;
    if producer.version != descriptor.version.as_str()
        || producer.executable_hash != descriptor.executable_hash.as_str()
        || producer.executable_hash != digest(&producer.executable_bytes)
        || producer.position_encoding != encoding(descriptor.position_encoding)
        || producer.id.is_empty()
        || producer.version.is_empty()
        || producer.artifact_bytes.is_some() != producer.artifact_hash.is_some()
        || (matches!(descriptor.kind, ProducerKind::Native)
            != (producer.artifact_bytes.is_none() && producer.artifact_hash.is_none()))
        || producer
            .artifact_bytes
            .as_ref()
            .zip(producer.artifact_hash.as_ref())
            .is_some_and(|(bytes, hash)| digest(bytes) != *hash)
        || !nonempty_unique(
            &descriptor
                .languages
                .iter()
                .map(|l| language_order(*l))
                .collect::<Vec<_>>(),
        )
    {
        return Err(EvidenceError::Producer(
            "producer metadata or bytes mismatch",
        ));
    }
    Ok(producer)
}

fn captured_identity(
    capture: &CapturedRevision,
    source: &SourceSet,
) -> Result<String, EvidenceError> {
    let languages = crate::semantic_identity::canonical_json(&source.languages)
        .map_err(|_| EvidenceError::Revision("invalid languages"))?;
    let dependencies = crate::semantic_identity::canonical_json(&capture.dependency_source_sets)
        .map_err(|_| EvidenceError::Revision("invalid dependencies"))?;
    let inputs = serde_json::to_vec(&capture.source_inputs)
        .map_err(|_| EvidenceError::Revision("invalid inputs"))?;
    let producers = serde_json::to_vec(
        &capture
            .producers
            .iter()
            .map(|p| {
                (
                    &p.id,
                    &p.tool_name,
                    &p.version,
                    &p.position_encoding,
                    &p.executable_hash,
                    &p.artifact_hash,
                )
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|_| EvidenceError::Revision("invalid producers"))?;
    let mut digest = Sha256::new();
    for part in [
        b"baleyg-captured-revision-v1".as_slice(),
        capture.source_set_id.as_bytes(),
        capture.root_id.as_bytes(),
        &capture.manifest_bytes,
        &languages,
        &dependencies,
        &inputs,
        &capture.toolchain_bytes,
        &capture.config_bytes,
        &capture.dependency_bytes,
        &producers,
    ] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part);
    }
    Ok(format!("rev:v1:{}", hex::encode(digest.finalize())))
}

/// Checks the closed producer/source-set/revision/document/coverage admission families.
/// Later validator waves add native and semantic facts; neither is admitted here.
pub fn validate_capture(
    capture: &CapturedRevision,
    evidence: &Evidence,
) -> Result<(), EvidenceError> {
    let context = &evidence.context;
    let source = &context.source_set;
    let revision = &context.revision;
    if source.id.as_str() != capture.source_set_id
        || source.root_id.as_str() != capture.root_id
        || source.id.as_str().starts_with('/')
        || capture.source_set_id.is_empty()
        || capture.root_id.is_empty()
        || !nonempty_unique(
            &source
                .languages
                .iter()
                .map(|l| language_order(*l))
                .collect::<Vec<_>>(),
        )
        || !source
            .dependencies
            .iter()
            .map(|d| d.as_str())
            .eq(capture.dependency_source_sets.iter().map(String::as_str))
        || !ordered_unique_or_empty(&source.dependencies)
        || source
            .dependencies
            .iter()
            .any(|d| d.as_str() == source.id.as_str())
    {
        return Err(EvidenceError::SourceSet("source set is not admitted"));
    }
    let mut producer_ids = BTreeSet::new();
    for producer in std::iter::once(&context.producer).chain(&evidence.producers) {
        if !producer_ids.insert(producer.id.as_str())
            || producer
                .languages
                .iter()
                .any(|l| !source.languages.contains(l))
        {
            return Err(EvidenceError::Producer("duplicate or unadmitted producer"));
        }
        captured_producer(capture, producer)?;
    }
    if producer_ids.len() != capture.producers.len()
        || !matches!(context.producer.kind, ProducerKind::Native)
    {
        return Err(EvidenceError::Producer(
            "missing or invalid native producer",
        ));
    }
    if revision.id.as_str() != capture.revision_id
        || capture.revision_id != captured_identity(capture, source)?
        || revision.source_set_id != source.id
        || revision.toolchain_hash.as_str() != digest(&capture.toolchain_bytes)
        || revision.config_hash.as_str() != digest(&capture.config_bytes)
        || revision.dependency_hash.as_str() != digest(&capture.dependency_bytes)
        || revision.toolchain_hash.as_str() != capture.toolchain_hash
        || revision.config_hash.as_str() != capture.config_hash
        || revision.dependency_hash.as_str() != capture.dependency_hash
        || revision.documents.len() != capture.documents.len()
    {
        return Err(EvidenceError::Revision(
            "revision basis differs from captured bytes",
        ));
    }
    let mut manifest = Vec::new();
    let mut previous = None;
    for (document, captured) in revision.documents.iter().zip(&capture.documents) {
        let tuple = (
            language_order(document.key.language),
            document.key.path.as_str().as_bytes(),
        );
        if previous.is_some_and(|prev| prev >= tuple) {
            return Err(EvidenceError::Revision("documents not strictly ordered"));
        }
        previous = Some(tuple);
        if document.key != captured.key
            || document.key.source_set_id != source.id
            || !source.languages.contains(&document.key.language)
            || document.revision_id != revision.id
            || document.content_hash.as_str() != digest(&captured.bytes)
            || captured.content_hash != document.content_hash.as_str()
            || document.byte_length.get() != captured.bytes.len() as u64
            || captured.byte_length != document.byte_length.get()
            || std::str::from_utf8(&captured.bytes).is_err()
        {
            return Err(EvidenceError::Document(
                "document bytes, key or length differ",
            ));
        }
        manifest.push(
            serde_json::json!({"document": document.key, "contentHash": document.content_hash}),
        );
    }
    if crate::semantic_identity::canonical_json(&manifest)
        .map_err(|_| EvidenceError::Revision("invalid manifest"))?
        != capture.manifest_bytes
    {
        return Err(EvidenceError::Revision(
            "manifest differs from captured documents",
        ));
    }
    let mut tuples = BTreeSet::new();
    for coverage in &evidence.coverage {
        let tuple = (
            coverage.producer_id.as_str(),
            language_order(coverage.language),
            coverage.document_path.as_str(),
            coverage.revision_id.as_str(),
        );
        if !tuples.insert(tuple)
            || !producer_ids.contains(coverage.producer_id.as_str())
            || coverage.source_set_id != source.id
            || coverage.revision_id != revision.id
            || !revision.documents.iter().any(|d| {
                d.key.language == coverage.language && d.key.path == coverage.document_path
            })
        {
            return Err(EvidenceError::Coverage(
                "duplicate, dangling or mismatched coverage tuple",
            ));
        }
        let producer = std::iter::once(&context.producer)
            .chain(&evidence.producers)
            .find(|p| p.id == coverage.producer_id)
            .expect("admitted producer");
        if !producer.languages.contains(&coverage.language)
            || !ordered_unique_or_empty(
                &coverage
                    .supported_roles
                    .iter()
                    .map(|r| role_order(*r))
                    .collect::<Vec<_>>(),
            )
            || !ordered_unique_or_empty(
                &coverage
                    .observed_roles
                    .iter()
                    .map(|r| role_order(*r))
                    .collect::<Vec<_>>(),
            )
            || coverage
                .supported_roles
                .iter()
                .chain(&coverage.observed_roles)
                .any(|r| coverage.language == Language::Java && *r == Role::Alias)
            || coverage
                .observed_roles
                .iter()
                .any(|r| !coverage.supported_roles.contains(r))
            || !valid_coverage(coverage)
        {
            return Err(EvidenceError::Coverage("invalid coverage state or roles"));
        }
    }
    if tuples.len() != producer_ids.len() * revision.documents.len() {
        return Err(EvidenceError::Coverage(
            "missing producer/document coverage tuple",
        ));
    }
    for id in &producer_ids {
        for document in &revision.documents {
            if !tuples.contains(&(
                *id,
                language_order(document.key.language),
                document.key.path.as_str(),
                revision.id.as_str(),
            )) {
                return Err(EvidenceError::Coverage(
                    "missing producer/document coverage tuple",
                ));
            }
        }
    }
    Ok(())
}
fn ordered_unique_or_empty<T: Ord>(items: &[T]) -> bool {
    items.windows(2).all(|w| w[0] < w[1])
}
fn role_order(role: Role) -> u8 {
    match role {
        Role::Definition => 0,
        Role::Read => 1,
        Role::Write => 2,
        Role::Call => 3,
        Role::Type => 4,
        Role::Import => 5,
        Role::Alias => 6,
    }
}
fn valid_coverage(c: &Coverage) -> bool {
    let flags = match c.state {
        CoverageState::NotRequested => (!c.requested, !c.selected, false),
        CoverageState::Omitted | CoverageState::Unsupported => (c.requested, !c.selected, true),
        CoverageState::Failed | CoverageState::Partial => (c.requested, c.selected, true),
        CoverageState::Complete => (c.requested, c.selected, false),
    };
    flags.0 && flags.1 && (c.diagnostic.is_some() == flags.2)
}

fn native_error(message: &'static str) -> EvidenceError {
    EvidenceError::Native(message)
}

fn measured<'a>(
    doc: &'a CapturedDocument,
    witness: &CapturedNativeWitness,
) -> Result<(&'a [u8], &'a [u8]), EvidenceError> {
    let bytes = &doc.bytes;
    let node = doc
        .syntax
        .get(witness.node_id)
        .ok_or_else(|| native_error("missing AST node"))?;
    if node.id != witness.node_id
        || node.parent_id != witness.parent_id
        || node.kind != witness.node_kind
        || node.start_byte != witness.start_byte
        || node.end_byte != witness.end_byte
        || node.source_bytes
            != bytes
                .get(witness.start_byte..witness.end_byte)
                .unwrap_or_default()
        || witness.token_start_byte < witness.start_byte
        || witness.token_end_byte > witness.end_byte
        || witness.token_start_byte >= witness.token_end_byte
        || bytes.get(witness.token_start_byte..witness.token_end_byte)
            != Some(witness.token_bytes.as_slice())
        || (witness.candidate_kind == NativeCandidateKind::Declaration
            && !doc.syntax.iter().any(|n| {
                n.parent_id == Some(node.id)
                    && n.start_byte == witness.token_start_byte
                    && n.end_byte == witness.token_end_byte
            })
            && (witness.token_start_byte != witness.start_byte
                || witness.token_end_byte != witness.end_byte))
    {
        return Err(native_error("candidate differs from captured AST or bytes"));
    }
    let name = bytes
        .get(witness.token_start_byte..witness.token_end_byte)
        .ok_or_else(|| native_error("invalid candidate token"))?;
    if witness.name_bytes != name && !witness.verified_member_token {
        return Err(native_error("candidate name differs from source token"));
    }
    if witness.candidate_kind == NativeCandidateKind::Declaration {
        let header_end = doc
            .syntax
            .iter()
            .filter(|child| {
                child.parent_id == Some(node.id)
                    && matches!(child.field_name.as_deref(), Some("body" | "value"))
            })
            .map(|child| child.start_byte)
            .min()
            .unwrap_or(witness.token_end_byte);
        let end = header_end
            .saturating_sub(node.start_byte)
            .min(node.source_bytes.len());
        if witness.header_bytes != node.source_bytes[..end]
            || witness.name_bytes != name
            || !doc.syntax.iter().any(|child| {
                child.parent_id == Some(node.id)
                    && child.start_byte == witness.token_start_byte
                    && child.end_byte == witness.token_end_byte
            }) && (witness.token_start_byte != witness.start_byte
                || witness.token_end_byte != witness.end_byte)
        {
            return Err(native_error(
                "declaration witness differs from measured header or name",
            ));
        }
    } else if !witness.header_bytes.is_empty() {
        return Err(native_error("non-declaration witness has header bytes"));
    }
    if doc.key.language == Language::Java {
        let expected = if matches!(
            node.kind.as_str(),
            "method_invocation" | "object_creation_expression" | "explicit_constructor_invocation"
        ) {
            NativeCandidateKind::Invocation
        } else if matches!(
            node.kind.as_str(),
            "if_statement"
                | "while_statement"
                | "do_statement"
                | "for_statement"
                | "enhanced_for_statement"
                | "switch_expression"
                | "switch_block_statement_group"
                | "switch_rule"
                | "try_statement"
                | "try_with_resources_statement"
                | "catch_clause"
                | "finally_clause"
                | "synchronized_statement"
                | "ternary_expression"
        ) {
            NativeCandidateKind::ControlRegion
        } else if matches!(
            node.kind.as_str(),
            "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
                | "annotation_type_declaration"
                | "method_declaration"
                | "annotation_type_element_declaration"
                | "constructor_declaration"
                | "compact_constructor_declaration"
                | "lambda_expression"
        ) {
            NativeCandidateKind::Declaration
        } else {
            NativeCandidateKind::Occurrence
        };
        if witness.stable_id.is_some() && witness.candidate_kind != expected {
            return Err(native_error("Java candidate kind differs from AST kind"));
        }
    }
    if witness.candidate_kind == NativeCandidateKind::Invocation {
        let expected = match doc.key.language {
            Language::Java if node.kind == "method_invocation" && witness.verified_member_token => {
                doc.syntax.iter().find(|candidate| {
                    candidate.parent_id == Some(node.id)
                        && candidate.field_name.as_deref() == Some("name")
                        && candidate.start_byte == witness.token_start_byte
                        && candidate.end_byte == witness.token_end_byte
                })
            }
            _ => None,
        };
        if doc.key.language == Language::Java && witness.verified_member_token && expected.is_none()
        {
            return Err(native_error(
                "member token is not the invocation name field",
            ));
        }
        if let Some(member) = expected {
            let decoded = std::str::from_utf8(&member.source_bytes)
                .ok()
                .and_then(|s| crate::semantic_identity::lookup_key(doc.key.language, s).ok());
            if witness.spelling.as_deref() != decoded.as_deref()
                || witness.name_bytes != member.source_bytes
            {
                return Err(native_error("member spelling differs from AST token"));
            }
        }
    } else if witness.verified_member_token || witness.spelling.is_some() {
        return Err(native_error("non-invocation has member-token metadata"));
    }
    let mut owner = node.parent_id.unwrap_or(0);
    while owner > 0 {
        let kind = doc.syntax[owner].kind.as_str();
        if kind.contains("declaration")
            || kind.contains("definition")
            || matches!(
                kind,
                "function_item"
                    | "method_definition"
                    | "class"
                    | "function_expression"
                    | "generator_function"
                    | "arrow_function"
                    | "lambda_expression"
                    | "lambda"
                    | "closure_expression"
                    | "impl_item"
                    | "trait_item"
                    | "struct_item"
                    | "enum_item"
                    | "union_item"
                    | "mod_item"
            )
        {
            break;
        }
        owner = doc.syntax[owner].parent_id.unwrap_or(0);
    }
    if witness.owner_id != owner {
        return Err(native_error("candidate owner differs from AST"));
    }
    let whole = bytes
        .get(witness.start_byte..witness.end_byte)
        .ok_or_else(|| native_error("invalid AST range"))?;
    let token = bytes
        .get(witness.token_start_byte..witness.token_end_byte)
        .ok_or_else(|| native_error("invalid token range"))?;
    std::str::from_utf8(whole).map_err(|_| native_error("non-scalar AST range"))?;
    std::str::from_utf8(token).map_err(|_| native_error("non-scalar token range"))?;
    Ok((whole, token))
}
fn java_header(
    doc: &CapturedDocument,
    witness: &CapturedNativeWitness,
) -> Result<(Key, Header), EvidenceError> {
    let node = &doc.syntax[witness.node_id];
    let children = |parent: usize| {
        doc.syntax
            .iter()
            .filter(move |n| n.parent_id == Some(parent))
    };
    let field = |parent: usize, label: &str| {
        children(parent).find(|n| n.field_name.as_deref() == Some(label))
    };
    let text = |node: &crate::indexer::CapturedSyntaxNode| {
        std::str::from_utf8(&node.source_bytes)
            .ok()
            .and_then(|s| Text::new(s.to_owned()))
            .ok_or_else(|| native_error("invalid Java header token"))
    };
    let kind = match node.kind.as_str() {
        "class_declaration"
        | "interface_declaration"
        | "enum_declaration"
        | "record_declaration"
        | "annotation_type_declaration" => Kind::Type,
        "method_declaration" | "annotation_type_element_declaration" => Kind::Method,
        "constructor_declaration" | "compact_constructor_declaration" => Kind::Constructor,
        "lambda_expression" => Kind::AnonymousFunction,
        _ => return Err(native_error("unsupported Java declaration kind")),
    };
    let name = field(node.id, "name").map(&text).transpose()?;
    let modifiers = children(node.id)
        .find(|n| n.kind == "modifiers")
        .map(|n| {
            children(n.id)
                .filter(|part| part.kind != "annotation" && part.kind != "marker_annotation")
                .map(&text)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let type_parameters = field(node.id, "type_parameters")
        .map(|n| {
            children(n.id)
                .filter(|part| part.kind == "type_parameter")
                .map(&text)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let parameters = field(node.id, "parameters")
        .map(|n| {
            children(n.id)
                .filter(|part| {
                    matches!(
                        part.kind.as_str(),
                        "formal_parameter" | "spread_parameter" | "receiver_parameter"
                    )
                })
                .map(|part| {
                    Ok(Parameter {
                        name: field(part.id, "name").map(&text).transpose()?,
                        r#type: field(part.id, "type").map(&text).transpose()?,
                        variadic: part.kind == "spread_parameter",
                    })
                })
                .collect::<Result<Vec<_>, EvidenceError>>()
        })
        .transpose()?
        .unwrap_or_default();
    let signature = matches!(kind, Kind::Method | Kind::Constructor).then(|| Signature {
        parameter_types: parameters.iter().filter_map(|p| p.r#type.clone()).collect(),
        type_parameter_count: UInt::new(type_parameters.len() as u64).unwrap(),
        variadic: parameters.last().is_some_and(|p| p.variadic),
    });
    let bases = doc
        .heritage
        .iter()
        .filter(|h| h.class_node_id == node.id)
        .map(|h| {
            std::str::from_utf8(
                doc.bytes
                    .get(h.base_start..h.base_end)
                    .ok_or_else(|| native_error("invalid heritage range"))?,
            )
            .ok()
            .and_then(|s| Text::new(s.to_owned()))
            .ok_or_else(|| native_error("invalid heritage token"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let key = Key {
        kind,
        name: name.clone(),
        signature,
        ordinal: UInt::new(0).unwrap(),
    };
    let header = Header {
        kind,
        name,
        modifiers,
        type_parameters,
        parameters,
        result_type: field(node.id, "type").map(&text).transpose()?,
        bases,
    };
    Ok((key, header))
}

fn measured_lineage(
    doc: &CapturedDocument,
    witness: &CapturedNativeWitness,
    file: &NativeFileEvidence,
) -> Result<Vec<String>, EvidenceError> {
    let module = crate::semantic_identity::syntax_id(
        &doc.key.source_set_id,
        &doc.key.path,
        doc.key.language,
        &[],
        &Key {
            kind: Kind::Module,
            name: None,
            signature: None,
            ordinal: UInt::new(0).unwrap(),
        },
    )
    .map_err(|_| native_error("invalid module identity"))?;
    let mut ids = vec![module.as_str().to_owned()];
    let mut parents = Vec::new();
    let mut parent = doc.syntax[witness.node_id].parent_id;
    while let Some(node_id) = parent {
        if let Some(candidate) = doc.native_candidates.iter().find(|candidate| {
            candidate.node_id == node_id
                && candidate.candidate_kind == NativeCandidateKind::Declaration
                && candidate.stable_id.is_some()
        }) {
            let row = file
                .declarations
                .iter()
                .find(|row| candidate.stable_id.as_deref() == Some(row.syntax_id.as_str()))
                .ok_or_else(|| native_error("actual enclosing declaration is absent"))?;
            parents.push(row.syntax_id.as_str().to_owned());
        }
        parent = doc.syntax[node_id].parent_id;
    }
    ids.extend(parents.into_iter().rev());
    Ok(ids)
}

fn same_range(range: &Range, start: usize, end: usize) -> bool {
    range.start.get() == start as u64 && range.end.get() == end as u64
}
fn contains(outer: &Range, inner: &Range) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// Validate only captured native syntax and provenance. No semantic facts or joins are admitted.
pub fn validate_native(
    capture: &CapturedRevision,
    evidence: &Evidence,
) -> Result<(), EvidenceError> {
    validate_capture(capture, evidence)?;
    let revision = &evidence.context.revision;
    let producer = &evidence.context.producer;
    let mut seen_files = BTreeSet::new();
    let mut seen_provenance = BTreeSet::new();
    let mut seen_ids = BTreeSet::new();
    for file in &evidence.native_files {
        let doc = capture
            .documents
            .iter()
            .find(|d| d.key == file.document.key)
            .ok_or_else(|| native_error("unknown native document"))?;
        let coverage = evidence
            .coverage
            .iter()
            .find(|c| {
                c.producer_id == producer.id
                    && c.language == doc.key.language
                    && c.document_path == doc.key.path
            })
            .ok_or_else(|| native_error("missing native coverage"))?;
        if !seen_files.insert((language_order(doc.key.language), doc.key.path.as_str()))
            || file.document
                != *revision
                    .documents
                    .iter()
                    .find(|d| d.key == doc.key)
                    .ok_or_else(|| native_error("missing admitted document"))?
            || file.coverage != *coverage
            || coverage.state != CoverageState::Complete
            || file.provenance.producer_id != producer.id
            || file.provenance.document != doc.key
            || file.provenance.revision_id != revision.id
            || file.provenance.content_hash != file.document.content_hash
            || file.provenance.evidence_kind != EvidenceKind::MeasuredSyntax
            || file.provenance.basis.is_some()
            || file.provenance.freshness != Freshness::Fresh
            || !seen_provenance.insert(file.provenance.id.as_str())
        {
            return Err(native_error("invalid native file or provenance"));
        }
        let witnesses = &doc.native_candidates;
        for witness in witnesses {
            measured(doc, witness)?;
        }
        let declaration = |id: &SyntaxId| file.declarations.iter().find(|d| d.syntax_id == *id);
        for row in &file.declarations {
            let matches: Vec<_> = witnesses
                .iter()
                .filter(|w| {
                    w.candidate_kind == NativeCandidateKind::Declaration
                        && w.stable_id.as_deref() == Some(row.syntax_id.as_str())
                })
                .collect();
            if matches.len() != 1 {
                return Err(native_error("declaration not uniquely captured"));
            }
            let witness = matches[0];
            if witness.ancestor_ids != measured_lineage(doc, witness, file)? {
                return Err(native_error(
                    "declaration ancestor IDs differ from AST chain",
                ));
            }
            let named = witness.token_start_byte != witness.start_byte
                || witness.token_end_byte != witness.end_byte;
            if !seen_ids.insert(row.syntax_id.as_str().to_owned())
                || row.document != doc.key
                || row.revision_id != revision.id
                || row.provenance_id != file.provenance.id
                || !same_range(&row.range, witness.start_byte, witness.end_byte)
                || row.kind != row.key.kind
                || row.name != row.key.name
                || row.header.kind != row.kind
                || row.header.name != row.name
                || named != row.name_range.is_some()
                || row.name_range.as_ref().is_some_and(|r| {
                    !same_range(r, witness.token_start_byte, witness.token_end_byte)
                })
                || row
                    .name
                    .as_ref()
                    .is_some_and(|name| name.as_str().as_bytes() != witness.name_bytes)
                || row.lookup_key.as_ref().map(Text::as_str)
                    != row
                        .name
                        .as_ref()
                        .and_then(|name| {
                            crate::semantic_identity::lookup_key(doc.key.language, name.as_str())
                                .ok()
                        })
                        .as_deref()
                || crate::semantic_identity::syntax_id(
                    &doc.key.source_set_id,
                    &doc.key.path,
                    doc.key.language,
                    &row.ancestors,
                    &row.key,
                )
                .ok()
                .as_ref()
                    != Some(&row.syntax_id)
            {
                return Err(native_error(
                    "declaration ID, token, header or lookup mismatch",
                ));
            }
            if doc.key.language == Language::Java {
                let (measured_key, measured_header) = java_header(doc, witness)?;
                let node = &doc.syntax[witness.node_id];
                let parent_declaration = |candidate: &CapturedNativeWitness| {
                    let mut parent = doc.syntax[candidate.node_id].parent_id;
                    while let Some(id) = parent {
                        if let Some(found) = witnesses.iter().find(|w| {
                            w.candidate_kind == NativeCandidateKind::Declaration
                                && w.node_id == id
                                && w.stable_id.is_some()
                        }) {
                            return Some(found.node_id);
                        }
                        parent = doc.syntax[id].parent_id;
                    }
                    None
                };
                let mut ancestor_nodes = Vec::new();
                let mut parent = parent_declaration(witness);
                while let Some(id) = parent {
                    ancestor_nodes.push(id);
                    let enclosing = witnesses
                        .iter()
                        .find(|w| {
                            w.node_id == id && w.candidate_kind == NativeCandidateKind::Declaration
                        })
                        .ok_or_else(|| native_error("missing ancestor witness"))?;
                    parent = parent_declaration(enclosing);
                }
                ancestor_nodes.reverse();
                let module = Key {
                    kind: Kind::Module,
                    name: None,
                    signature: None,
                    ordinal: UInt::new(0).unwrap(),
                };
                let mut measured_ancestors = vec![module];
                for id in ancestor_nodes {
                    let ancestor = file
                        .declarations
                        .iter()
                        .find(|d| {
                            witnesses.iter().any(|w| {
                                w.node_id == id
                                    && w.stable_id.as_deref() == Some(d.syntax_id.as_str())
                            })
                        })
                        .ok_or_else(|| native_error("missing actual ancestor declaration"))?;
                    measured_ancestors.push(ancestor.key.clone());
                }
                let siblings = witnesses
                    .iter()
                    .filter(|candidate| {
                        candidate.candidate_kind == NativeCandidateKind::Declaration
                            && candidate.stable_id.is_some()
                            && parent_declaration(candidate) == parent_declaration(witness)
                    })
                    .filter_map(|candidate| {
                        let (key, _) = java_header(doc, candidate).ok()?;
                        (key.kind == measured_key.kind
                            && key.name == measured_key.name
                            && key.signature == measured_key.signature)
                            .then_some(candidate)
                    })
                    .collect::<Vec<_>>();
                let earlier = siblings
                    .iter()
                    .filter(|candidate| {
                        (candidate.start_byte, candidate.end_byte)
                            < (witness.start_byte, witness.end_byte)
                    })
                    .count();
                if row.ancestors != measured_ancestors
                    || row.key.ordinal.get() != earlier as u64
                    || node.kind != witness.node_kind
                    || row.key.kind != measured_key.kind
                    || row.key.name != measured_key.name
                    || row.key.signature != measured_key.signature
                    || row.header != measured_header
                {
                    return Err(native_error("Java key or complete header differs from AST"));
                }
            }
            if row.kind == Kind::Type {
                let measured_bases: Vec<_> = doc
                    .heritage
                    .iter()
                    .filter(|h| h.class_node_id == witness.node_id)
                    .map(|h| {
                        let base = doc
                            .bytes
                            .get(h.base_start..h.base_end)
                            .ok_or_else(|| native_error("heritage range outside source"))?;
                        let class = doc
                            .syntax
                            .get(h.class_node_id)
                            .ok_or_else(|| native_error("heritage class missing"))?;
                        if class.id != witness.node_id
                            || h.owner_id != class.id && h.owner_id != class.parent_id.unwrap_or(0)
                            || doc.bytes.get(h.subclass_name_start..h.subclass_name_end)
                                != Some(witness.name_bytes.as_slice())
                            || base != h.base_bytes
                            || !doc.syntax.iter().any(|n| {
                                n.start_byte == h.base_start
                                    && n.end_byte == h.base_end
                                    && n.source_bytes == base
                            })
                        {
                            return Err(native_error("heritage witness differs from source"));
                        }
                        std::str::from_utf8(base)
                            .map_err(|_| native_error("invalid heritage bytes"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if row
                    .header
                    .bases
                    .iter()
                    .map(Text::as_str)
                    .collect::<Vec<_>>()
                    != measured_bases
                {
                    return Err(native_error(
                        "declaration bases differ from measured syntax",
                    ));
                }
            }
            if let Some(owner) = witness.ancestor_ids.last()
                && row.ancestors.last().is_none_or(|key| {
                    crate::semantic_identity::syntax_id(
                        &doc.key.source_set_id,
                        &doc.key.path,
                        doc.key.language,
                        &row.ancestors[..row.ancestors.len() - 1],
                        key,
                    )
                    .map_or(true, |id| id.as_str() != owner)
                })
            {
                return Err(native_error("declaration owner differs from AST"));
            }
        }
        let mut occurrence_ranges = Vec::new();
        for call in &file.calls {
            let matches: Vec<_> = witnesses
                .iter()
                .filter(|w| {
                    w.candidate_kind == NativeCandidateKind::Invocation
                        && w.stable_id.as_deref() == Some(call.id.as_str())
                })
                .collect();
            if matches.len() != 1 {
                return Err(native_error("call not uniquely captured"));
            }
            let w = matches[0];
            if w.ancestor_ids != measured_lineage(doc, w, file)? {
                return Err(native_error("call owner chain differs from AST"));
            }
            if !seen_ids.insert(call.id.as_str().to_owned())
                || call.document != doc.key
                || call.revision_id != revision.id
                || call.provenance_id != file.provenance.id
                || !same_range(&call.range, w.start_byte, w.end_byte)
                || w.ancestor_ids.last().map(String::as_str) != Some(call.owner_syntax_id.as_str())
                || call.callee_range.as_ref().is_some_and(|r| {
                    !w.verified_member_token || !same_range(r, w.token_start_byte, w.token_end_byte)
                })
                || (w.verified_member_token && call.callee_range.is_none())
                || call.spelling.as_ref().map(Text::as_str) != w.spelling.as_deref()
                || call
                    .callee_range
                    .as_ref()
                    .is_some_and(|r| !contains(&call.range, r))
                || declaration(&call.owner_syntax_id).is_none()
                    && !module_owner(doc, &call.owner_syntax_id)
            {
                return Err(native_error("call owner, token or source range mismatch"));
            }
            occurrence_ranges.push((
                call.owner_syntax_id.clone(),
                crate::semantic_identity::OccurrenceKind::Call,
                call.range.start.get(),
                call.range.end.get(),
            ));
        }
        for region in &file.control_regions {
            let matches: Vec<_> = witnesses
                .iter()
                .filter(|w| {
                    w.candidate_kind == NativeCandidateKind::ControlRegion
                        && w.stable_id.as_deref() == Some(region.id.as_str())
                })
                .collect();
            if matches.len() != 1 {
                return Err(native_error("region not uniquely captured"));
            }
            let w = matches[0];
            if w.ancestor_ids != measured_lineage(doc, w, file)? {
                return Err(native_error("region owner chain differs from AST"));
            }
            if !seen_ids.insert(region.id.as_str().to_owned())
                || region.document != doc.key
                || region.revision_id != revision.id
                || region.provenance_id != file.provenance.id
                || !same_range(&region.range, w.start_byte, w.end_byte)
                || region.kind.as_str() != w.node_kind
                || w.ancestor_ids.last().map(String::as_str)
                    != Some(region.owner_syntax_id.as_str())
                || declaration(&region.owner_syntax_id).is_none()
                    && !module_owner(doc, &region.owner_syntax_id)
            {
                return Err(native_error("region owner, kind or range mismatch"));
            }
            let node = &doc.syntax[w.node_id];
            let actual_arm = node.field_name.as_deref().and_then(|field| {
                matches!(field, "consequence" | "alternative" | "right" | "value").then_some(field)
            });
            if region.arm.as_ref().map(Text::as_str) != actual_arm {
                return Err(native_error("region arm differs from AST field"));
            }
            let mut ancestor_node = node.parent_id;
            let mut measured_parent = None;
            while let Some(id) = ancestor_node {
                if let Some(parent_witness) = witnesses.iter().find(|candidate| {
                    candidate.node_id == id
                        && candidate.candidate_kind == NativeCandidateKind::ControlRegion
                        && candidate.ancestor_ids.last() == w.ancestor_ids.last()
                }) {
                    measured_parent = parent_witness.stable_id.as_deref();
                    break;
                }
                ancestor_node = doc.syntax[id].parent_id;
            }
            if region.parent_id.as_ref().map(OccurrenceId::as_str) != measured_parent {
                return Err(native_error("region parent differs from AST ancestry"));
            }
            if let Some(parent) = &region.parent_id {
                let ancestor = file
                    .control_regions
                    .iter()
                    .find(|r| r.id == *parent)
                    .ok_or_else(|| native_error("missing region parent"))?;
                if ancestor.id == region.id
                    || ancestor.owner_syntax_id != region.owner_syntax_id
                    || !contains(&ancestor.range, &region.range)
                    || ancestor.range == region.range
                {
                    return Err(native_error("cyclic or foreign region parent"));
                }
            }
            occurrence_ranges.push((
                region.owner_syntax_id.clone(),
                crate::semantic_identity::OccurrenceKind::Control,
                region.range.start.get(),
                region.range.end.get(),
            ));
        }
        let ordinals = crate::semantic_identity::occurrence_ordinals(&occurrence_ranges)
            .map_err(|_| native_error("duplicate occurrence ranges"))?;
        for (row, ordinal) in file
            .calls
            .iter()
            .map(|c| (c.owner_syntax_id.clone(), c.id.clone(), c.ordinal))
            .chain(
                file.control_regions
                    .iter()
                    .map(|r| (r.owner_syntax_id.clone(), r.id.clone(), r.ordinal)),
            )
            .zip(ordinals)
        {
            if row.2 != ordinal
                || crate::semantic_identity::occurrence_id(
                    &revision.id,
                    &row.0,
                    if file.calls.iter().any(|c| c.id == row.1) {
                        crate::semantic_identity::OccurrenceKind::Call
                    } else {
                        crate::semantic_identity::OccurrenceKind::Control
                    },
                    ordinal,
                )
                .ok()
                    != Some(row.1)
            {
                return Err(native_error("incorrect occurrence ordinal or ID"));
            }
        }
        for call in &file.calls {
            let mut enclosing: Vec<_> = file
                .control_regions
                .iter()
                .filter(|r| {
                    r.owner_syntax_id == call.owner_syntax_id && contains(&r.range, &call.range)
                })
                .collect();
            enclosing.sort_by_key(|r| (r.range.start.get(), std::cmp::Reverse(r.range.end.get())));
            if enclosing.iter().map(|r| &r.id).ne(call.region_ids.iter())
                || enclosing
                    .windows(2)
                    .any(|pair| !contains(&pair[0].range, &pair[1].range))
            {
                return Err(native_error(
                    "call region ancestry differs from measured regions",
                ));
            }
        }
    }
    for document in &capture.documents {
        let selected = evidence.coverage.iter().any(|coverage| {
            coverage.producer_id == producer.id
                && coverage.language == document.key.language
                && coverage.document_path == document.key.path
                && coverage.state == CoverageState::Complete
        });
        if selected
            && !seen_files.contains(&(
                language_order(document.key.language),
                document.key.path.as_str(),
            ))
        {
            return Err(native_error("selected complete native document is missing"));
        }
    }
    Ok(())
}
fn module_owner(doc: &CapturedDocument, id: &SyntaxId) -> bool {
    crate::semantic_identity::syntax_id(
        &doc.key.source_set_id,
        &doc.key.path,
        doc.key.language,
        &[],
        &Key {
            kind: Kind::Module,
            name: None,
            signature: None,
            ordinal: UInt::new(0).unwrap(),
        },
    )
    .is_ok_and(|module| module == *id)
}

pub fn validate_evidence(
    capture: &CapturedRevision,
    evidence: &Evidence,
    requested_snapshot: &CapturedRevision,
) -> Result<Evidence, EvidenceError> {
    validate_capture(capture, evidence)?;
    if capture != requested_snapshot {
        return Err(EvidenceError::Revision(
            "snapshot comparison awaits basis validation",
        ));
    }
    if evidence.native_files.is_empty() {
        return Err(EvidenceError::NotYetValidated(
            "native syntax and later phases not supplied",
        ));
    }
    validate_native(capture, evidence)?;
    if !evidence.native_files.is_empty()
        || !evidence.provenance.is_empty()
        || !evidence.symbols.is_empty()
        || !evidence.declaration_bindings.is_empty()
        || !evidence.type_relationships.is_empty()
        || !evidence.references.is_empty()
        || !evidence.call_bindings.is_empty()
    {
        return Err(EvidenceError::NotYetValidated(
            "fact families require later validator phases",
        ));
    }
    Err(EvidenceError::NotYetValidated(
        "native syntax, provenance, basis and semantic facts require later validator phases",
    ))
}
