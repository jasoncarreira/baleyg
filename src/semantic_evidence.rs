//! Admission of immutable captured bytes before syntax and semantic validation.
use crate::{
    indexer::{CapturedProducer, CapturedRevision},
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
