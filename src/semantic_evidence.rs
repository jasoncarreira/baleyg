//! Admission of immutable captured bytes before syntax and semantic validation.
use crate::{
    indexer::{
        CapturedDocument, CapturedNativeWitness, CapturedProducer, CapturedRevision,
        NativeCandidateKind,
    },
    model::v1::*,
};
use protobuf::Message;
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
    Basis(&'static str),
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
    if capture.lookup_dependencies.len() != capture.documents.len()
        || capture
            .lookup_dependencies
            .iter()
            .zip(&capture.documents)
            .any(|((key, keys), document)| {
                key != &document.key
                    || crate::indexer::captured_lookup_dependencies(document)
                        .map_or(true, |derived| derived != *keys)
            })
    {
        return Err(EvidenceError::Basis(
            "lookup dependencies differ from captured source positions",
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
    let supported_kind = match doc.key.language {
        Language::Java => match node.kind.as_str() {
            "method_invocation"
            | "object_creation_expression"
            | "explicit_constructor_invocation" => Some(NativeCandidateKind::Invocation),
            "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration"
            | "method_declaration"
            | "annotation_type_element_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration"
            | "lambda_expression" => Some(NativeCandidateKind::Declaration),
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
            | "ternary_expression" => Some(NativeCandidateKind::ControlRegion),
            _ => None,
        },
        Language::Javascript => match node.kind.as_str() {
            "call_expression" | "new_expression" => Some(NativeCandidateKind::Invocation),
            "class_declaration"
            | "class"
            | "method_definition"
            | "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "generator_function"
            | "arrow_function" => Some(NativeCandidateKind::Declaration),
            "if_statement"
            | "for_statement"
            | "for_in_statement"
            | "while_statement"
            | "do_statement"
            | "switch_statement"
            | "switch_case"
            | "switch_default"
            | "try_statement"
            | "catch_clause"
            | "finally_clause"
            | "ternary_expression"
            | "conditional_expression"
            | "statement_block"
            | "expression_statement"
            | "binary_expression"
            | "class_static_block" => Some(NativeCandidateKind::ControlRegion),
            _ => None,
        },
        Language::Python => match node.kind.as_str() {
            "call" => Some(NativeCandidateKind::Invocation),
            "class_definition" | "function_definition" | "lambda" => {
                Some(NativeCandidateKind::Declaration)
            }
            "if_statement"
            | "elif_clause"
            | "else_clause"
            | "for_statement"
            | "while_statement"
            | "try_statement"
            | "except_clause"
            | "finally_clause"
            | "with_statement"
            | "match_statement"
            | "boolean_operator"
            | "conditional_expression"
            | "list_comprehension"
            | "set_comprehension"
            | "dictionary_comprehension"
            | "generator_expression" => Some(NativeCandidateKind::ControlRegion),
            _ => None,
        },
        Language::Rust => match node.kind.as_str() {
            "call_expression" | "method_call_expression" => Some(NativeCandidateKind::Invocation),
            "function_item" | "closure_expression" | "impl_item" | "trait_item" | "struct_item"
            | "enum_item" | "union_item" | "mod_item" => Some(NativeCandidateKind::Declaration),
            "if_expression" | "match_arm" | "loop_expression" | "while_expression"
            | "for_expression" | "async_block" | "unsafe_block" => {
                Some(NativeCandidateKind::ControlRegion)
            }
            _ => None,
        },
    };
    if witness.stable_id.is_some() && supported_kind != Some(witness.candidate_kind.clone()) {
        return Err(native_error(
            "candidate kind differs from supported AST kind",
        ));
    }
    if witness.candidate_kind == NativeCandidateKind::Invocation {
        let child = |parent: usize, field: &str| {
            doc.syntax
                .iter()
                .find(|n| n.parent_id == Some(parent) && n.field_name.as_deref() == Some(field))
        };
        let function = child(node.id, "function");
        let member = match doc.key.language {
            Language::Java
                if node.kind == "method_invocation" && child(node.id, "object").is_some() =>
            {
                child(node.id, "name").filter(|n| n.kind == "identifier")
            }
            Language::Javascript => function
                .filter(|n| {
                    n.kind == "member_expression"
                        && !doc
                            .syntax
                            .iter()
                            .any(|c| c.parent_id == Some(n.id) && c.kind == "optional_chain")
                })
                .and_then(|n| child(n.id, "property"))
                .filter(|n| {
                    matches!(
                        n.kind.as_str(),
                        "property_identifier" | "private_property_identifier"
                    )
                }),
            Language::Python => function
                .filter(|n| n.kind == "attribute" && child(n.id, "object").is_some())
                .and_then(|n| child(n.id, "attribute"))
                .filter(|n| n.kind == "identifier"),
            Language::Rust => function
                .filter(|n| n.kind == "field_expression" && child(n.id, "value").is_some())
                .and_then(|n| child(n.id, "field"))
                .filter(|n| n.kind == "field_identifier"),
            _ => None,
        };
        let verified = member.is_some();
        let spelling = if let Some(member) = member {
            std::str::from_utf8(&member.source_bytes)
                .ok()
                .and_then(|raw| crate::semantic_identity::lookup_key(doc.key.language, raw).ok())
        } else if doc.key.language == Language::Javascript {
            function
                .filter(|n| n.kind == "identifier")
                .and_then(|n| std::str::from_utf8(&n.source_bytes).ok())
                .and_then(|raw| crate::semantic_identity::lookup_key(doc.key.language, raw).ok())
        } else {
            None
        };
        if witness.verified_member_token != verified
            || witness.spelling != spelling
            || member.is_some_and(|n| {
                n.start_byte != witness.token_start_byte
                    || n.end_byte != witness.token_end_byte
                    || n.source_bytes != witness.name_bytes
            })
        {
            return Err(native_error("member token or spelling differs from AST"));
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

fn non_java_header(
    doc: &CapturedDocument,
    witness: &CapturedNativeWitness,
) -> Result<(Key, Header), EvidenceError> {
    let node = &doc.syntax[witness.node_id];
    let children = |id: usize| doc.syntax.iter().filter(move |n| n.parent_id == Some(id));
    let field =
        |id: usize, label: &str| children(id).find(|n| n.field_name.as_deref() == Some(label));
    let text = |n: &crate::indexer::CapturedSyntaxNode| {
        std::str::from_utf8(&n.source_bytes)
            .ok()
            .and_then(|s| Text::new(s.to_owned()))
            .ok_or_else(|| native_error("invalid declaration source token"))
    };
    let kind = match (doc.key.language, node.kind.as_str()) {
        (Language::Javascript, "class_declaration" | "class")
        | (Language::Python, "class_definition")
        | (
            Language::Rust,
            "impl_item" | "trait_item" | "struct_item" | "enum_item" | "union_item" | "mod_item",
        ) => Kind::Type,
        (Language::Javascript, "method_definition") => Kind::Method,
        (Language::Javascript, "function_expression" | "generator_function")
            if field(node.id, "name").is_none() =>
        {
            Kind::AnonymousFunction
        }
        (
            Language::Javascript,
            "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "generator_function",
        )
        | (Language::Python, "function_definition") => Kind::Function,
        (Language::Javascript, "arrow_function")
        | (Language::Python, "lambda")
        | (Language::Rust, "closure_expression") => Kind::AnonymousFunction,
        (Language::Rust, "function_item") => {
            let parent = node.parent_id.and_then(|id| doc.syntax.get(id));
            let grandparent = parent
                .and_then(|n| n.parent_id)
                .and_then(|id| doc.syntax.get(id));
            if parent.is_some_and(|n| n.kind == "declaration_list")
                && grandparent
                    .is_some_and(|n| matches!(n.kind.as_str(), "impl_item" | "trait_item"))
            {
                Kind::Method
            } else {
                Kind::Function
            }
        }
        _ => return Err(native_error("unsupported non-Java declaration kind")),
    };
    let named = field(node.id, "name");
    let name = if doc.key.language == Language::Rust && node.kind == "impl_item" {
        let ty = field(node.id, "type").ok_or_else(|| native_error("impl type missing"))?;
        let prefix = field(node.id, "trait")
            .map(|n| format!("{} for ", String::from_utf8_lossy(&n.source_bytes)))
            .unwrap_or_default();
        Text::new(format!(
            "impl {prefix}{}",
            String::from_utf8_lossy(&ty.source_bytes)
        ))
    } else if named.is_none()
        && matches!(
            node.kind.as_str(),
            "function_expression"
                | "generator_function"
                | "arrow_function"
                | "lambda"
                | "closure_expression"
        )
    {
        None
    } else {
        named.map(&text).transpose()?
    };
    let kind = if doc.key.language == Language::Python
        && kind == Kind::Function
        && enclosing_declaration(doc, node.id)
            .is_some_and(|parent| doc.syntax[parent].kind == "class_definition")
    {
        Kind::Method
    } else {
        kind
    };
    let key = Key {
        kind,
        name: name.clone(),
        signature: None,
        ordinal: UInt::new(0).unwrap(),
    };
    let mut modifiers = Vec::new();
    for part in children(node.id).filter(|n| n.kind == "visibility_modifier") {
        modifiers.push(text(part)?);
    }
    let type_parameters = field(node.id, "type_parameters")
        .map(|n| {
            children(n.id)
                .filter(|part| part.candidate_kind.is_some() && part.kind != "type_parameter_list")
                .map(&text)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let parameters = field(node.id, "parameters")
        .or_else(|| field(node.id, "formal_parameters"))
        .map(|n| {
            children(n.id)
                .filter(|part| part.candidate_kind.is_some())
                .map(|part| {
                    Ok(Parameter {
                        name: field(part.id, "name").map(&text).transpose()?,
                        r#type: field(part.id, "type").map(&text).transpose()?,
                        variadic: matches!(
                            part.kind.as_str(),
                            "rest_pattern" | "list_splat_pattern"
                        ),
                    })
                })
                .collect::<Result<Vec<_>, EvidenceError>>()
        })
        .transpose()?
        .unwrap_or_default();
    let source_bases: Vec<_> = match (doc.key.language, node.kind.as_str()) {
        (Language::Javascript, "class_declaration" | "class") => children(node.id)
            .filter(|n| n.kind == "class_heritage")
            .flat_map(|heritage| children(heritage.id))
            .filter(|base| base.candidate_kind.is_some())
            .collect(),
        (Language::Python, "class_definition") => field(node.id, "superclasses")
            .into_iter()
            .flat_map(|arguments| children(arguments.id))
            .filter(|base| base.candidate_kind.is_some())
            .collect(),
        _ => Vec::new(),
    };
    if doc.key.language == Language::Javascript {
        let captured: Vec<_> = doc
            .heritage
            .iter()
            .filter(|h| h.class_node_id == node.id)
            .collect();
        if captured.len() != source_bases.len()
            || captured.iter().zip(&source_bases).any(|(h, base)| {
                h.base_start != base.start_byte
                    || h.base_end != base.end_byte
                    || h.base_bytes != base.source_bytes
            })
        {
            return Err(native_error("JavaScript class heritage differs from AST"));
        }
    }
    let bases = source_bases
        .into_iter()
        .map(&text)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((
        key,
        Header {
            kind,
            name,
            modifiers,
            type_parameters,
            parameters,
            result_type: field(node.id, "return_type").map(&text).transpose()?,
            bases,
        },
    ))
}

fn enclosing_declaration(doc: &CapturedDocument, node_id: usize) -> Option<usize> {
    let mut child = node_id;
    let mut parent = doc.syntax[node_id].parent_id;
    while let Some(id) = parent {
        let node = &doc.syntax[id];
        let python_header = doc.key.language == Language::Python
            && matches!(
                node.kind.as_str(),
                "class_definition" | "function_definition" | "lambda"
            )
            && doc.syntax[child].field_name.as_deref() != Some("body");
        if !python_header
            && doc.native_candidates.iter().any(|w| {
                w.node_id == id
                    && w.candidate_kind == NativeCandidateKind::Declaration
                    && w.stable_id.is_some()
            })
        {
            return Some(id);
        }
        child = id;
        parent = node.parent_id;
    }
    None
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
    let mut parent = enclosing_declaration(doc, witness.node_id);
    while let Some(node_id) = parent {
        let candidate = doc
            .native_candidates
            .iter()
            .find(|w| {
                w.node_id == node_id
                    && w.candidate_kind == NativeCandidateKind::Declaration
                    && w.stable_id.is_some()
            })
            .ok_or_else(|| native_error("enclosing declaration witness missing"))?;
        let row = file
            .declarations
            .iter()
            .find(|row| candidate.stable_id.as_deref() == Some(row.syntax_id.as_str()))
            .ok_or_else(|| native_error("actual enclosing declaration is absent"))?;
        parents.push(row.syntax_id.as_str().to_owned());
        parent = enclosing_declaration(doc, node_id);
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
        for node in &doc.syntax {
            let kind = match doc.key.language {
                Language::Java => match node.kind.as_str() {
                    "class_declaration"
                    | "interface_declaration"
                    | "enum_declaration"
                    | "record_declaration"
                    | "annotation_type_declaration"
                    | "method_declaration"
                    | "annotation_type_element_declaration"
                    | "constructor_declaration"
                    | "compact_constructor_declaration"
                    | "lambda_expression" => Some(NativeCandidateKind::Declaration),
                    "method_invocation"
                    | "object_creation_expression"
                    | "explicit_constructor_invocation" => Some(NativeCandidateKind::Invocation),
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
                    | "ternary_expression" => Some(NativeCandidateKind::ControlRegion),
                    _ => None,
                },
                Language::Javascript => match node.kind.as_str() {
                    "class_declaration"
                    | "method_definition"
                    | "function_declaration"
                    | "generator_function_declaration"
                    | "function_expression"
                    | "generator_function"
                    | "arrow_function" => Some(NativeCandidateKind::Declaration),
                    "class" if node.candidate_kind.is_some() => {
                        Some(NativeCandidateKind::Declaration)
                    }
                    "call_expression" | "new_expression" => Some(NativeCandidateKind::Invocation),
                    "if_statement"
                    | "for_statement"
                    | "for_in_statement"
                    | "while_statement"
                    | "do_statement"
                    | "switch_statement"
                    | "switch_case"
                    | "switch_default"
                    | "try_statement"
                    | "catch_clause"
                    | "finally_clause"
                    | "ternary_expression"
                    | "conditional_expression"
                    | "statement_block"
                    | "expression_statement"
                    | "binary_expression"
                    | "class_static_block" => Some(NativeCandidateKind::ControlRegion),
                    _ => None,
                },
                Language::Python => match node.kind.as_str() {
                    "class_definition" | "function_definition" | "lambda" => {
                        Some(NativeCandidateKind::Declaration)
                    }
                    "call" => Some(NativeCandidateKind::Invocation),
                    "if_statement"
                    | "elif_clause"
                    | "else_clause"
                    | "for_statement"
                    | "while_statement"
                    | "try_statement"
                    | "except_clause"
                    | "finally_clause"
                    | "with_statement"
                    | "match_statement"
                    | "boolean_operator"
                    | "conditional_expression"
                    | "list_comprehension"
                    | "set_comprehension"
                    | "dictionary_comprehension"
                    | "generator_expression" => Some(NativeCandidateKind::ControlRegion),
                    _ => None,
                },
                Language::Rust => match node.kind.as_str() {
                    "function_item" | "closure_expression" | "impl_item" | "trait_item"
                    | "struct_item" | "enum_item" | "union_item" | "mod_item" => {
                        Some(NativeCandidateKind::Declaration)
                    }
                    "call_expression" | "method_call_expression" => {
                        Some(NativeCandidateKind::Invocation)
                    }
                    "if_expression" | "match_arm" | "loop_expression" | "while_expression"
                    | "for_expression" | "async_block" | "unsafe_block" => {
                        Some(NativeCandidateKind::ControlRegion)
                    }
                    _ => None,
                },
            };
            if let Some(kind) = kind {
                let matches: Vec<_> = witnesses
                    .iter()
                    .filter(|w| {
                        w.node_id == node.id && w.candidate_kind == kind && w.stable_id.is_some()
                    })
                    .collect();
                if matches.len() != 1 {
                    return Err(native_error(
                        "supported AST candidate lacks exactly one native witness",
                    ));
                }
            }
        }
        for witness in witnesses.iter().filter(|w| w.stable_id.is_some()) {
            let count = match witness.candidate_kind {
                NativeCandidateKind::Declaration => file
                    .declarations
                    .iter()
                    .filter(|r| Some(r.syntax_id.as_str()) == witness.stable_id.as_deref())
                    .count(),
                NativeCandidateKind::Invocation => file
                    .calls
                    .iter()
                    .filter(|r| Some(r.id.as_str()) == witness.stable_id.as_deref())
                    .count(),
                NativeCandidateKind::ControlRegion => file
                    .control_regions
                    .iter()
                    .filter(|r| Some(r.id.as_str()) == witness.stable_id.as_deref())
                    .count(),
                NativeCandidateKind::Occurrence => 0,
            };
            if count != 1 {
                return Err(native_error(
                    "selected complete document omits a captured supported candidate",
                ));
            }
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
                || row.name.as_ref().is_some_and(|name| {
                    !(doc.key.language == Language::Rust && witness.node_kind == "impl_item")
                        && name.as_str().as_bytes() != witness.name_bytes
                })
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
            if true {
                let (measured_key, measured_header) = if doc.key.language == Language::Java {
                    java_header(doc, witness)?
                } else {
                    non_java_header(doc, witness)?
                };
                let node = &doc.syntax[witness.node_id];
                let parent_declaration = |candidate: &CapturedNativeWitness| {
                    enclosing_declaration(doc, candidate.node_id)
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
                let mut measured_ancestors =
                    if matches!(doc.key.language, Language::Python | Language::Rust) {
                        vec![]
                    } else {
                        vec![module]
                    };
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
                        let (key, _) = if doc.key.language == Language::Java {
                            java_header(doc, candidate)
                        } else {
                            non_java_header(doc, candidate)
                        }
                        .ok()?;
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
                    return Err(native_error(
                        "declaration key or complete header differs from AST",
                    ));
                }
            }
            if row.kind == Kind::Type {
                if doc.key.language == Language::Java {
                    let class = &doc.syntax[witness.node_id];
                    let superclass = doc.syntax.iter().find(|n| {
                        n.parent_id == Some(class.id)
                            && n.field_name.as_deref() == Some("superclass")
                    });
                    let expected = superclass.and_then(|n| {
                        doc.syntax.iter().find(|child| {
                            child.parent_id == Some(n.id) && child.candidate_kind.is_some()
                        })
                    });
                    let actual: Vec<_> = doc
                        .heritage
                        .iter()
                        .filter(|h| h.class_node_id == class.id)
                        .collect();
                    match expected {
                        Some(base)
                            if actual.len() == 1
                                && actual[0].base_start == base.start_byte
                                && actual[0].base_end == base.end_byte
                                && actual[0].base_bytes == base.source_bytes => {}
                        None if actual.is_empty() => {}
                        _ => {
                            return Err(native_error(
                                "Java superclass heritage differs from AST field",
                            ));
                        }
                    }
                }
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
                let expected_bases = if doc.key.language == Language::Java {
                    measured_bases
                        .into_iter()
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                } else {
                    non_java_header(doc, witness)?
                        .1
                        .bases
                        .iter()
                        .map(|base| base.as_str().to_owned())
                        .collect()
                };
                if row
                    .header
                    .bases
                    .iter()
                    .map(Text::as_str)
                    .collect::<Vec<_>>()
                    != expected_bases
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                {
                    return Err(native_error(
                        "declaration bases differ from measured syntax",
                    ));
                }
            }
            if let Some(owner) = witness.ancestor_ids.last()
                && row.ancestors.last().map_or(
                    !module_owner(
                        doc,
                        &SyntaxId::new(owner.clone())
                            .ok_or_else(|| native_error("invalid module owner"))?,
                    ),
                    |key| {
                        crate::semantic_identity::syntax_id(
                            &doc.key.source_set_id,
                            &doc.key.path,
                            doc.key.language,
                            &row.ancestors[..row.ancestors.len() - 1],
                            key,
                        )
                        .map_or(true, |id| id.as_str() != owner)
                    },
                )
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

fn basis_error(message: &'static str) -> EvidenceError {
    EvidenceError::Basis(message)
}

/// Validates the captured semantic artifact before comparing it with a requested snapshot.
/// The comparison cannot repair a malformed captured basis.
pub fn validate_semantic_basis(
    capture: &CapturedRevision,
    evidence: &Evidence,
    requested_snapshot: &CapturedRevision,
) -> Result<(), EvidenceError> {
    validate_capture(capture, evidence)?;
    let native_ids: BTreeSet<_> = evidence
        .native_files
        .iter()
        .map(|f| f.provenance.id.as_str())
        .collect();
    let mut ids = native_ids;
    for provenance in &evidence.provenance {
        if !ids.insert(provenance.id.as_str()) {
            return Err(basis_error("duplicate provenance identity"));
        }
        let producer = evidence
            .producers
            .iter()
            .find(|p| p.id == provenance.producer_id && p.kind == ProducerKind::Semantic)
            .ok_or_else(|| basis_error("semantic provenance lacks producer"))?;
        let captured_producer = captured_producer(capture, producer)?;
        let basis = provenance
            .basis
            .as_ref()
            .ok_or_else(|| basis_error("semantic provenance lacks basis"))?;
        let document = capture
            .documents
            .iter()
            .find(|d| d.key == provenance.document)
            .ok_or_else(|| basis_error("semantic provenance lacks captured document"))?;
        let coverage = evidence
            .coverage
            .iter()
            .find(|c| {
                c.producer_id == producer.id
                    && c.language == document.key.language
                    && c.document_path == document.key.path
            })
            .ok_or_else(|| basis_error("semantic provenance lacks coverage"))?;
        let artifact = captured_producer
            .artifact_bytes
            .as_deref()
            .ok_or_else(|| basis_error("semantic producer lacks captured artifact"))?;
        if !matches!(
            coverage.state,
            CoverageState::Complete | CoverageState::Partial
        ) || provenance.revision_id != evidence.context.revision.id
            || provenance.content_hash.as_str() != digest(&document.bytes)
            || !producer.languages.contains(&document.key.language)
            || provenance.evidence_kind == EvidenceKind::MeasuredSyntax
            || basis.producer_id != producer.id
            || basis.producer_version != producer.version
            || basis.producer_hash != producer.executable_hash
            || basis.producer_hash.as_str() != digest(&captured_producer.executable_bytes)
            || basis.artifact_hash.as_str() != digest(artifact)
            || captured_producer.artifact_hash.as_deref() != Some(basis.artifact_hash.as_str())
            || basis.language != document.key.language
            || basis.source_set_id != evidence.context.source_set.id
            || basis.revision_id != evidence.context.revision.id
            || basis.source_manifest_hash.as_str() != digest(&capture.manifest_bytes)
            || basis.toolchain_hash.as_str() != digest(&capture.toolchain_bytes)
            || basis.config_hash.as_str() != digest(&capture.config_bytes)
            || basis.dependency_hash.as_str() != digest(&capture.dependency_bytes)
            || !ordered_unique_or_empty(&basis.lookup_dependencies)
            || capture
                .lookup_dependencies
                .iter()
                .find(|(key, _)| key == &document.key)
                .is_none_or(|(_, keys)| {
                    !keys
                        .iter()
                        .map(String::as_str)
                        .eq(basis.lookup_dependencies.iter().map(Text::as_str))
                })
        {
            return Err(basis_error(
                "semantic basis or provenance differs from captured bytes",
            ));
        }
        let requested_document = requested_snapshot
            .documents
            .iter()
            .find(|d| d.key == document.key);
        let freshness = if requested_document.is_none_or(|d| d.bytes != document.bytes) {
            Freshness::Stale
        } else if requested_snapshot.revision_id != capture.revision_id
            || requested_snapshot.source_set_id != capture.source_set_id
            || requested_snapshot.manifest_bytes != capture.manifest_bytes
            || requested_snapshot.toolchain_bytes != capture.toolchain_bytes
            || requested_snapshot.config_bytes != capture.config_bytes
            || requested_snapshot.dependency_bytes != capture.dependency_bytes
            || !requested_snapshot.producers.iter().any(|p| {
                p.id == captured_producer.id
                    && p.version == captured_producer.version
                    && p.executable_bytes == captured_producer.executable_bytes
                    && p.position_encoding == captured_producer.position_encoding
                    && p.tool_name == captured_producer.tool_name
            })
        {
            Freshness::PossiblyStale
        } else {
            Freshness::Fresh
        };
        if provenance.freshness != freshness {
            return Err(basis_error(
                "semantic freshness differs from captured comparison",
            ));
        }
    }
    Ok(())
}

fn semantic_offset(
    bytes: &[u8],
    line: u64,
    column: u64,
    encoding: PositionEncoding,
) -> Option<usize> {
    let source = std::str::from_utf8(bytes).ok()?;
    let start = source
        .split_inclusive('\n')
        .take(usize::try_from(line).ok()?)
        .map(str::len)
        .sum::<usize>();
    let text = source.get(start..)?.split('\n').next()?;
    let mut units = 0_u64;
    for (offset, ch) in text.char_indices() {
        if units == column {
            return Some(start + offset);
        }
        units += match encoding {
            PositionEncoding::Utf8 => ch.len_utf8() as u64,
            PositionEncoding::Utf16 => ch.len_utf16() as u64,
            PositionEncoding::UnicodeScalar => 1,
        };
        if units > column {
            return None;
        }
    }
    (units == column).then_some(start + text.len())
}
fn semantic_span(
    bytes: &[u8],
    coordinate: &[u64],
    encoding: PositionEncoding,
) -> Option<(usize, usize)> {
    let (start_line, start_column, end_line, end_column) = match coordinate {
        [line, start, end] => (*line, *start, *line, *end),
        [line, start, end_line, end] => (*line, *start, *end_line, *end),
        _ => return None,
    };
    let start = semantic_offset(bytes, start_line, start_column, encoding)?;
    let end = semantic_offset(bytes, end_line, end_column, encoding)?;
    (start < end).then_some((start, end))
}

fn semantic_proves_declaration(
    capture: &CapturedRevision,
    evidence: &Evidence,
    proof: &Provenance,
    key: &SymbolKey,
    syntax_id: &SyntaxId,
    document: &DocumentKey,
) -> bool {
    let Some(declaration) = evidence
        .native_files
        .iter()
        .find(|f| f.document.key == *document)
        .and_then(|f| f.declarations.iter().find(|d| d.syntax_id == *syntax_id))
    else {
        return false;
    };
    let Some(name_range) = declaration.name_range.as_ref() else {
        return false;
    };
    let Some(doc) = capture.documents.iter().find(|d| d.key == *document) else {
        return false;
    };
    let Some(encoding) = evidence
        .producers
        .iter()
        .find(|p| p.id == proof.producer_id)
        .map(|p| p.position_encoding)
    else {
        return false;
    };
    doc.semantic_positions.iter().any(|position| {
        position.producer_id == proof.producer_id.as_str()
            && position.symbol == key.symbol.as_str()
            && position.roles & 1 != 0
            && semantic_span(&doc.bytes, &position.coordinates, encoding)
                == Some((
                    name_range.start.get() as usize,
                    name_range.end.get() as usize,
                ))
    })
}

fn symbol_in_artifact(
    capture: &CapturedRevision,
    provenance: &Provenance,
    key: &SymbolKey,
) -> Result<(), EvidenceError> {
    let producer = capture
        .producers
        .iter()
        .find(|p| p.id == provenance.producer_id.as_str())
        .ok_or_else(|| basis_error("unknown semantic producer"))?;
    let bytes = producer
        .artifact_bytes
        .as_deref()
        .ok_or_else(|| basis_error("missing semantic artifact"))?;
    let index = scip::types::Index::parse_from_bytes(bytes)
        .map_err(|_| basis_error("invalid semantic artifact"))?;
    if (key.scope == SymbolScope::Document) != key.document.is_some()
        || key
            .document
            .as_ref()
            .is_some_and(|d| *d != provenance.document)
        || !index.documents.iter().any(|d| {
            d.relative_path == provenance.document.path.as_str()
                && (d.symbols.iter().any(|s| s.symbol == key.symbol.as_str())
                    || d.occurrences
                        .iter()
                        .any(|o| o.symbol == key.symbol.as_str()))
        })
    {
        return Err(basis_error(
            "symbol not asserted in captured producer artifact",
        ));
    }
    Ok(())
}

/// Source and captured-producer checks for facts. Exact cross-family joins remain
/// the responsibility of the next validation phase.
pub fn validate_semantic_facts(
    capture: &CapturedRevision,
    evidence: &Evidence,
) -> Result<(), EvidenceError> {
    let provenance = |id: &Text| {
        evidence
            .provenance
            .iter()
            .find(|p| p.id == *id)
            .ok_or_else(|| basis_error("fact has no semantic provenance"))
    };
    let mut symbols = BTreeSet::new();
    for symbol in &evidence.symbols {
        let proof = provenance(&symbol.provenance_id)?;
        if !symbols.insert((
            proof.producer_id.as_str(),
            symbol.key.symbol.as_str(),
            symbol.key.document.as_ref().map(|d| d.path.as_str()),
        )) {
            return Err(basis_error("duplicate scoped producer symbol"));
        }
        symbol_in_artifact(capture, proof, &symbol.key)?;
        let mut targets = BTreeSet::new();
        for target in &symbol.declarations {
            let canonical = crate::semantic_identity::canonical_json(target)
                .map_err(|_| basis_error("invalid symbol target"))?;
            if !targets.insert(canonical) {
                return Err(basis_error("duplicate symbol target"));
            }
            match target {
                Target::Internal {
                    syntax_id,
                    document,
                    revision_id,
                } => {
                    if revision_id != &evidence.context.revision.id
                        || document.source_set_id != evidence.context.source_set.id
                        || !semantic_proves_declaration(
                            capture,
                            evidence,
                            proof,
                            &symbol.key,
                            syntax_id,
                            document,
                        )
                    {
                        return Err(basis_error(
                            "internal symbol target lacks captured declaration",
                        ));
                    }
                }
                Target::External { symbol: external } => {
                    symbol_in_artifact(capture, proof, external)?
                }
            }
        }
    }
    let mut reference_ids = BTreeSet::new();
    for reference in &evidence.references {
        if !reference_ids.insert(reference.id.as_str())
            || evidence
                .references
                .iter()
                .filter(|prior| {
                    prior.owner_syntax_id == reference.owner_syntax_id
                        && prior.document == reference.document
                        && (prior.range.start.get(), prior.range.end.get())
                            < (reference.range.start.get(), reference.range.end.get())
                })
                .count() as u64
                != reference.ordinal.get()
        {
            return Err(basis_error(
                "reference occurrence ordinal or identity duplicated",
            ));
        }
        let proof = provenance(&reference.provenance_id)?;
        if proof.evidence_kind != EvidenceKind::SemanticReference
            || reference.document != proof.document
            || reference.revision_id != proof.revision_id
            || reference.revision_id != evidence.context.revision.id
            || !ordered_unique_or_empty(
                &reference
                    .roles
                    .iter()
                    .map(|r| role_order(*r))
                    .collect::<Vec<_>>(),
            )
            || reference.roles.is_empty()
            || reference.document.language == Language::Java
                && reference.roles.contains(&Role::Alias)
            || (reference.site == ReferenceSite::Declaration)
                != reference.roles.contains(&Role::Definition)
            || reference.roles.contains(&Role::Alias)
                && (reference.site != ReferenceSite::Declaration
                    || !reference.roles.contains(&Role::Definition))
            || crate::semantic_identity::lookup_key(
                reference.document.language,
                reference.spelling.as_str(),
            )
            .ok()
            .as_deref()
                != Some(reference.lookup_key.as_str())
        {
            return Err(basis_error(
                "reference role, spelling or provenance mismatch",
            ));
        }
        match (
            &reference.resolution,
            &reference.declared_target,
            reference.candidates.as_slice(),
        ) {
            (
                Resolution::Resolved,
                Some(Target::Internal {
                    syntax_id,
                    document,
                    revision_id,
                }),
                [],
            ) if *revision_id == reference.revision_id
                && *document == reference.document
                && evidence.native_files.iter().any(|f| {
                    f.document.key == *document
                        && f.declarations.iter().any(|d| d.syntax_id == *syntax_id)
                }) => {}
            (Resolution::External, Some(Target::External { symbol }), [])
                if symbol_in_artifact(capture, proof, symbol).is_ok() => {}
            (Resolution::Unresolved, None, []) => {}
            (Resolution::Ambiguous, None, candidates)
                if candidates.len() >= 2
                    && candidates.windows(2).all(|pair| {
                        crate::semantic_identity::canonical_json(&pair[0]).ok()
                            < crate::semantic_identity::canonical_json(&pair[1]).ok()
                    })
                    && candidates.iter().all(|target| match target {
                        Target::Internal {
                            syntax_id,
                            document,
                            revision_id,
                        } => {
                            revision_id == &reference.revision_id
                                && document.source_set_id == reference.document.source_set_id
                                && evidence.native_files.iter().any(|file| {
                                    file.document.key == *document
                                        && file
                                            .declarations
                                            .iter()
                                            .any(|d| d.syntax_id == *syntax_id)
                                })
                                && evidence.symbols.iter().any(|symbol| {
                                    evidence.provenance.iter().any(|symbol_proof| {
                                        symbol_proof.id == symbol.provenance_id
                                            && symbol_proof.producer_id == proof.producer_id
                                    }) && symbol.declarations.contains(target)
                                        && capture.documents.iter().any(|doc| {
                                            doc.key == reference.document
                                                && doc.semantic_positions.iter().any(|position| {
                                                    position.producer_id
                                                        == proof.producer_id.as_str()
                                                        && position.symbol
                                                            == symbol.key.symbol.as_str()
                                                        && semantic_span(
                                                            &doc.bytes,
                                                            &position.coordinates,
                                                            evidence
                                                                .producers
                                                                .iter()
                                                                .find(|p| p.id == proof.producer_id)
                                                                .unwrap()
                                                                .position_encoding,
                                                        ) == Some((
                                                            reference.range.start.get() as usize,
                                                            reference.range.end.get() as usize,
                                                        ))
                                                })
                                        })
                                })
                        }
                        Target::External { symbol } => {
                            symbol_in_artifact(capture, proof, symbol).is_ok()
                                && capture.documents.iter().any(|doc| {
                                    doc.key == reference.document
                                        && doc.semantic_positions.iter().any(|position| {
                                            position.producer_id == proof.producer_id.as_str()
                                                && position.symbol == symbol.symbol.as_str()
                                                && semantic_span(
                                                    &doc.bytes,
                                                    &position.coordinates,
                                                    evidence
                                                        .producers
                                                        .iter()
                                                        .find(|p| p.id == proof.producer_id)
                                                        .unwrap()
                                                        .position_encoding,
                                                ) == Some((
                                                    reference.range.start.get() as usize,
                                                    reference.range.end.get() as usize,
                                                ))
                                        })
                                })
                        }
                    }) => {}
            _ => {
                return Err(basis_error(
                    "reference target cardinality or assertion invalid",
                ));
            }
        }
        if crate::semantic_identity::occurrence_id(
            &reference.revision_id,
            &reference.owner_syntax_id,
            crate::semantic_identity::OccurrenceKind::Reference,
            reference.ordinal,
        )
        .ok()
            != Some(reference.id.clone())
        {
            return Err(basis_error("reference occurrence identity mismatch"));
        }
        let document = capture
            .documents
            .iter()
            .find(|d| d.key == reference.document)
            .ok_or_else(|| basis_error("reference document not captured"))?;
        let bytes = document
            .bytes
            .get(reference.range.start.get() as usize..reference.range.end.get() as usize)
            .ok_or_else(|| basis_error("reference source span invalid"))?;
        if bytes != reference.spelling.as_str().as_bytes()
            || !document.semantic_positions.iter().any(|position| {
                position.producer_id == proof.producer_id.as_str()
                    && match reference.declared_target.as_ref() {
                        Some(Target::External { symbol }) => {
                            position.symbol == symbol.symbol.as_str()
                        }
                        Some(target @ Target::Internal { .. }) => {
                            evidence.symbols.iter().any(|symbol| {
                                symbol.key.symbol.as_str() == position.symbol
                                    && symbol.declarations.contains(target)
                            })
                        }
                        None => true,
                    }
                    && (position.roles & 1 != 0) == reference.roles.contains(&Role::Definition)
                    && (!reference.roles.contains(&Role::Import) || position.roles & 2 != 0)
                    && (!reference.roles.contains(&Role::Write) || position.roles & 4 != 0)
                    && (!reference.roles.contains(&Role::Read) || position.roles & 8 != 0)
                    && (!reference.roles.contains(&Role::Call)
                        || evidence.native_files.iter().any(|f| {
                            f.document.key == reference.document
                                && f.calls.iter().any(|call| {
                                    call.callee_range.as_ref() == Some(&reference.range)
                                })
                        }))
                    && (!reference.roles.contains(&Role::Type)
                        || document.native_candidates.iter().any(|w| {
                            w.candidate_kind == NativeCandidateKind::Declaration
                                && w.token_start_byte == reference.range.start.get() as usize
                                && w.token_end_byte == reference.range.end.get() as usize
                        }))
                    && (!reference.roles.contains(&Role::Alias)
                        || document.syntax.iter().any(|node| {
                            node.start_byte == reference.range.start.get() as usize
                                && node.end_byte == reference.range.end.get() as usize
                                && node.parent_id.is_some_and(|id| {
                                    document.syntax.iter().any(|parent| {
                                        parent.id == id
                                            && match reference.document.language {
                                                Language::Python => {
                                                    parent.kind == "aliased_import"
                                                        && node.field_name.as_deref()
                                                            == Some("alias")
                                                }
                                                Language::Javascript => {
                                                    parent.kind == "import_specifier"
                                                        && node.field_name.as_deref()
                                                            == Some("alias")
                                                }
                                                Language::Rust => {
                                                    parent.kind == "use_as_clause"
                                                        && node.field_name.as_deref()
                                                            == Some("alias")
                                                }
                                                Language::Java => false,
                                            }
                                    })
                                })
                        }))
                    && position.revision_id == proof.revision_id.as_str()
                    && position.artifact_hash
                        == proof
                            .basis
                            .as_ref()
                            .map(|b| b.artifact_hash.as_str())
                            .unwrap_or("")
                    && semantic_span(
                        &document.bytes,
                        &position.coordinates,
                        evidence
                            .producers
                            .iter()
                            .find(|p| p.id == proof.producer_id)
                            .unwrap()
                            .position_encoding,
                    ) == Some((
                        reference.range.start.get() as usize,
                        reference.range.end.get() as usize,
                    ))
            })
            || !evidence.native_files.iter().any(|f| {
                f.document.key == reference.document
                    && (f
                        .declarations
                        .iter()
                        .any(|d| d.syntax_id == reference.owner_syntax_id)
                        || module_owner(document, &reference.owner_syntax_id))
            })
        {
            return Err(basis_error(
                "reference lacks exact source and producer position",
            ));
        }
    }
    for relationship in &evidence.type_relationships {
        let proof = provenance(&relationship.provenance_id)?;
        if proof.evidence_kind != EvidenceKind::TypeRelationship {
            return Err(basis_error("relationship provenance has wrong family"));
        }
        let Target::Internal {
            syntax_id,
            document,
            revision_id,
        } = &relationship.source
        else {
            return Err(basis_error(
                "relationship source is not an internal declaration",
            ));
        };
        let source = evidence
            .native_files
            .iter()
            .find(|f| f.document.key == *document)
            .and_then(|f| f.declarations.iter().find(|d| d.syntax_id == *syntax_id))
            .ok_or_else(|| basis_error("relationship source declaration not measured"))?;
        if revision_id != &proof.revision_id || *document != proof.document {
            return Err(basis_error("relationship source differs from provenance"));
        }
        let source_symbol = evidence
            .symbols
            .iter()
            .find(|s| {
                evidence
                    .provenance
                    .iter()
                    .any(|p| p.id == s.provenance_id && p.producer_id == proof.producer_id)
                    && s.declarations.contains(&relationship.source)
            })
            .ok_or_else(|| basis_error("relationship source has no producer-bound symbol"))?;
        let (target_symbol, target_name) = match &relationship.target {
            Target::External { symbol } => {
                symbol_in_artifact(capture, proof, symbol)?;
                (symbol.symbol.as_str(), None)
            }
            Target::Internal {
                syntax_id,
                document,
                revision_id,
            } => {
                if revision_id != &proof.revision_id {
                    return Err(basis_error("relationship target revision differs"));
                }
                let target = evidence
                    .native_files
                    .iter()
                    .find(|f| f.document.key == *document)
                    .and_then(|f| f.declarations.iter().find(|d| d.syntax_id == *syntax_id))
                    .ok_or_else(|| basis_error("relationship target declaration not measured"))?;
                let symbol =
                    evidence
                        .symbols
                        .iter()
                        .find(|s| {
                            evidence.provenance.iter().any(|p| {
                                p.id == s.provenance_id && p.producer_id == proof.producer_id
                            }) && s.declarations.contains(&relationship.target)
                        })
                        .ok_or_else(|| {
                            basis_error("relationship target has no producer-bound symbol")
                        })?;
                (
                    symbol.key.symbol.as_str(),
                    target.name.as_ref().map(Text::as_str),
                )
            }
        };
        let doc = capture
            .documents
            .iter()
            .find(|d| d.key == *document)
            .ok_or_else(|| basis_error("relationship source bytes not captured"))?;
        let Some(producer) = capture
            .producers
            .iter()
            .find(|p| p.id == proof.producer_id.as_str())
        else {
            return Err(basis_error("relationship producer not captured"));
        };
        let index = scip::types::Index::parse_from_bytes(
            producer
                .artifact_bytes
                .as_deref()
                .ok_or_else(|| basis_error("relationship artifact missing"))?,
        )
        .map_err(|_| basis_error("relationship artifact malformed"))?;
        if !index
            .documents
            .iter()
            .filter(|d| d.relative_path == document.path.as_str())
            .flat_map(|d| &d.symbols)
            .any(|s| {
                s.symbol == source_symbol.key.symbol.as_str()
                    && s.relationships
                        .iter()
                        .any(|r| r.symbol == target_symbol && r.is_implementation)
            })
        {
            return Err(basis_error(
                "producer has no directed relationship assertion",
            ));
        }
        let measured_header = doc
            .native_candidates
            .iter()
            .find(|w| w.stable_id.as_deref() == Some(syntax_id.as_str()))
            .ok_or_else(|| basis_error("relationship source header missing"))?;
        let header = std::str::from_utf8(&measured_header.header_bytes)
            .map_err(|_| basis_error("relationship header invalid"))?;
        if relationship.kind == RelationshipKind::Overrides {
            let target_owner = target_symbol
                .split('#')
                .next()
                .and_then(|prefix| prefix.split_whitespace().last())
                .map(|owner| owner.replace(['/', '$'], "."));
            let containing_type = evidence
                .native_files
                .iter()
                .find(|file| file.document.key == *document)
                .and_then(|file| {
                    file.declarations
                        .iter()
                        .filter(|owner| {
                            owner.kind == Kind::Type
                                && owner.range.start <= source.range.start
                                && source.range.end <= owner.range.end
                        })
                        .min_by_key(|owner| owner.range.end.get() - owner.range.start.get())
                });
            if source.kind != Kind::Method
                || !containing_type.is_some_and(|owner| {
                    target_owner.as_ref().is_some_and(|target_owner| {
                        owner
                            .header
                            .bases
                            .iter()
                            .any(|base| base.as_str().replace(['/', '$'], ".") == *target_owner)
                    })
                })
                || target_name
                    .is_some_and(|name| source.name.as_ref().map(Text::as_str) != Some(name))
            {
                return Err(basis_error(
                    "override has no measured heritage and directed semantic proof",
                ));
            }
            continue;
        }
        let bases = &source.header.bases;
        if bases.is_empty()
            || source.kind != Kind::Type
            || !doc.semantic_positions.iter().any(|position| {
                position.producer_id == proof.producer_id.as_str()
                    && position.symbol == target_symbol
                    && semantic_span(
                        &doc.bytes,
                        &position.coordinates,
                        evidence
                            .producers
                            .iter()
                            .find(|p| p.id == proof.producer_id)
                            .unwrap()
                            .position_encoding,
                    )
                    .is_some_and(|(start, end)| {
                        source.range.start.get() <= start as u64
                            && end
                                <= measured_header.start_byte + measured_header.header_bytes.len()
                            && std::str::from_utf8(&doc.bytes[start..end])
                                .ok()
                                .is_some_and(|name| {
                                    bases.iter().any(|base| base.as_str() == name)
                                        && target_name.is_none_or(|expected| expected == name)
                                })
                    })
            })
        {
            return Err(basis_error(
                "relationship has no source heritage and directed target proof",
            ));
        }
        let keyword = match relationship.kind {
            RelationshipKind::Extends => "extends",
            RelationshipKind::Implements => "implements",
            RelationshipKind::Overrides => unreachable!(),
        };
        if !(doc.key.language == Language::Python
            && relationship.kind == RelationshipKind::Extends
            && header.contains('(')
            && header.contains(')'))
            && !header.contains(keyword)
            && !(doc.key.language == Language::Rust
                && relationship.kind == RelationshipKind::Implements
                && header.contains(" for "))
        {
            return Err(basis_error(
                "relationship kind differs from source heritage",
            ));
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
    if evidence.native_files.is_empty() {
        return Err(EvidenceError::NotYetValidated(
            "native syntax and later phases not supplied",
        ));
    }
    validate_native(capture, evidence)?;
    validate_semantic_basis(capture, evidence, requested_snapshot)?;
    validate_semantic_facts(capture, evidence)?;
    if !evidence.native_files.is_empty()
        || !evidence.provenance.is_empty()
        || !evidence.symbols.is_empty()
        || !evidence.declaration_bindings.is_empty()
        || !evidence.type_relationships.is_empty()
        || !evidence.references.is_empty()
        || !evidence.call_bindings.is_empty()
    {
        return Err(EvidenceError::NotYetValidated(
            "measured joins and bindings require later validator phase",
        ));
    }
    Err(EvidenceError::NotYetValidated(
        "native syntax, provenance, basis and semantic facts require later validator phases",
    ))
}
