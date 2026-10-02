//! Closed native-only in-memory evidence, derived from the admitted immutable capture.
use crate::{
    capture::Capture,
    model::{CancelFlag, SourceFile},
    native_ids::{IdentityRegistry, canonical, digest, extraction_context},
};
use anyhow::{Context, Result, ensure};
use icu_normalizer::ComposingNormalizerBorrowed;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
use tree_sitter::Node;

fn required_nullable<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

const LANGUAGES: [&str; 4] = ["java", "rust", "python", "javascript"];
const PRODUCER: &str = "baleyg.native.syntax";
/// Native producer descriptor version. Any change that can alter a native measured field or
/// projection, or that starts reading another input, must change it (Decision 0003).
pub(crate) const NATIVE_VERSION: &str = "native-v4";
/// The native producer's declared extraction-input inventory (Decision 0003), keyed by its
/// descriptor `(id, version)` and language. `extract` measures one document from only its own
/// bytes, language and path; the tree-sitter grammars and Unicode normalization tables it uses
/// are compiled into the executable and fixed by `NATIVE_VERSION`. It reads no configuration,
/// toolchain or dependency capture, and no other source document, so every language declares
/// the explicit empty inventory. An unlisted descriptor or language has no declaration.
type LanguageInventory = (&'static str, &'static [&'static str]);
const EXTRACTION_INPUTS: &[(&str, &str, &[LanguageInventory])] = &[(
    PRODUCER,
    NATIVE_VERSION,
    &[
        ("java", &[]),
        ("rust", &[]),
        ("python", &[]),
        ("javascript", &[]),
    ],
)];
/// Authenticate the declared inventory of `producer` for `language` against the captured
/// revision and derive the document's extraction context. This is the fail-closed
/// precondition for minting any `occ:v2` ID: an undeclared descriptor/language or a declared
/// component without a valid captured revision digest refuses, and nothing is minted.
fn native_extraction_context(
    producer: &Producer,
    language: &str,
    revision: &Revision,
) -> Result<String> {
    let inventory = EXTRACTION_INPUTS
        .iter()
        .find(|(id, version, _)| *id == producer.id && *version == producer.version)
        .and_then(|(_, _, languages)| languages.iter().find(|(l, _)| *l == language))
        .map(|(_, inventory)| *inventory)
        .context("undeclared native extraction-input inventory")?;
    // The v4 parser reads no external component: all grammars and normalization tables are
    // fixed in this executable. Other captured inputs affect revision admission, not syntax.
    authenticated_extraction_context(language, inventory, &[], revision)
}
fn authenticated_extraction_context(
    language: &str,
    inventory: &[&str],
    observed: &[(&str, Option<&[u8]>)],
    revision: &Revision,
) -> Result<String> {
    let mut declared = BTreeSet::new();
    for name in inventory {
        ensure!(
            matches!(*name, "toolchain" | "config" | "dependency") && declared.insert(*name),
            "unsupported or duplicate native extraction component: {name}"
        );
    }
    let mut captured = BTreeMap::new();
    for (name, bytes) in observed {
        ensure!(
            declared.contains(name) && captured.insert(*name, *bytes).is_none(),
            "undeclared or duplicate captured native extraction component: {name}"
        );
    }
    let mut components = Vec::with_capacity(inventory.len());
    for name in inventory {
        let bytes = captured
            .get(name)
            .context("absent native extraction component capture")?
            .context("absent native extraction component bytes")?;
        let expected = match *name {
            "toolchain" => &revision.toolchain_hash,
            "config" => &revision.config_hash,
            "dependency" => &revision.dependency_hash,
            _ => unreachable!(),
        };
        ensure!(
            valid_hash(expected) && hash(bytes) == *expected,
            "mismatched native extraction component: {name}"
        );
        components.push(((*name).to_owned(), expected.clone()));
    }
    components.sort();
    extraction_context(language, &components)
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn text(s: &str) -> Result<()> {
    ensure!(!s.is_empty(), "empty text");
    Ok(())
}
fn safe_path(s: &str) -> Result<()> {
    text(s)?;
    ensure!(
        !s.starts_with('/')
            && !s.contains(['\\', '\0'])
            && s.split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."),
        "invalid native path: {s}"
    );
    Ok(())
}
fn valid_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn language_order(s: &str) -> Result<usize> {
    LANGUAGES
        .iter()
        .position(|l| *l == s)
        .context("unsupported native language")
}
fn span(text: &str, r: &Range, empty: bool) -> Result<()> {
    ensure!(
        r.start <= r.end
            && r.end <= text.len()
            && (empty || r.start < r.end)
            && text.is_char_boundary(r.start)
            && text.is_char_boundary(r.end),
        "invalid native byte range"
    );
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Producer {
    pub id: String,
    pub version: String,
    pub executable_hash: String,
    pub kind: String,
    pub languages: Vec<String>,
    pub position_encoding: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSet {
    pub id: String,
    pub root_id: String,
    pub languages: Vec<String>,
    pub dependencies: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentKey {
    pub source_set_id: String,
    pub language: String,
    pub path: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Document {
    pub key: DocumentKey,
    pub revision_id: String,
    pub content_hash: String,
    pub byte_length: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Revision {
    pub id: String,
    pub source_set_id: String,
    pub documents: Vec<Document>,
    pub toolchain_hash: String,
    pub config_hash: String,
    pub dependency_hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Coverage {
    pub producer_id: String,
    pub language: String,
    pub source_set_id: String,
    pub document_path: String,
    pub revision_id: String,
    pub requested: bool,
    pub selected: bool,
    pub state: String,
    pub supported_roles: Vec<String>,
    pub observed_roles: Vec<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub diagnostic: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Provenance {
    pub id: String,
    pub producer_id: String,
    pub document: DocumentKey,
    pub revision_id: String,
    pub content_hash: String,
    pub evidence_kind: String,
    #[serde(deserialize_with = "required_nullable")]
    pub basis: Option<Value>,
    pub freshness: String,
    #[serde(deserialize_with = "required_nullable")]
    pub derived_from: Option<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub start: usize,
    pub end: usize,
}
impl Range {
    fn node(n: Node<'_>) -> Self {
        Self {
            start: n.start_byte(),
            end: n.end_byte(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Signature {
    #[serde(rename = "parameterTypes")]
    pub parameter_types: Vec<String>,
    #[serde(rename = "typeParameterCount")]
    pub type_parameter_count: usize,
    pub variadic: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Key {
    pub kind: String,
    #[serde(deserialize_with = "required_nullable")]
    pub name: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub signature: Option<Signature>,
    pub ordinal: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Parameter {
    #[serde(deserialize_with = "required_nullable")]
    pub name: Option<String>,
    #[serde(rename = "type")]
    #[serde(deserialize_with = "required_nullable")]
    pub type_name: Option<String>,
    pub variadic: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Header {
    pub kind: String,
    #[serde(deserialize_with = "required_nullable")]
    pub name: Option<String>,
    pub modifiers: Vec<String>,
    pub type_parameters: Vec<String>,
    pub parameters: Vec<Parameter>,
    #[serde(deserialize_with = "required_nullable")]
    pub result_type: Option<String>,
    pub bases: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Declaration {
    pub syntax_id: String,
    pub document: DocumentKey,
    pub revision_id: String,
    pub kind: String,
    #[serde(deserialize_with = "required_nullable")]
    pub name: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub lookup_key: Option<String>,
    pub ancestors: Vec<Key>,
    pub key: Key,
    pub range: Range,
    #[serde(deserialize_with = "required_nullable")]
    pub name_range: Option<Range>,
    pub header: Header,
    pub provenance_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Call {
    pub id: String,
    pub owner_syntax_id: String,
    pub ordinal: usize,
    pub document: DocumentKey,
    pub revision_id: String,
    pub range: Range,
    #[serde(deserialize_with = "required_nullable")]
    pub callee_range: Option<Range>,
    #[serde(deserialize_with = "required_nullable")]
    pub spelling: Option<String>,
    pub region_ids: Vec<String>,
    pub provenance_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlRegion {
    pub id: String,
    pub owner_syntax_id: String,
    pub ordinal: usize,
    pub document: DocumentKey,
    pub revision_id: String,
    pub kind: String,
    pub range: Range,
    #[serde(deserialize_with = "required_nullable")]
    pub parent_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub arm: Option<String>,
    pub provenance_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Artifact {
    pub producer: Producer,
    pub source_set: SourceSet,
    pub revision: Revision,
    pub coverage: Vec<Coverage>,
    pub provenance: Vec<Provenance>,
    pub declarations: Vec<Declaration>,
    pub calls: Vec<Call>,
    pub control_regions: Vec<ControlRegion>,
}

pub fn lookup(language: &str, name: &str) -> Result<String> {
    text(name)?;
    Ok(match language {
        "rust" => ComposingNormalizerBorrowed::new_nfc()
            .normalize(name)
            .into_owned(),
        "python" => ComposingNormalizerBorrowed::new_nfkc()
            .normalize(name)
            .into_owned(),
        "java" | "javascript" => name.to_owned(),
        _ => anyhow::bail!("unsupported native language"),
    })
}

// Each absent input is explicit, never indistinguishable from a present empty file.
fn entry(root: &Path, path: &Path, bytes: Option<&[u8]>) -> Result<Value> {
    let rel = path
        .strip_prefix(root)?
        .to_str()
        .context("non-UTF8 native input path")?
        .replace('\\', "/");
    safe_path(&rel)?;
    Ok(match bytes {
        Some(bytes) => {
            json!({"path":rel,"type":"present","bytes":hex::encode(bytes),"byteLength":bytes.len()})
        }
        None => json!({"path":rel,"type":"absent"}),
    })
}

pub fn from_capture(
    capture: &Capture,
    root: &Path,
    root_id: &str,
    cancel: &CancelFlag,
) -> Result<Artifact> {
    let artifact = build_native(capture, root, root_id)?;
    artifact.validate(capture, root, root_id, cancel)?;
    Ok(artifact)
}

/// The full captured revision header is shared by full and selected measurement.
/// Selecting one file must never change the source-set, producer or revision ID.
fn build_native_header<'a>(
    capture: &'a Capture,
    root: &Path,
    root_id: &str,
) -> Result<(Artifact, Vec<&'a SourceFile>)> {
    text(root_id)?;
    let identity = crate::store::topology::WorkspaceIdentity::discover(Some(root), root)?;
    ensure!(
        identity.record_id == root_id,
        "native source-set root identity mismatch"
    );
    identity.verify()?;
    let source_set_id = format!("source-set:v1:{root_id}");
    let exe = std::env::current_exe()?;
    let executable_hash = capture
        .executable_digest(&exe)
        .context("native executable not admitted or replaced")?
        .to_owned();
    let producer = Producer {
        id: PRODUCER.into(),
        version: NATIVE_VERSION.into(),
        executable_hash: executable_hash.clone(),
        kind: "native".into(),
        languages: LANGUAGES.map(str::to_owned).into(),
        position_encoding: "utf8".into(),
    };
    let source_set = SourceSet {
        id: source_set_id.clone(),
        root_id: root_id.into(),
        languages: producer.languages.clone(),
        dependencies: vec![],
    };
    let root_inputs: Vec<_> = crate::capture::ROOT_INPUTS[..22]
        .iter()
        .map(|name| root.join(name))
        .collect();
    let dependencies: Vec<Value> = root_inputs
        .iter()
        .map(|p| entry(root, p, capture.bytes(p)))
        .collect::<Result<_>>()?;
    let mut config = dependencies.clone();
    let selectors: Vec<_> = crate::capture::ROOT_INPUTS[22..]
        .iter()
        .map(|n| root.join(n))
        .collect();
    for p in &selectors {
        config.push(entry(root, p, capture.bytes(p))?);
    }
    config.push(
        json!({"nativeAdmission":{"maxFileBytes":capture.reconcile_options().max_file_bytes}}),
    );
    for (p, b) in capture.admitted_inputs() {
        if p.file_name()
            .is_some_and(|n| n == ".gitignore" || n == ".ignore")
            && p.starts_with(root)
        {
            config.push(entry(root, p, b)?);
        }
    }
    let selectors: Vec<Value> = selectors
        .iter()
        .map(|p| entry(root, p, capture.bytes(p)))
        .collect::<Result<_>>()?;
    let toolchain = json!({"nativeExecutable":executable_hash,"rootRustToolchain":selectors[0],"rootRustToolchainToml":selectors[1]});
    let mut files: Vec<_> = capture.files.iter().collect();
    files.sort_by(|a, b| {
        language_order(&a.language)
            .unwrap()
            .cmp(&language_order(&b.language).unwrap())
            .then(a.path.as_bytes().cmp(b.path.as_bytes()))
    });
    let docs: Vec<Value> = files.iter().map(|f| json!({"language":f.language,"path":f.path,"bytes":hex::encode(f.text.as_bytes()),"byteLength":f.text.len()})).collect();
    let revision_input = json!({"sourceSetId":source_set_id,"documents":docs,"toolchain":toolchain,"config":config,"dependencies":dependencies});
    let id = format!(
        "revision:v1:{}",
        digest(b"baleyg.native-revision.v1\0", &canonical(&revision_input))
    );
    let documents = files
        .iter()
        .map(|f| Document {
            key: DocumentKey {
                source_set_id: source_set_id.clone(),
                language: f.language.clone(),
                path: f.path.clone(),
            },
            revision_id: id.clone(),
            content_hash: f.hash.clone(),
            byte_length: f.text.len(),
        })
        .collect();
    let revision = Revision {
        id,
        source_set_id,
        documents,
        toolchain_hash: hash(&canonical(&toolchain)),
        config_hash: hash(&canonical(&json!(config))),
        dependency_hash: hash(&canonical(&json!(dependencies))),
    };
    let artifact = Artifact {
        producer,
        source_set,
        revision,
        coverage: vec![],
        provenance: vec![],
        declarations: vec![],
        calls: vec![],
        control_regions: vec![],
    };
    Ok((artifact, files))
}

fn build_native(capture: &Capture, root: &Path, root_id: &str) -> Result<Artifact> {
    let (mut artifact, files) = build_native_header(capture, root, root_id)?;
    let mut ids = IdentityRegistry::default();
    for f in files {
        extract(&mut artifact, f, &mut ids)?;
    }
    Ok(artifact)
}

/// Native facts for exactly one document of the full admitted revision. This is not
/// an Artifact and cannot be passed to the v8 publisher or full-bundle validator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedDocument {
    pub producer: Producer,
    pub source_set: SourceSet,
    pub revision: Revision,
    pub document: Document,
    pub coverage: Coverage,
    pub provenance: Provenance,
    pub declarations: Vec<Declaration>,
    pub calls: Vec<Call>,
    pub control_regions: Vec<ControlRegion>,
}

/// Extract only the selected admitted source; all header fields retain the full
/// captured snapshot identity. `on_extract` attests successful real native assembly,
/// not a classifier candidate or a full-bundle parse.
pub(crate) fn measure_captured_document(
    capture: &Capture,
    root: &Path,
    root_id: &str,
    path: &str,
    cancel: &CancelFlag,
    mut on_extract: impl FnMut(&DocumentKey),
) -> Result<SelectedDocument> {
    capture.verify(cancel)?;
    let (mut artifact, files) = build_native_header(capture, root, root_id)?;
    let selected: Vec<_> = files.into_iter().filter(|file| file.path == path).collect();
    ensure!(selected.len() == 1, "selected native source must be unique and admitted");
    let file = selected[0];
    ensure!(capture.hashes.get(path) == Some(&file.hash)
        && file.hash == hash(file.text.as_bytes()),
        "selected native source bytes/hash mismatch");
    let documents: Vec<_> = artifact.revision.documents.iter()
        .filter(|document| document.key.path == path).cloned().collect();
    ensure!(documents.len() == 1, "selected native revision document must be unique");
    let document = documents[0].clone();
    ensure!(document.key.source_set_id == artifact.source_set.id
        && document.key.language == file.language
        && document.content_hash == file.hash
        && document.byte_length == file.text.len()
        && document.revision_id == artifact.revision.id,
        "selected native document not bound to full capture");
    let mut ids = IdentityRegistry::default();
    extract(&mut artifact, file, &mut ids)?;
    ensure!(artifact.coverage.len() == 1 && artifact.provenance.len() == 1
        && artifact.coverage[0].document_path == path
        && artifact.provenance[0].document == document.key
        && artifact.declarations.iter().all(|row| row.document == document.key)
        && artifact.calls.iter().all(|row| row.document == document.key)
        && artifact.control_regions.iter().all(|row| row.document == document.key),
        "selected native facts escaped the admitted document");
    capture.verify(cancel)?;
    on_extract(&document.key);
    Ok(SelectedDocument {
        producer: artifact.producer,
        source_set: artifact.source_set,
        revision: artifact.revision,
        document,
        coverage: artifact.coverage.remove(0),
        provenance: artifact.provenance.remove(0),
        declarations: artifact.declarations,
        calls: artifact.calls,
        control_regions: artifact.control_regions,
    })
}

fn kind(lang: &str, n: Node<'_>) -> Option<&'static str> {
    match lang {
        "java" => crate::indexer_java::native_kind(n),
        "rust" => crate::indexer_rust::native_kind(n),
        "python" => crate::indexer_python::native_kind(n),
        "javascript" => crate::indexer::native_js_kind(n),
        _ => None,
    }
}
fn is_call(lang: &str, n: Node<'_>) -> bool {
    matches!(
        (lang, n.kind()),
        (
            "java",
            "method_invocation" | "object_creation_expression" | "explicit_constructor_invocation"
        ) | ("rust", "call_expression" | "method_call_expression")
            | ("python", "call")
            | ("javascript", "call_expression" | "new_expression")
    )
}
fn is_region(n: Node<'_>) -> bool {
    matches!(
        n.kind(),
        "if_statement"
            | "if_expression"
            | "else_clause"
            | "elif_clause"
            | "while_statement"
            | "do_statement"
            | "for_statement"
            | "for_in_statement"
            | "for_expression"
            | "loop_expression"
            | "match_expression"
            | "match_statement"
            | "switch_statement"
            | "switch_expression"
            | "switch_case"
            | "switch_default"
            | "try_statement"
            | "catch_clause"
            | "finally_clause"
            | "conditional_expression"
            | "ternary_expression"
    )
}
fn child_nodes(n: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = n.walk();
    n.named_children(&mut cursor).collect()
}
fn name(n: Node<'_>, f: &SourceFile) -> Option<(String, Range)> {
    let node = n
        .child_by_field_name("name")
        .or_else(|| {
            (n.kind() == "assignment")
                .then(|| n.child_by_field_name("left"))
                .flatten()
        })
        .or_else(|| {
            (n.kind() == "field_definition")
                .then(|| n.child_by_field_name("property"))
                .flatten()
        })
        .or_else(|| {
            (n.kind() == "parameter" && n.named_child_count() > 0)
                .then(|| n.named_child(0))
                .flatten()
        })
        .or_else(|| {
            matches!(n.kind(), "formal_parameter" | "typed_parameter")
                .then(|| n.named_child(0))
                .flatten()
        })
        .or_else(|| (n.kind() == "identifier").then_some(n))?;
    if !matches!(
        node.kind(),
        "identifier" | "property_identifier" | "field_identifier" | "type_identifier"
    ) {
        return None;
    }
    let value = f.text.get(node.byte_range())?;
    if value.is_empty() {
        return None;
    }
    Some((value.to_owned(), Range::node(node)))
}
fn java_signature(n: Node<'_>, f: &SourceFile) -> Signature {
    let parameters = n
        .child_by_field_name("parameters")
        .map(child_nodes)
        .unwrap_or_default();
    let mut types = vec![];
    let mut variadic = false;
    for parameter in parameters {
        if !parameter.kind().contains("parameter") {
            continue;
        }
        if let Some(t) = parameter
            .child_by_field_name("type")
            .and_then(|part| f.text.get(part.byte_range()))
        {
            types.push(t.to_owned());
        }
        if parameter.kind().contains("spread") {
            variadic = true;
        }
    }
    let type_parameter_count = n
        .child_by_field_name("type_parameters")
        .map(child_nodes)
        .unwrap_or_default()
        .len();
    Signature {
        parameter_types: types,
        type_parameter_count,
        variadic,
    }
}
fn key_of(n: Node<'_>, f: &SourceFile, k: &str) -> Key {
    Key {
        kind: k.into(),
        name: name(n, f).map(|v| v.0),
        signature: if f.language == "java" && (k == "method" || k == "constructor") {
            Some(java_signature(n, f))
        } else {
            None
        },
        ordinal: 0,
    }
}
fn modifier_keyword(value: &str) -> bool {
    matches!(
        value,
        "public"
            | "protected"
            | "private"
            | "static"
            | "abstract"
            | "final"
            | "native"
            | "synchronized"
            | "volatile"
            | "transient"
            | "strictfp"
            | "default"
            | "async"
            | "const"
            | "unsafe"
            | "extern"
            | "override"
            | "get"
            | "set"
            | "readonly"
    )
}

fn header(n: Node<'_>, f: &SourceFile, key: &Key) -> Header {
    let from = |node: Node<'_>| f.text.get(node.byte_range()).map(str::to_owned);
    let field = |field: &str| n.child_by_field_name(field).and_then(from);
    let mut modifier_tokens: Vec<(usize, usize, String)> = Vec::new();
    let mut cursor = n.walk();
    for child in n.children(&mut cursor) {
        match child.kind() {
            "visibility_modifier" => {
                if let Some(value) = from(child) {
                    modifier_tokens.push((child.start_byte(), child.end_byte(), value));
                }
            }
            "modifiers" | "function_modifiers" => {
                let mut nested_cursor = child.walk();
                for token in child.children(&mut nested_cursor) {
                    if let Some(value) = from(token)
                        && (token.is_named() || modifier_keyword(&value))
                    {
                        modifier_tokens.push((token.start_byte(), token.end_byte(), value));
                    }
                }
            }
            _ if !child.is_named() => {
                if let Some(value) = from(child)
                    && modifier_keyword(&value)
                {
                    modifier_tokens.push((child.start_byte(), child.end_byte(), value));
                }
            }
            _ => {}
        }
    }
    modifier_tokens.sort_by_key(|(start, end, _)| (*start, *end));
    let modifiers = modifier_tokens
        .into_iter()
        .map(|(_, _, value)| value)
        .collect();
    let type_parameters = n
        .child_by_field_name("type_parameters")
        .map(child_nodes)
        .unwrap_or_default()
        .into_iter()
        .filter_map(from)
        .collect();
    let parameters = n
        .child_by_field_name("parameters")
        .map(child_nodes)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| {
            p.kind().contains("parameter")
                || matches!(p.kind(), "identifier" | "rest_pattern" | "self_parameter")
        })
        .map(|p| {
            let pname = p
                .child_by_field_name("name")
                .or_else(|| {
                    child_nodes(p)
                        .into_iter()
                        .find(|c| c.kind() == "identifier")
                })
                .and_then(from)
                .or_else(|| (p.kind() == "identifier").then(|| from(p)).flatten());
            let type_name = p.child_by_field_name("type").and_then(from);
            Parameter {
                name: pname,
                type_name,
                variadic: p.kind().contains("spread")
                    || p.kind().contains("rest")
                    || f.text
                        .get(p.byte_range())
                        .is_some_and(|s| s.starts_with('*') || s.starts_with("...")),
            }
        })
        .collect();
    let result_type = field("return_type").or_else(|| {
        if f.language == "java" && matches!(key.kind.as_str(), "method") {
            field("type")
        } else {
            None
        }
    });
    let bases = ["superclass", "interfaces", "super_interfaces", "traits"]
        .into_iter()
        .flat_map(|field| n.child_by_field_name(field))
        .filter_map(from)
        .collect();
    Header {
        kind: key.kind.clone(),
        name: key.name.clone(),
        modifiers,
        type_parameters,
        parameters,
        result_type,
        bases,
    }
}

fn callee(n: Node<'_>, f: &SourceFile) -> (Option<String>, Option<Range>) {
    let expr = n
        .child_by_field_name(match f.language.as_str() {
            "java" => "name",
            "python" => "function",
            "javascript" if n.kind() == "new_expression" => "constructor",
            "javascript" => "function",
            _ => "function",
        })
        .or_else(|| n.child_by_field_name("constructor"));
    let Some(expr) = expr else {
        return (None, None);
    };
    let member = match f.language.as_str() {
        "java" => Some(expr),
        "python" if expr.kind() == "attribute" => expr.child_by_field_name("attribute"),
        "javascript" if expr.kind() == "member_expression" => expr.child_by_field_name("property"),
        "rust" if expr.kind() == "field_expression" => expr.child_by_field_name("field"),
        _ => Some(expr),
    };
    if let Some(member) = member
        && matches!(
            member.kind(),
            "identifier" | "property_identifier" | "field_identifier" | "type_identifier"
        )
        && let Some(token) = f.text.get(member.byte_range())
        && !token.is_empty()
    {
        return (Some(token.into()), Some(Range::node(member)));
    }
    (
        f.text
            .get(expr.byte_range())
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        None,
    )
}
/// Reparse only a selected stored source snapshot. This does not admit or reread
/// workspace files and does not project a second whole-workspace graph.
pub(crate) fn selected_source_witness(
    file: &SourceFile,
    producer: Producer,
    source_set: SourceSet,
    revision: Revision,
) -> Result<Artifact> {
    ensure!(
        revision.source_set_id == source_set.id
            && revision.documents.len() == 1
            && revision.documents[0].key.path == file.path
            && revision.documents[0].content_hash == file.hash,
        "selected native source header mismatch"
    );
    // The revision carries only this selected document here. Its ID and hashes
    // are read from the pinned persisted header, never invented or republished.
    let mut artifact = Artifact {
        producer,
        source_set,
        revision,
        coverage: vec![],
        provenance: vec![],
        declarations: vec![],
        calls: vec![],
        control_regions: vec![],
    };
    extract(&mut artifact, file, &mut IdentityRegistry::default())?;
    Ok(artifact)
}

/// Measure exactly one document. Under `occ:v2` this is strictly document-local: it reads only
/// `f` and the producer/source-set/revision descriptors, never another source document.
fn extract(a: &mut Artifact, f: &SourceFile, ids: &mut IdentityRegistry) -> Result<()> {
    language_order(&f.language)?;
    safe_path(&f.path)?;
    // Occurrence identity binds this captured document version. Authenticate its content hash
    // and extraction context against the captured revision before measuring, so a failed
    // precondition mints no occ:v2 record at all.
    ensure!(
        f.hash == hash(f.text.as_bytes()),
        "native captured source hash mismatch"
    );
    ensure!(
        a.revision
            .documents
            .iter()
            .any(|d| d.key.source_set_id == a.source_set.id
                && d.key.language == f.language
                && d.key.path == f.path
                && d.content_hash == f.hash
                && d.byte_length == f.text.len()),
        "native document not in captured revision"
    );
    let extraction_context = native_extraction_context(&a.producer, &f.language, &a.revision)?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&match f.language.as_str() {
        "java" => tree_sitter_java::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        _ => tree_sitter_javascript::LANGUAGE.into(),
    })?;
    let tree = parser
        .parse(&f.text, None)
        .context("native parser failed")?;
    let key = DocumentKey {
        source_set_id: a.source_set.id.clone(),
        language: f.language.clone(),
        path: f.path.clone(),
    };
    let proof_id = format!(
        "native-proof:v1:{}",
        digest(
            b"baleyg.native-proof.v1\0",
            &canonical(&json!({"producerId":PRODUCER,"document":key,"revisionId":a.revision.id}))
        )
    );
    a.provenance.push(Provenance {
        id: proof_id.clone(),
        producer_id: PRODUCER.into(),
        document: key.clone(),
        revision_id: a.revision.id.clone(),
        content_hash: f.hash.clone(),
        evidence_kind: "measuredSyntax".into(),
        basis: None,
        freshness: "fresh".into(),
        derived_from: None,
    });
    let module_key = Key {
        kind: "module".into(),
        name: None,
        signature: None,
        ordinal: 0,
    };
    let module_id=ids.stable(&json!({"sourceSet":a.source_set.id,"path":f.path,"language":f.language,"ancestors":[],"declaration":module_key}))?;
    a.declarations.push(Declaration {
        syntax_id: module_id.clone(),
        document: key.clone(),
        revision_id: a.revision.id.clone(),
        kind: "module".into(),
        name: None,
        lookup_key: None,
        ancestors: vec![],
        key: module_key,
        range: Range::node(tree.root_node()),
        name_range: None,
        header: Header {
            kind: "module".into(),
            name: None,
            modifiers: vec![],
            type_parameters: vec![],
            parameters: vec![],
            result_type: None,
            bases: vec![],
        },
        provenance_id: proof_id.clone(),
    });
    let mut state = ParserState {
        artifact: a,
        file: f,
        ids,
        document: key,
        proof_id,
        extraction_context,
        seen: BTreeSet::new(),
        visits: 0,
        opaque_rust: false,
    };
    state.walk(tree.root_node(), &module_id, &[], &[], 0)?;
    state.finish_occurrences()?;
    let observed_roles = [
        (
            !state
                .artifact
                .declarations
                .iter()
                .all(|d| d.document != state.document || d.kind == "module"),
            "definition",
        ),
        (
            !state
                .artifact
                .calls
                .iter()
                .all(|c| c.document != state.document),
            "call",
        ),
    ]
    .into_iter()
    .filter_map(|(present, role)| present.then_some(role.into()))
    .collect();
    state.artifact.coverage.push(Coverage {
        producer_id: PRODUCER.into(),
        language: f.language.clone(),
        source_set_id: state.artifact.source_set.id.clone(),
        document_path: f.path.clone(),
        revision_id: state.artifact.revision.id.clone(),
        requested: true,
        selected: true,
        state: if tree.root_node().has_error() || state.opaque_rust {
            "partial"
        } else {
            "complete"
        }
        .into(),
        supported_roles: vec!["definition".into(), "call".into()],
        observed_roles,
        diagnostic: match (tree.root_node().has_error(), state.opaque_rust) {
            (true, true) => Some("parser recovered from invalid source; opaque Rust macro/attribute token trees skipped".into()),
            (true, false) => Some("parser recovered from invalid source".into()),
            (false, true) => Some("opaque Rust macro/attribute token trees skipped; calls and definitions inside were not measured".into()),
            (false, false) => None,
        },
    });
    Ok(())
}
/// Decision 0003 `occ:v2` input: document version, extraction context and native producer
/// descriptor, never the containing revision.
fn occurrence_input(
    content_hash: &str,
    extraction_context: &str,
    producer: &Producer,
    owner: &str,
    kind: &str,
    ordinal: usize,
) -> Value {
    json!({"contentHash":content_hash,"extractionContext":extraction_context,"nativeProducerId":producer.id,"nativeProducerVersion":producer.version,"ownerSyntaxId":owner,"kind":kind,"ordinal":ordinal})
}
struct ParserState<'a> {
    artifact: &'a mut Artifact,
    file: &'a SourceFile,
    ids: &'a mut IdentityRegistry,
    document: DocumentKey,
    proof_id: String,
    extraction_context: String,
    seen: BTreeSet<(String, String, usize, usize)>,
    visits: usize,
    opaque_rust: bool,
}
impl ParserState<'_> {
    fn occurrence_input(&self, owner: &str, kind: &str, ordinal: usize) -> Value {
        occurrence_input(
            &self.file.hash,
            &self.extraction_context,
            &self.artifact.producer,
            owner,
            kind,
            ordinal,
        )
    }
    fn finish_occurrences(&mut self) -> Result<()> {
        let mut replacements = BTreeMap::new();
        let mut by_owner: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, region) in self.artifact.control_regions.iter().enumerate() {
            if region.document == self.document {
                by_owner
                    .entry(region.owner_syntax_id.clone())
                    .or_default()
                    .push(i);
            }
        }
        for indexes in by_owner.values_mut() {
            indexes.sort_by_key(|i| {
                let r = &self.artifact.control_regions[*i].range;
                (r.start, r.end)
            });
            for (ordinal, i) in indexes.iter().enumerate() {
                let input = self.occurrence_input(
                    &self.artifact.control_regions[*i].owner_syntax_id,
                    "control",
                    ordinal,
                );
                let id = self.ids.occurrence(&input)?;
                let region = &mut self.artifact.control_regions[*i];
                replacements.insert(std::mem::replace(&mut region.id, id.clone()), id);
                region.ordinal = ordinal;
            }
        }
        for region in &mut self.artifact.control_regions {
            if region.document == self.document
                && let Some(parent) = &mut region.parent_id
            {
                *parent = replacements
                    .get(parent)
                    .context("native region parent not measured")?
                    .clone();
            }
        }
        let mut by_owner: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, call) in self.artifact.calls.iter().enumerate() {
            if call.document == self.document {
                by_owner
                    .entry(call.owner_syntax_id.clone())
                    .or_default()
                    .push(i);
            }
        }
        for indexes in by_owner.values_mut() {
            indexes.sort_by_key(|i| {
                let r = &self.artifact.calls[*i].range;
                (r.start, r.end)
            });
            for (ordinal, i) in indexes.iter().enumerate() {
                let input = self.occurrence_input(
                    &self.artifact.calls[*i].owner_syntax_id,
                    "call",
                    ordinal,
                );
                let id = self.ids.occurrence(&input)?;
                let call = &mut self.artifact.calls[*i];
                call.ordinal = ordinal;
                call.id = id;
                for region in &mut call.region_ids {
                    *region = replacements
                        .get(region)
                        .context("native call region not measured")?
                        .clone();
                }
            }
        }
        Ok(())
    }

    fn walk(
        &mut self,
        n: Node<'_>,
        owner: &str,
        ancestors: &[Key],
        regions: &[String],
        depth: usize,
    ) -> Result<()> {
        ensure!(
            depth < 128 && self.visits < 1_000_000,
            "native extraction work budget exceeded"
        );
        self.visits += 1;
        // Rust token trees and attributes are opaque source syntax. A call-shaped token
        // inside a macro is not a measured call, and expanding it is outside #22 native
        // extraction. Still count this subtree root against the bounded visit budget.
        if self.file.language == "rust"
            && matches!(
                n.kind(),
                "macro_invocation"
                    | "macro_definition"
                    | "token_tree"
                    | "attribute_item"
                    | "inner_attribute_item"
            )
        {
            self.opaque_rust = true;
            return Ok(());
        }
        let mut owner = owner.to_owned();
        let mut ancestry = ancestors.to_vec();
        let mut regions = regions.to_vec();
        if let Some(k) = kind(&self.file.language, n)
            && (matches!(k, "implementation" | "anonymousFunction") || name(n, self.file).is_some())
        {
            let mut key = key_of(n, self.file, k);
            let previous = self
                .artifact
                .declarations
                .iter()
                .filter(|d| {
                    d.document == self.document
                        && d.ancestors == ancestry
                        && d.key.kind == key.kind
                        && d.key.name == key.name
                        && d.key.signature == key.signature
                        && d.kind != "module"
                })
                .count();
            key.ordinal = previous;
            let id=self.ids.stable(&json!({"sourceSet":self.artifact.source_set.id,"path":self.file.path,"language":self.file.language,"ancestors":ancestry,"declaration":key}))?;
            let named = name(n, self.file);
            self.artifact.declarations.push(Declaration {
                syntax_id: id.clone(),
                document: self.document.clone(),
                revision_id: self.artifact.revision.id.clone(),
                kind: k.into(),
                name: named.as_ref().map(|v| v.0.clone()),
                lookup_key: named
                    .as_ref()
                    .map(|v| lookup(&self.file.language, &v.0))
                    .transpose()?,
                ancestors: ancestry.clone(),
                key: key.clone(),
                range: Range::node(n),
                name_range: named.map(|v| v.1),
                header: header(n, self.file, &key),
                provenance_id: self.proof_id.clone(),
            });
            // A lexical declaration key may nest syntax, but its initializer runs in
            // the enclosing executable scope. Only executable containers own calls.
            if matches!(
                k,
                "module"
                    | "namespace"
                    | "type"
                    | "implementation"
                    | "function"
                    | "method"
                    | "constructor"
                    | "anonymousFunction"
            ) {
                owner = id;
                regions.clear();
            }
            ancestry.push(key);
        }
        if is_region(n) {
            let ordinal = self
                .artifact
                .control_regions
                .iter()
                .filter(|r| r.owner_syntax_id == owner && r.document == self.document)
                .count();
            let range = Range::node(n);
            ensure!(
                self.seen
                    .insert((owner.clone(), "control".into(), range.start, range.end)),
                "duplicate native control range"
            );
            let id = format!(
                "pending:control:{}:{}:{}:{}",
                self.file.path, owner, range.start, range.end
            );
            self.artifact.control_regions.push(ControlRegion {
                id: id.clone(),
                owner_syntax_id: owner.clone(),
                ordinal,
                document: self.document.clone(),
                revision_id: self.artifact.revision.id.clone(),
                kind: n.kind().into(),
                range,
                parent_id: regions.last().cloned(),
                arm: None,
                provenance_id: self.proof_id.clone(),
            });
            regions.push(id);
        }
        if is_call(&self.file.language, n) {
            let ordinal = self
                .artifact
                .calls
                .iter()
                .filter(|c| c.owner_syntax_id == owner && c.document == self.document)
                .count();
            let range = Range::node(n);
            ensure!(
                self.seen
                    .insert((owner.clone(), "call".into(), range.start, range.end)),
                "duplicate native call range"
            );
            let id = String::new();
            let (spelling, callee_range) = callee(n, self.file);
            self.artifact.calls.push(Call {
                id,
                owner_syntax_id: owner.clone(),
                ordinal,
                document: self.document.clone(),
                revision_id: self.artifact.revision.id.clone(),
                range,
                callee_range,
                spelling,
                region_ids: regions.clone(),
                provenance_id: self.proof_id.clone(),
            });
        }
        for child in child_nodes(n) {
            self.walk(child, &owner, &ancestry, &regions, depth + 1)?;
        }
        Ok(())
    }
}

impl Artifact {
    /// Reconstruct the native syntax from already-admitted immutable buffers; no source file
    /// is opened or hashed again. Comparison forbids well-shaped fabricated evidence.
    /// Honors the caller's cancellation before and after the re-derivation.
    pub fn validate(
        &self,
        capture: &Capture,
        root: &Path,
        root_id: &str,
        cancel: &CancelFlag,
    ) -> Result<()> {
        capture.verify(cancel)?;
        self.validate_structure(&capture.files)?;
        let expected = build_native(capture, root, root_id)?;
        capture.verify(cancel)?;
        expected.validate_structure(&capture.files)?;
        ensure!(
            self == &expected,
            "native evidence differs from captured source or inputs"
        );
        Ok(())
    }
    fn validate_structure(&self, files: &[SourceFile]) -> Result<()> {
        ensure!(
            self.producer.id == PRODUCER
                && self.producer.kind == "native"
                && self.producer.version == NATIVE_VERSION
                && self.producer.position_encoding == "utf8"
                && valid_hash(&self.producer.executable_hash),
            "invalid native producer"
        );
        ensure!(
            self.producer.languages == LANGUAGES.map(str::to_owned)
                && self.source_set.languages == self.producer.languages
                && self.source_set.dependencies.is_empty()
                && self.source_set.id == format!("source-set:v1:{}", self.source_set.root_id),
            "invalid native source set"
        );
        text(&self.source_set.root_id)?;
        ensure!(
            self.revision.source_set_id == self.source_set.id
                && self.revision.id.starts_with("revision:v1:")
                && valid_hash(&self.revision.id[12..])
                && valid_hash(&self.revision.toolchain_hash)
                && valid_hash(&self.revision.config_hash)
                && valid_hash(&self.revision.dependency_hash),
            "invalid native revision"
        );
        ensure!(
            self.revision.documents.len() == files.len()
                && self.coverage.len() == files.len()
                && self.provenance.len() == files.len(),
            "native document tuple cardinality"
        );
        let mut docs = BTreeMap::new();
        let mut last: Option<(usize, &str)> = None;
        for d in &self.revision.documents {
            safe_path(&d.key.path)?;
            let current = (language_order(&d.key.language)?, d.key.path.as_str());
            ensure!(
                last.is_none_or(|prev| prev < current),
                "unsorted or duplicate native documents"
            );
            last = Some(current);
            let file = files
                .iter()
                .find(|f| f.path == d.key.path && f.language == d.key.language)
                .context("native document not captured")?;
            ensure!(
                d.key.source_set_id == self.source_set.id
                    && d.revision_id == self.revision.id
                    && d.byte_length == file.text.len()
                    && d.content_hash == file.hash
                    && valid_hash(&d.content_hash),
                "native document bytes/hash mismatch"
            );
            docs.insert(d.key.clone(), file);
        }
        let mut covers = BTreeSet::new();
        for c in &self.coverage {
            let key = DocumentKey {
                source_set_id: c.source_set_id.clone(),
                language: c.language.clone(),
                path: c.document_path.clone(),
            };
            ensure!(
                docs.contains_key(&key)
                    && covers.insert(key)
                    && c.producer_id == PRODUCER
                    && c.revision_id == self.revision.id
                    && c.requested
                    && c.selected
                    && matches!(c.state.as_str(), "complete" | "partial" | "failed")
                    && c.supported_roles == ["definition", "call"]
                    && c.observed_roles
                        .iter()
                        .all(|role| c.supported_roles.contains(role))
                    && c.observed_roles.windows(2).all(|window| c
                        .supported_roles
                        .iter()
                        .position(|r| r == &window[0])
                        < c.supported_roles.iter().position(|r| r == &window[1])),
                "invalid native coverage"
            );
            ensure!(
                (c.state == "complete") == c.diagnostic.is_none(),
                "native diagnostic mismatch"
            );
            if let Some(d) = &c.diagnostic {
                text(d)?;
            }
        }
        let mut proofs = BTreeSet::new();
        for proof in &self.provenance {
            let f = docs
                .get(&proof.document)
                .context("native proof dangling document")?;
            ensure!(
                proofs.insert(proof.id.clone())
                    && proof.producer_id == PRODUCER
                    && proof.revision_id == self.revision.id
                    && proof.content_hash == f.hash
                    && proof.evidence_kind == "measuredSyntax"
                    && proof.basis.is_none()
                    && proof.derived_from.is_none()
                    && proof.freshness == "fresh",
                "invalid native provenance"
            );
            let expected = format!(
                "native-proof:v1:{}",
                digest(
                    b"baleyg.native-proof.v1\0",
                    &canonical(
                        &json!({"producerId":PRODUCER,"document":proof.document,"revisionId":self.revision.id})
                    )
                )
            );
            ensure!(proof.id == expected, "native proof digest mismatch");
        }
        let mut declarations = BTreeMap::new();
        let mut identity = IdentityRegistry::default();
        for d in &self.declarations {
            let file = docs
                .get(&d.document)
                .context("native declaration dangling document")?;
            ensure!(
                d.revision_id == self.revision.id
                    && proofs.contains(&d.provenance_id)
                    && matches!(
                        d.kind.as_str(),
                        "module"
                            | "namespace"
                            | "type"
                            | "implementation"
                            | "function"
                            | "method"
                            | "constructor"
                            | "field"
                            | "variable"
                            | "parameter"
                            | "typeParameter"
                            | "alias"
                            | "anonymousFunction"
                    )
                    && d.kind == d.key.kind
                    && d.name == d.key.name
                    && d.header.kind == d.kind
                    && d.header.name == d.name,
                "native declaration mismatch"
            );
            ensure!(
                d.name.is_some() == d.lookup_key.is_some()
                    && d.name.is_some() == d.name_range.is_some(),
                "native name nullability"
            );
            span(
                &file.text,
                &d.range,
                d.kind == "module" && file.text.is_empty(),
            )?;
            if let Some(r) = &d.name_range {
                span(&file.text, r, false)?;
                ensure!(
                    r.start >= d.range.start
                        && r.end <= d.range.end
                        && d.name.as_deref() == file.text.get(r.start..r.end),
                    "native name not source-measured"
                );
            }
            if let Some(n) = &d.name {
                ensure!(
                    d.lookup_key.as_deref() == Some(lookup(&d.document.language, n)?.as_str()),
                    "native lookup normalization mismatch"
                );
            }
            if d.document.language != "java" || !matches!(d.kind.as_str(), "method" | "constructor")
            {
                ensure!(d.key.signature.is_none(), "unexpected native signature");
            }
            if d.kind == "module" {
                ensure!(
                    d.name.is_none() && d.ancestors.is_empty(),
                    "invalid native module"
                );
            }
            let expected=identity.stable(&json!({"sourceSet":self.source_set.id,"path":d.document.path,"language":d.document.language,"ancestors":d.ancestors,"declaration":d.key}))?;
            ensure!(
                d.syntax_id == expected && declarations.insert(d.syntax_id.clone(), d).is_none(),
                "native stable ID collision/duplicate"
            );
        }
        let mut siblings: BTreeMap<Vec<u8>, Vec<&Declaration>> = BTreeMap::new();
        for d in &self.declarations {
            let group = canonical(
                &json!({"document":d.document,"ancestors":d.ancestors,"kind":d.kind,"name":d.name,"signature":d.key.signature}),
            );
            siblings.entry(group).or_default().push(d);
        }
        for group in siblings.values_mut() {
            group.sort_by_key(|d| (d.range.start, d.range.end));
            for (ordinal, d) in group.iter().enumerate() {
                ensure!(d.key.ordinal == ordinal, "native sibling ordinal mismatch");
                if ordinal != 0 {
                    ensure!(
                        group[ordinal - 1].range != d.range,
                        "duplicate sibling range"
                    );
                }
            }
        }
        let mut call_ordinals: BTreeMap<(DocumentKey, String), Vec<&Call>> = BTreeMap::new();
        for c in &self.calls {
            call_ordinals
                .entry((c.document.clone(), c.owner_syntax_id.clone()))
                .or_default()
                .push(c);
        }
        for group in call_ordinals.values_mut() {
            group.sort_by_key(|c| (c.range.start, c.range.end));
            for (ordinal, c) in group.iter().enumerate() {
                ensure!(c.ordinal == ordinal, "native call ordinal mismatch");
                if ordinal != 0 {
                    ensure!(group[ordinal - 1].range != c.range, "duplicate call range");
                }
            }
        }
        let mut control_ordinals: BTreeMap<(DocumentKey, String), Vec<&ControlRegion>> =
            BTreeMap::new();
        for r in &self.control_regions {
            control_ordinals
                .entry((r.document.clone(), r.owner_syntax_id.clone()))
                .or_default()
                .push(r);
        }
        for group in control_ordinals.values_mut() {
            group.sort_by_key(|r| (r.range.start, r.range.end));
            for (ordinal, r) in group.iter().enumerate() {
                ensure!(r.ordinal == ordinal, "native control ordinal mismatch");
                if ordinal != 0 {
                    ensure!(
                        group[ordinal - 1].range != r.range,
                        "duplicate control range"
                    );
                }
            }
        }
        let mut occurrence = IdentityRegistry::default();
        let mut calls = BTreeSet::new();
        for c in &self.calls {
            let file = docs
                .get(&c.document)
                .context("native call dangling document")?;
            let owner = declarations
                .get(&c.owner_syntax_id)
                .context("native call dangling owner")?;
            ensure!(
                owner.document == c.document
                    && c.revision_id == self.revision.id
                    && proofs.contains(&c.provenance_id)
                    && calls.insert(c.id.clone()),
                "native call ownership/duplicate"
            );
            span(&file.text, &c.range, false)?;
            if let Some(r) = &c.callee_range {
                span(&file.text, r, false)?;
                ensure!(
                    r.start >= c.range.start
                        && r.end <= c.range.end
                        && c.spelling.as_deref() == file.text.get(r.start..r.end),
                    "callee not measured in call"
                );
            }
            if let Some(s) = &c.spelling {
                text(s)?;
            }
            let context =
                native_extraction_context(&self.producer, &c.document.language, &self.revision)?;
            ensure!(
                c.id == occurrence.occurrence(&occurrence_input(
                    &file.hash,
                    &context,
                    &self.producer,
                    &c.owner_syntax_id,
                    "call",
                    c.ordinal
                ))?,
                "native call occurrence mismatch"
            );
        }
        let mut regions = BTreeMap::new();
        for region in &self.control_regions {
            let file = docs
                .get(&region.document)
                .context("native control dangling document")?;
            let owner = declarations
                .get(&region.owner_syntax_id)
                .context("native control dangling owner")?;
            ensure!(
                owner.document == region.document
                    && region.revision_id == self.revision.id
                    && proofs.contains(&region.provenance_id),
                "native control ownership"
            );
            span(&file.text, &region.range, false)?;
            let context = native_extraction_context(
                &self.producer,
                &region.document.language,
                &self.revision,
            )?;
            ensure!(
                region.id
                    == occurrence.occurrence(&occurrence_input(
                        &file.hash,
                        &context,
                        &self.producer,
                        &region.owner_syntax_id,
                        "control",
                        region.ordinal
                    ))?
                    && regions.insert(region.id.clone(), region).is_none(),
                "native control occurrence mismatch"
            );
        }
        for region in &self.control_regions {
            if let Some(parent) = &region.parent_id {
                let parent = regions
                    .get(parent)
                    .context("native control dangling parent")?;
                ensure!(
                    parent.owner_syntax_id == region.owner_syntax_id
                        && parent.document == region.document
                        && parent.range.start <= region.range.start
                        && region.range.end <= parent.range.end
                        && parent.id != region.id,
                    "invalid native control parent"
                );
            }
        }
        for c in &self.calls {
            for id in &c.region_ids {
                let r = regions.get(id).context("native call dangling region")?;
                ensure!(
                    r.owner_syntax_id == c.owner_syntax_id
                        && r.document == c.document
                        && r.range.start <= c.range.start
                        && c.range.end <= r.range.end,
                    "native call region mismatch"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod occurrence_identity_tests {
    use super::*;

    fn producer(version: &str) -> Producer {
        Producer {
            id: PRODUCER.into(),
            version: version.into(),
            executable_hash: "e".repeat(64),
            kind: "native".into(),
            languages: LANGUAGES.map(str::to_owned).into(),
            position_encoding: "utf8".into(),
        }
    }
    fn selected(file: &SourceFile, revision_id: &str, version: &str) -> Result<Artifact> {
        let source_set = SourceSet {
            id: "source-set:v1:root".into(),
            root_id: "root".into(),
            languages: LANGUAGES.map(str::to_owned).into(),
            dependencies: vec![],
        };
        let key = DocumentKey {
            source_set_id: source_set.id.clone(),
            language: file.language.clone(),
            path: file.path.clone(),
        };
        let revision = Revision {
            id: revision_id.into(),
            source_set_id: source_set.id.clone(),
            documents: vec![Document {
                key,
                revision_id: revision_id.into(),
                content_hash: file.hash.clone(),
                byte_length: file.text.len(),
            }],
            toolchain_hash: "1".repeat(64),
            config_hash: hash(revision_id.as_bytes()),
            dependency_hash: "3".repeat(64),
        };
        selected_source_witness(file, producer(version), source_set, revision)
    }
    fn file(text: &str) -> SourceFile {
        SourceFile {
            path: "a.js".into(),
            hash: hash(text.as_bytes()),
            language: "javascript".into(),
            text: text.into(),
        }
    }

    #[test]
    fn native_producer_declares_and_authenticates_an_explicit_empty_inventory() {
        let revision = selected(&file("f();"), "revision:v1:r1", NATIVE_VERSION)
            .unwrap()
            .revision;
        for language in LANGUAGES {
            assert_eq!(
                native_extraction_context(&producer(NATIVE_VERSION), language, &revision).unwrap(),
                extraction_context(language, &[]).unwrap()
            );
        }
        // An undeclared producer version (e.g. the withdrawn occ:v1 producer) or language has
        // no authenticated inventory, so it fails closed.
        for version in ["native-v2", "native-v3"] {
            assert!(native_extraction_context(&producer(version), "rust", &revision).is_err());
        }
        assert!(native_extraction_context(&producer(NATIVE_VERSION), "go", &revision).is_err());
        let mut captured = revision.clone();
        captured.config_hash = hash(b"captured config");
        assert_eq!(
            authenticated_extraction_context(
                "rust",
                &["config"],
                &[("config", Some(b"captured config"))],
                &captured,
            )
            .unwrap(),
            extraction_context("rust", &[("config".into(), captured.config_hash.clone())]).unwrap()
        );
        for (inventory, observed) in [
            (vec!["config"], vec![]),
            (vec!["config"], vec![("config", None)]),
            (
                vec!["config"],
                vec![("config", Some(b"stale config".as_slice()))],
            ),
            (
                vec![],
                vec![("config", Some(b"captured config".as_slice()))],
            ),
            (
                vec!["settings"],
                vec![("settings", Some(b"captured config".as_slice()))],
            ),
            (
                vec!["config", "config"],
                vec![("config", Some(b"captured config".as_slice()))],
            ),
        ] {
            assert!(
                authenticated_extraction_context("rust", &inventory, &observed, &captured).is_err(),
                "{inventory:?} {observed:?}"
            );
        }
        let mut forged_digest = captured.clone();
        forged_digest.config_hash = "not-a-captured-digest".into();
        assert!(
            authenticated_extraction_context(
                "rust",
                &["config"],
                &[("config", Some(b"captured config"))],
                &forged_digest,
            )
            .is_err()
        );
    }

    #[test]
    fn failed_precondition_mints_no_occurrence() {
        let source = file("function f() { g(); if (x) { h(); } }");
        assert!(selected(&source, "revision:v1:r1", "native-v2").is_err());
        let mut forged = source.clone();
        forged.hash = "0".repeat(64);
        let mut artifact = selected(&source, "revision:v1:r1", NATIVE_VERSION).unwrap();
        artifact.calls.clear();
        artifact.control_regions.clear();
        assert!(extract(&mut artifact, &forged, &mut IdentityRegistry::default()).is_err());
        forged = source.clone();
        forged.text.push_str(" // changed after admission");
        assert!(extract(&mut artifact, &forged, &mut IdentityRegistry::default()).is_err());
        assert!(artifact.calls.is_empty() && artifact.control_regions.is_empty());
    }

    #[test]
    fn document_version_keeps_occurrence_ids_across_revisions() {
        let source = file("function f() { g(); if (x) { h(); } }");
        let r1 = selected(&source, "revision:v1:r1", NATIVE_VERSION).unwrap();
        let r2 = selected(&source, "revision:v1:r2", NATIVE_VERSION).unwrap();
        assert_eq!(r1.calls.len(), 2);
        assert_eq!(r1.control_regions.len(), 1);
        for (a, b) in r1.calls.iter().zip(&r2.calls) {
            assert!(a.id.starts_with("occ:v2:"));
            assert_eq!(a.id, b.id);
            assert_eq!(a.region_ids, b.region_ids);
            assert_eq!(
                (a.revision_id.as_str(), b.revision_id.as_str()),
                ("revision:v1:r1", "revision:v1:r2")
            );
        }
        assert_eq!(r1.control_regions[0].id, r2.control_regions[0].id);
        assert_ne!(r1.provenance[0].id, r2.provenance[0].id);
        let edited = selected(
            &file("function f() { g(); if (x) { h(); } } "),
            "revision:v1:r2",
            NATIVE_VERSION,
        )
        .unwrap();
        assert!(
            r1.calls
                .iter()
                .zip(&edited.calls)
                .all(|(a, b)| a.id != b.id)
        );
        assert_ne!(r1.control_regions[0].id, edited.control_regions[0].id);
    }
}

#[cfg(test)]
mod cached_executable_digest_tests {
    use super::*;
    use crate::{
        indexer::{IndexOptions, index_workspace_bundle},
        store::Store,
    };
    use std::{
        fs,
        sync::{Arc, atomic::AtomicBool},
    };

    #[test]
    fn admitted_executable_digest_survives_projection_validation_and_paired_publish() {
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        fs::write(
            workspace.path().join("Types.java"),
            "class A { void go() { helper(); } void helper() {} }\n",
        )
        .unwrap();
        let store = Store::open_for_tests(state.path(), workspace.path()).unwrap();
        let root = fs::canonicalize(workspace.path()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let (graph, native, capture) = index_workspace_bundle(
            &IndexOptions::new(workspace.path().to_owned()),
            store.root_id(),
            &cancel,
            |_| {},
        )
        .unwrap();
        let exe = std::env::current_exe().unwrap();
        assert_eq!(
            native.producer.executable_hash,
            capture.executable_digest(&exe).unwrap()
        );
        assert_eq!(capture.graph_projection_count(), 1);
        assert!(
            capture
                .source_operations
                .values()
                .all(|ops| ops.opens == 1 && ops.complete_reads == 1 && ops.hashes == 1)
        );
        native
            .validate(&capture, &root, store.root_id(), &cancel)
            .unwrap();
        assert_eq!(
            from_capture(&capture, &root, store.root_id(), &cancel).unwrap(),
            native
        );
        let session = store.leader_session().unwrap();
        let pin = store
            .publish_native(
                &graph,
                &capture,
                &native,
                session.leader_guard().unwrap(),
                store.index_baseline().unwrap(),
                &cancel,
            )
            .unwrap();
        assert_eq!(store.status().unwrap().revision, pin);
        let key = &native.revision.documents[0].key;
        let (document, bytes) = store.native_source_at(pin, key).unwrap().unwrap();
        assert_eq!(
            document.content_hash,
            native.revision.documents[0].content_hash
        );
        assert_eq!(bytes.as_slice(), graph.files[0].text.as_bytes());
        let go = store.native_declarations_at(pin, "java", "go").unwrap();
        assert!(!go.is_empty());
        assert!(go.iter().all(|row| {
            native
                .declarations
                .iter()
                .any(|expected| row.syntax_id == expected.syntax_id)
        }));
        drop(session);
    }
}
