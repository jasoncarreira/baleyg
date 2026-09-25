//! Native lexical JavaScript, Rust, Java and Python extraction. SCIP is optional JS-only evidence.
//!
//! A paired flat hash manifest is trusted user input, not proof that an artifact was
//! generated from those inputs or by any particular SCIP tool/version. Freshness
//! compares discovered sources and supported root config/lock files only. Regions
//! describe lexical guards, not execution order, dispatch, exceptions, or points-to
//! analysis. Invalid JavaScript retains recovered syntax but no semantic evidence.
//!
//! Regions cover if/else, ternary arms, short-circuit RHS, loops (excluding their
//! once-only initializer/iterable expression), try/catch/finally, and switch/cases.
//! Callback bodies have separate owners; computed method keys retain the outer
//! caller. Accessor reads are never treated as calls to a getter. Dynamic property
//! calls and injected parameters remain unresolved unless explicit callable SCIP
//! evidence exists; this module never infers points-to targets.
//!
//! Root hash scope: package.json, tsconfig.json, jsconfig.json, package-lock.json,
//! yarn.lock, pnpm-lock.yaml, bun.lock, bun.lockb, Cargo.toml, Cargo.lock, plus
//! discovered JS/MJS/CJS/Rust/Java/Python files and supported root Maven/Gradle/Python
//! config/lock files below. Non-JavaScript extraction never consumes SCIP evidence.
//! Transitive configs, dependencies, environment, and indexer tool versions are not
//! attested. Aggregate semantic_state measures manifest freshness; recovered files
//! have Unavailable provenance even when that manifest is Fresh.
//!
//! Discovery skips symlinks. Unix reads use O_NOFOLLOW and validate the opened
//! regular file and inode; malicious concurrent replacement of ancestor directories
//! is not a security boundary (there is no capability-scoped filesystem sandbox).
use crate::model::*;
use anyhow::{Context, Result, ensure};
use protobuf::Message;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};
use tree_sitter::Node;

#[derive(Clone, Debug)]
pub struct IndexOptions {
    pub workspace_root: PathBuf,
    pub scip_path: Option<PathBuf>,
    pub manifest_path: Option<PathBuf>,
    pub max_file_bytes: u64,
}
impl IndexOptions {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            scip_path: None,
            manifest_path: None,
            max_file_bytes: 2 * 1024 * 1024,
        }
    }
}
fn check(cancel: &CancelFlag) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "indexing cancelled");
    Ok(())
}
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn diag(g: &mut Graph, path: Option<String>, code: &str, message: impl Into<String>) {
    g.diagnostics.push(Diagnostic {
        path,
        code: code.into(),
        message: message.into(),
    });
}
// Discovery never follows directory symlinks; reads also reject file symlinks.
fn safe_read(path: &Path, cap: u64) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && meta.len() <= cap,
        "not a regular file or exceeds byte limit: {}",
        path.display()
    );
    use std::io::Read;
    let mut open = fs::OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Do not follow a final-component symlink inserted after metadata inspection.
        // NONBLOCK also prevents a concurrent replacement with a FIFO from hanging.
        open.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = open.open(path)?;
    let opened = file.metadata()?;
    ensure!(
        opened.is_file() && opened.len() <= cap,
        "opened input is not a regular file or exceeds byte limit: {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            meta.dev() == opened.dev() && meta.ino() == opened.ino(),
            "input changed during open: {}",
            path.display()
        );
    }
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1)).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= cap, "file grew beyond byte limit");
    Ok(bytes)
}

// These are internal acquisition records, not contract-v1 wire records. A capture
// is independent of the disposable graph and of its storage generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedDocument {
    pub key: crate::model::v1::DocumentKey,
    pub bytes: Vec<u8>,
    pub content_hash: String,
    pub byte_length: u64,
    pub syntax: Vec<CapturedSyntaxNode>,
    pub native_candidates: Vec<CapturedNativeWitness>,
    pub semantic_positions: Vec<CapturedPosition>,
    pub heritage: Vec<CapturedHeritageWitness>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedSyntaxNode {
    pub id: usize,
    pub parent_id: Option<usize>,
    pub kind: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub source_bytes: Vec<u8>,
    pub field_name: Option<String>,
    pub candidate_kind: Option<String>,
    pub name_bytes: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NativeCandidateKind {
    Declaration,
    Occurrence,
    Invocation,
    ControlRegion,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedNativeWitness {
    pub node_id: usize,
    pub parent_id: Option<usize>,
    pub owner_id: usize,
    pub candidate_kind: NativeCandidateKind,
    pub node_kind: String,
    pub name_bytes: Vec<u8>,
    pub header_bytes: Vec<u8>,
    pub token_bytes: Vec<u8>,
    pub start_byte: usize,
    pub end_byte: usize,
    pub token_start_byte: usize,
    pub token_end_byte: usize,
    pub stable_id: Option<String>,
    pub ancestor_ids: Vec<String>,
    pub spelling: Option<String>,
    pub verified_member_token: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedHeritageWitness {
    pub class_node_id: usize,
    pub owner_id: usize,
    pub subclass_name_start: usize,
    pub subclass_name_end: usize,
    pub base_start: usize,
    pub base_end: usize,
    pub base_bytes: Vec<u8>,
}

// These are AST candidates, not validated declarations/occurrences or graph facts.
fn native_candidates(nodes: &[CapturedSyntaxNode]) -> Vec<CapturedNativeWitness> {
    let mut output = Vec::new();
    for node in nodes {
        let declaration = is_declaration(&node.kind)
            || matches!(
                node.kind.as_str(),
                "function_expression" | "generator_function" | "arrow_function"
            );
        let invocation = matches!(
            node.kind.as_str(),
            "call_expression" | "new_expression" | "method_invocation" | "macro_invocation"
        );
        let control = matches!(
            node.kind.as_str(),
            "if_statement"
                | "if_expression"
                | "if_expression_statement"
                | "else_clause"
                | "for_statement"
                | "for_in_statement"
                | "do_statement"
                | "switch_case"
                | "switch_default"
                | "for_expression"
                | "while_statement"
                | "while_expression"
                | "loop_expression"
                | "try_statement"
                | "catch_clause"
                | "finally_clause"
                | "switch_statement"
                | "switch_expression"
                | "match_expression"
                | "conditional_expression"
                | "ternary_expression"
                | "binary_expression"
                | "statement_block"
                | "expression_statement"
                | "class_static_block"
        );
        let graph_region = node.parent_id.is_some_and(|id| {
            let parent = &nodes[id];
            (matches!(parent.kind.as_str(), "if_statement" | "ternary_expression")
                && matches!(
                    node.field_name.as_deref(),
                    Some("consequence" | "alternative")
                ))
                || (parent.kind == "binary_expression"
                    && node.field_name.as_deref() == Some("right")
                    && nodes.iter().any(|op| {
                        op.parent_id == Some(id)
                            && op.field_name.as_deref() == Some("operator")
                            && matches!(op.source_bytes.as_slice(), b"&&" | b"||" | b"??")
                    }))
                || (parent.kind == "field_definition"
                    && node.field_name.as_deref() == Some("value"))
        });
        let occurrence = node.kind.contains("identifier");
        if !declaration && !occurrence && !invocation && !control && !graph_region {
            continue;
        }
        let mut owner = node.parent_id.unwrap_or(0);
        while owner > 0
            && !is_declaration(&nodes[owner].kind)
            && !matches!(
                nodes[owner].kind.as_str(),
                "function_expression" | "generator_function" | "arrow_function"
            )
        {
            owner = nodes[owner].parent_id.unwrap_or(0);
        }
        let name_node = if declaration {
            nodes
                .iter()
                .find(|candidate| {
                    candidate.parent_id == Some(node.id)
                        && matches!(candidate.field_name.as_deref(), Some("name" | "declarator"))
                })
                .and_then(|candidate| {
                    if candidate.field_name.as_deref() == Some("declarator") {
                        nodes.iter().find(|child| {
                            child.parent_id == Some(candidate.id)
                                && child.field_name.as_deref() == Some("name")
                        })
                    } else {
                        Some(candidate)
                    }
                })
        } else if invocation {
            nodes.iter().find(|candidate| {
                candidate.parent_id == Some(node.id)
                    && matches!(candidate.field_name.as_deref(), Some("function" | "name"))
            })
        } else {
            None
        };
        // Anonymous JavaScript function expressions and arrows are still measured
        // declaration sites: their source header and parent give an exact key.
        let anonymous = matches!(
            node.kind.as_str(),
            "function_expression" | "generator_function" | "arrow_function"
        );
        if declaration && name_node.is_none() && !anonymous {
            continue;
        }
        let token = name_node.unwrap_or(node);
        let header_end = if declaration {
            nodes
                .iter()
                .filter(|child| {
                    child.parent_id == Some(node.id)
                        && matches!(child.field_name.as_deref(), Some("body" | "value"))
                })
                .map(|child| child.start_byte)
                .min()
                .unwrap_or(token.end_byte)
        } else {
            node.start_byte
        };
        let prefix_len = header_end
            .saturating_sub(node.start_byte)
            .min(node.source_bytes.len());
        let extra_region = graph_region && (declaration || invocation || occurrence);
        output.push(CapturedNativeWitness {
            node_id: node.id,
            parent_id: node.parent_id,
            owner_id: owner,
            candidate_kind: if declaration {
                NativeCandidateKind::Declaration
            } else if invocation {
                NativeCandidateKind::Invocation
            } else if control || graph_region {
                NativeCandidateKind::ControlRegion
            } else {
                NativeCandidateKind::Occurrence
            },
            node_kind: node.kind.clone(),
            name_bytes: token.source_bytes.clone(),
            header_bytes: if declaration {
                node.source_bytes[..prefix_len].to_vec()
            } else {
                Vec::new()
            },
            token_bytes: token.source_bytes.clone(),
            start_byte: node.start_byte,
            end_byte: node.end_byte,
            token_start_byte: token.start_byte,
            token_end_byte: token.end_byte,
            stable_id: None,
            ancestor_ids: Vec::new(),
            spelling: None,
            verified_member_token: false,
        });
        if extra_region {
            let mut region = output.last().unwrap().clone();
            region.candidate_kind = NativeCandidateKind::ControlRegion;
            output.push(region);
        }
    }
    output
}

// Only JavaScript candidates receive IDs here. Other languages have separate adapter waves.
fn identify_javascript(document: &mut CapturedDocument, revision_id: &str) -> Result<()> {
    use crate::model::v1::{Key, Kind, Language, Text};
    use crate::semantic_identity::{self as identity, OccurrenceKind};
    let nodes = &document.syntax;
    let source_set = &document.key.source_set_id;
    let path = &document.key.path;
    let module = Key {
        kind: Kind::Module,
        name: None,
        signature: None,
        ordinal: crate::model::v1::UInt::new(0).unwrap(),
    };
    let module_id = identity::syntax_id(source_set, path, Language::Javascript, &[], &module)?;
    let mut declarations: Vec<(usize, Key, u64, u64)> = Vec::new();
    for witness in &document.native_candidates {
        if witness.candidate_kind != NativeCandidateKind::Declaration {
            continue;
        }
        let kind = match witness.node_kind.as_str() {
            "class_declaration" | "class" => Kind::Type,
            "method_definition" => Kind::Method,
            "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "generator_function"
            | "arrow_function" => Kind::Function,
            _ => continue,
        };
        let anonymous = matches!(
            witness.node_kind.as_str(),
            "function_expression" | "generator_function" | "arrow_function"
        ) && witness.token_start_byte == witness.start_byte
            && witness.token_end_byte == witness.end_byte;
        let name = if anonymous {
            None
        } else {
            let Ok(name) = std::str::from_utf8(&witness.name_bytes) else {
                continue;
            };
            if !nodes.iter().any(|n| {
                n.parent_id == Some(witness.node_id)
                    && n.start_byte == witness.token_start_byte
                    && n.end_byte == witness.token_end_byte
                    && matches!(
                        n.kind.as_str(),
                        "identifier" | "property_identifier" | "private_property_identifier"
                    )
            }) {
                continue;
            }
            Some(Text::new(name.to_owned()).context("invalid JS declaration name")?)
        };
        declarations.push((
            witness.node_id,
            Key {
                kind: if anonymous {
                    Kind::AnonymousFunction
                } else {
                    kind
                },
                name,
                signature: None,
                ordinal: crate::model::v1::UInt::new(0).unwrap(),
            },
            witness.start_byte as u64,
            witness.end_byte as u64,
        ));
    }
    // Resolve each depth independently: sibling grouping sees the finalized
    // immediate-parent key, never an ancestor placeholder ordinal.
    let mut ids = HashMap::from([(0usize, module_id.as_str().to_owned())]);
    let mut resolved: HashMap<usize, Key> = HashMap::new();
    let mut collisions = identity::CollisionRegistry::default();
    declarations.sort_by_key(|(id, _, _, _)| {
        let mut depth = 0;
        let mut parent = nodes[*id].parent_id;
        while let Some(p) = parent {
            depth += 1;
            parent = nodes[p].parent_id;
        }
        (depth, nodes[*id].start_byte)
    });
    let mut offset = 0;
    while offset < declarations.len() {
        let depth = {
            let mut d = 0;
            let mut p = nodes[declarations[offset].0].parent_id;
            while let Some(id) = p {
                d += 1;
                p = nodes[id].parent_id;
            }
            d
        };
        let mut end = offset + 1;
        while end < declarations.len() {
            let mut d = 0;
            let mut p = nodes[declarations[end].0].parent_id;
            while let Some(id) = p {
                d += 1;
                p = nodes[id].parent_id;
            }
            if d != depth {
                break;
            }
            end += 1;
        }
        let entries: Vec<_> = declarations[offset..end]
            .iter()
            .map(|(id, key, start, finish)| {
                let mut lineage = Vec::new();
                let mut parent = nodes[*id].parent_id;
                while let Some(p) = parent {
                    if let Some(k) = resolved.get(&p) {
                        lineage.push(k.clone());
                    }
                    parent = nodes[p].parent_id;
                }
                lineage.reverse();
                let mut ancestors = vec![module.clone()];
                ancestors.extend(lineage);
                (ancestors, key.clone(), *start, *finish)
            })
            .collect();
        let ordinals = identity::sibling_ordinals(&entries)?;
        for ((entry, ordinal), (ancestors, _, _, _)) in
            declarations[offset..end].iter().zip(ordinals).zip(entries)
        {
            let (node_id, key, _, _) = entry;
            let mut key = key.clone();
            key.ordinal = ordinal;
            let dig =
                identity::syntax_digest(source_set, path, Language::Javascript, &ancestors, &key)?;
            let id = identity::syntax_id(source_set, path, Language::Javascript, &ancestors, &key)?;
            collisions.syntax(&id, dig.input)?;
            ids.insert(*node_id, id.as_str().to_owned());
            resolved.insert(*node_id, key);
        }
        offset = end;
    }
    let mut occurrence_entries = Vec::new();
    let mut occurrence_indices = Vec::new();
    for (index, witness) in document.native_candidates.iter_mut().enumerate() {
        let mut lineage = Vec::new();
        let mut parent = witness.parent_id;
        while let Some(id) = parent {
            if let Some(sid) = ids.get(&id) {
                lineage.push(sid.clone());
            }
            parent = nodes[id].parent_id;
        }
        lineage.reverse();
        if lineage.is_empty() {
            lineage.push(module_id.as_str().to_owned());
        }
        witness.ancestor_ids = lineage;
        if witness.candidate_kind == NativeCandidateKind::Declaration {
            witness.stable_id = ids.get(&witness.node_id).cloned();
        } else if matches!(
            witness.candidate_kind,
            NativeCandidateKind::Invocation | NativeCandidateKind::ControlRegion
        ) {
            let owner =
                crate::model::v1::SyntaxId::new(witness.ancestor_ids.last().unwrap().clone())
                    .unwrap();
            let kind = if witness.candidate_kind == NativeCandidateKind::Invocation {
                OccurrenceKind::Call
            } else {
                OccurrenceKind::Control
            };
            occurrence_entries.push((
                owner,
                kind,
                witness.start_byte as u64,
                witness.end_byte as u64,
            ));
            occurrence_indices.push(index);
        }
        if witness.candidate_kind == NativeCandidateKind::Invocation {
            let function = nodes.iter().find(|n| {
                n.parent_id == Some(witness.node_id) && n.field_name.as_deref() == Some("function")
            });
            if let Some(function) = function {
                // Optional chaining, subscripts and compound callees do not have a
                // proved simple member token. The invocation span remains measurable.
                let optional = nodes
                    .iter()
                    .any(|n| n.parent_id == Some(function.id) && n.kind == "optional_chain");
                let property = (function.kind == "member_expression" && !optional)
                    .then(|| {
                        nodes.iter().find(|n| {
                            n.parent_id == Some(function.id)
                                && n.field_name.as_deref() == Some("property")
                        })
                    })
                    .flatten();
                if let Some(property) = property.filter(|n| {
                    matches!(
                        n.kind.as_str(),
                        "property_identifier" | "private_property_identifier"
                    ) && n.end_byte <= document.bytes.len()
                }) {
                    if let Ok(raw) =
                        std::str::from_utf8(&document.bytes[property.start_byte..property.end_byte])
                        && let Ok(decoded) = identity::lookup_key(Language::Javascript, raw)
                    {
                        witness.token_start_byte = property.start_byte;
                        witness.token_end_byte = property.end_byte;
                        witness.token_bytes = raw.as_bytes().to_vec();
                        witness.name_bytes = witness.token_bytes.clone();
                        witness.spelling = Some(decoded);
                        witness.verified_member_token = true;
                    }
                } else if function.kind == "identifier" {
                    witness.spelling = std::str::from_utf8(&function.source_bytes)
                        .ok()
                        .and_then(|s| identity::lookup_key(Language::Javascript, s).ok());
                }
            }
        }
    }
    let ordinals = identity::occurrence_ordinals(&occurrence_entries)?;
    let revision = Text::new(revision_id.to_owned()).context("invalid revision ID")?;
    for (index, ordinal) in occurrence_indices.into_iter().zip(ordinals) {
        let witness = &mut document.native_candidates[index];
        let owner =
            crate::model::v1::SyntaxId::new(witness.ancestor_ids.last().unwrap().clone()).unwrap();
        let kind = if witness.candidate_kind == NativeCandidateKind::Invocation {
            OccurrenceKind::Call
        } else {
            OccurrenceKind::Control
        };
        let dig = identity::occurrence_digest(&revision, &owner, kind, ordinal)?;
        let id = identity::occurrence_id(&revision, &owner, kind, ordinal)?;
        collisions.occurrence(&id, dig.input)?;
        witness.stable_id = Some(id.as_str().to_owned());
    }
    for node in nodes
        .iter()
        .filter(|n| matches!(n.kind.as_str(), "class_declaration" | "class"))
    {
        let name = nodes
            .iter()
            .find(|n| n.parent_id == Some(node.id) && n.field_name.as_deref() == Some("name"));
        let heritage = nodes
            .iter()
            .find(|n| n.parent_id == Some(node.id) && n.kind == "class_heritage");
        if let (Some(name), Some(heritage)) = (name, heritage) {
            let base = nodes
                .iter()
                .find(|n| n.parent_id == Some(heritage.id) && n.kind == "identifier");
            if let Some(base) = base {
                document.heritage.push(CapturedHeritageWitness {
                    class_node_id: node.id,
                    owner_id: node.parent_id.unwrap_or(0),
                    subclass_name_start: name.start_byte,
                    subclass_name_end: name.end_byte,
                    base_start: base.start_byte,
                    base_end: base.end_byte,
                    base_bytes: base.source_bytes.clone(),
                });
            }
        }
    }
    Ok(())
}

fn is_declaration(kind: &str) -> bool {
    kind.contains("declaration")
        || kind.contains("definition")
        || matches!(kind, "function_item" | "method_definition")
}

fn capture_syntax(
    language: crate::model::v1::Language,
    bytes: &[u8],
) -> Result<Vec<CapturedSyntaxNode>> {
    use crate::model::v1::Language;
    let grammar = match language {
        Language::Java => tree_sitter_java::LANGUAGE,
        Language::Rust => tree_sitter_rust::LANGUAGE,
        Language::Python => tree_sitter_python::LANGUAGE,
        Language::Javascript => tree_sitter_javascript::LANGUAGE,
    };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&grammar.into())?;
    let tree = parser
        .parse(bytes, None)
        .context("native AST parse failed")?;
    let mut nodes = Vec::new();
    let mut pending = vec![(tree.root_node(), None, None)];
    while let Some((node, parent_id, field_name)) = pending.pop() {
        ensure!(nodes.len() < 1_000_000, "AST exceeds node limit");
        let id = nodes.len();
        nodes.push(CapturedSyntaxNode {
            id,
            parent_id,
            kind: node.kind().into(),
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            source_bytes: bytes[node.byte_range()].to_vec(),
            field_name,
            candidate_kind: node.is_named().then(|| node.kind().to_owned()),
            name_bytes: node.is_named().then(|| bytes[node.byte_range()].to_vec()),
        });
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).enumerate().collect();
        for (index, child) in children.into_iter().rev() {
            pending.push((
                child,
                Some(id),
                node.field_name_for_child(index as u32).map(str::to_owned),
            ));
        }
    }
    Ok(nodes)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedProducer {
    pub id: String,
    pub tool_name: String,
    pub version: String,
    pub position_encoding: String,
    pub executable_bytes: Vec<u8>,
    pub executable_hash: String,
    pub artifact_bytes: Option<Vec<u8>>,
    pub artifact_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProducerInput {
    pub id: String,
    pub tool_name: String,
    pub version: String,
    pub position_encoding: String,
    pub executable: PathBuf,
    pub artifact: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureAdmission {
    pub source_set_id: String,
    pub root_id: String,
    pub languages: Vec<crate::model::v1::Language>,
    pub toolchain: PathBuf,
    pub config: PathBuf,
    pub dependency: PathBuf,
    pub dependency_source_sets: Vec<String>,
    pub producers: Vec<ProducerInput>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedRevision {
    pub source_set_id: String,
    pub root_id: String,
    pub revision_id: String,
    pub documents: Vec<CapturedDocument>,
    pub manifest_bytes: Vec<u8>,
    pub source_inputs: Vec<(String, Vec<u8>)>,
    pub toolchain_bytes: Vec<u8>,
    pub config_bytes: Vec<u8>,
    pub dependency_bytes: Vec<u8>,
    pub toolchain_hash: String,
    pub config_hash: String,
    pub dependency_hash: String,
    pub producers: Vec<CapturedProducer>,
    pub dependency_source_sets: Vec<String>,
}

// A semantic adapter keeps the producer's original coordinate and encoding;
// conversion and join validation are deliberately deferred to the validator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedPosition {
    pub producer_id: String,
    pub document: crate::model::v1::DocumentKey,
    pub revision_id: String,
    pub position_encoding: String,
    pub coordinates: Vec<u64>,
    pub symbol: String,
    pub roles: u32,
    pub artifact_hash: String,
}

fn capture_file(root: &Path, path: &Path, cap: u64) -> Result<Vec<u8>> {
    let canonical_root = fs::canonicalize(root)?;
    let parent = fs::canonicalize(path.parent().context("input has no parent")?)?;
    ensure!(
        parent.starts_with(&canonical_root),
        "input escapes admitted root: {}",
        path.display()
    );
    // A before/after read catches in-place edits and replacements during capture.
    let first = safe_read(path, cap)?;
    let second = safe_read(path, cap)?;
    ensure!(
        first == second,
        "input changed during capture: {}",
        path.display()
    );
    Ok(first)
}

fn discover_capture_inputs(
    root: &Path,
    cancel: &CancelFlag,
) -> Result<Vec<(PathBuf, Option<crate::model::v1::Language>)>> {
    use crate::model::v1::Language;
    let mut paths = Vec::new();
    let mut walk = ignore::WalkBuilder::new(root);
    walk.require_git(false)
        .follow_links(false)
        .hidden(true)
        .filter_entry({
            let root = root.to_owned();
            move |e| {
                if e.depth() == 0 {
                    return true;
                }
                match e.file_name().to_str() {
                    Some(".git" | "node_modules" | ".venv" | ".baleyg") => false,
                    Some("target" | "dist" | "build") => {
                        e.path().strip_prefix(&root).ok().is_some_and(|r| {
                            let parts: Vec<_> = r.components().collect();
                            parts.windows(3).any(|p| {
                                p[0].as_os_str() == "src"
                                    && matches!(
                                        p[1].as_os_str().to_str(),
                                        Some("main" | "test" | "testFixtures")
                                    )
                                    && p[2].as_os_str() == "java"
                            })
                        })
                    }
                    _ => true,
                }
            }
        });
    for entry in walk.build() {
        check(cancel)?;
        let entry = entry.context("unsafe or unreadable source discovery")?;
        if entry.file_type().is_some_and(|t| t.is_symlink()) {
            let extension = entry.path().extension().and_then(|s| s.to_str());
            ensure!(
                !matches!(extension, Some("rs" | "java" | "py" | "js" | "mjs" | "cjs")),
                "unsafe symlink source: {}",
                entry.path().display()
            );
        }
        if entry.file_type().is_some_and(|t| t.is_file()) {
            let path = entry.into_path();
            let name = path.file_name().and_then(|s| s.to_str());
            let language = match path.extension().and_then(|s| s.to_str()) {
                Some("rs") => Some(Language::Rust),
                Some("java") => Some(Language::Java),
                Some("py") => Some(Language::Python),
                Some("js" | "mjs" | "cjs") => Some(Language::Javascript),
                _ => None,
            };
            if language.is_some() || matches!(name, Some(".gitignore" | ".ignore")) {
                paths.push((path, language));
            }
        }
        ensure!(paths.len() <= 100_000, "capture exceeds 100000 inputs");
    }
    // The ignore policy itself affects discovery and therefore belongs to the snapshot.
    // Also record root-level project metadata even if it is not a source document.
    for name in [
        ".gitignore",
        ".ignore",
        "package.json",
        "tsconfig.json",
        "jsconfig.json",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "bun.lock",
        "bun.lockb",
        "Cargo.toml",
        "Cargo.lock",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
        "gradle.properties",
        "pyproject.toml",
        "requirements.txt",
        "uv.lock",
        "poetry.lock",
        "Pipfile",
        "Pipfile.lock",
    ] {
        let path = root.join(name);
        if fs::symlink_metadata(&path).is_ok() && !paths.iter().any(|(p, _)| p == &path) {
            paths.push((path, None));
        }
    }
    paths.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(paths)
}

pub fn capture_revision(
    options: &IndexOptions,
    admission: &CaptureAdmission,
    cancel: &CancelFlag,
) -> Result<CapturedRevision> {
    capture_revision_with_hook(options, admission, cancel, || {})
}

/// The hook runs after the first source inventory and before its verification.
/// This makes acquisition races reproducible without changing normal capture.
#[doc(hidden)]
pub fn capture_revision_with_hook(
    options: &IndexOptions,
    admission: &CaptureAdmission,
    cancel: &CancelFlag,
    before_verification: impl FnOnce(),
) -> Result<CapturedRevision> {
    use crate::model::v1::{DocumentKey, Language, Path as EvidencePath, Text};
    check(cancel)?;
    let identity = crate::store::topology::WorkspaceIdentity::discover_unattached(
        Some(&options.workspace_root),
        &options.workspace_root,
    )?;
    identity.verify()?;
    ensure!(
        admission.source_set_id == identity.record_id && admission.root_id == identity.root_key,
        "unadmitted workspace identity"
    );
    // No external source-set registry is available at this boundary. Do not
    // admit a caller-asserted dependency, including a self-reference.
    ensure!(
        admission.dependency_source_sets.is_empty(),
        "dependency source set is not independently admitted"
    );
    ensure!(
        !admission.languages.is_empty()
            && admission
                .languages
                .iter()
                .enumerate()
                .all(|(i, lang)| !admission.languages[..i].contains(lang)),
        "missing or duplicate admitted languages"
    );
    ensure!(
        !fs::symlink_metadata(&options.workspace_root)?
            .file_type()
            .is_symlink(),
        "workspace root is a symlink"
    );
    let root = identity.root.clone();
    let mut documents = Vec::new();
    let mut source_inputs = Vec::new();
    let paths = discover_capture_inputs(&root, cancel)?;
    let mut total = 0u64;
    let mut observed_inputs = Vec::new();
    for (path, language) in &paths {
        check(cancel)?;
        let relative = path
            .strip_prefix(&root)?
            .to_str()
            .context("non-UTF8 capture path")?
            .replace('\\', "/");
        let bytes = capture_file(&root, path, options.max_file_bytes.min(256 * 1024 * 1024))?;
        total += bytes.len() as u64;
        ensure!(total <= 256 * 1024 * 1024, "capture exceeds 256 MiB");
        observed_inputs.push((path, bytes.clone()));
        if let Some(language) = language {
            ensure!(
                admission.languages.contains(language),
                "source language not admitted: {relative}"
            );
            let key = DocumentKey {
                source_set_id: Text::new(&admission.source_set_id).context("invalid source set")?,
                language: *language,
                path: EvidencePath::new(relative).context("invalid source path")?,
            };
            let syntax = capture_syntax(*language, &bytes)?;
            documents.push(CapturedDocument {
                key,
                content_hash: digest(&bytes),
                byte_length: bytes.len() as u64,
                bytes,
                native_candidates: native_candidates(&syntax),
                syntax,
                semantic_positions: Vec::new(),
                heritage: Vec::new(),
            });
        } else {
            source_inputs.push((relative, bytes));
        }
    }
    // v1 canonical manifest is an array of {document,contentHash}, ordered by
    // the language declaration order and then unsigned UTF-8 path bytes.
    fn language_order(language: crate::model::v1::Language) -> u8 {
        match language {
            Language::Java => 0,
            Language::Rust => 1,
            Language::Python => 2,
            Language::Javascript => 3,
        }
    }
    documents.sort_by(|a, b| {
        (
            language_order(a.key.language),
            a.key.path.as_str().as_bytes(),
        )
            .cmp(&(
                language_order(b.key.language),
                b.key.path.as_str().as_bytes(),
            ))
    });
    let manifest: Vec<_> = documents
        .iter()
        .map(|d| serde_json::json!({"document":d.key,"contentHash":d.content_hash}))
        .collect();
    let manifest_bytes = crate::semantic_identity::canonical_json(&manifest)?;
    let toolchain_bytes = capture_file(&root, &admission.toolchain, 16 * 1024 * 1024)?;
    let config_bytes = capture_file(&root, &admission.config, 16 * 1024 * 1024)?;
    let dependency_bytes = capture_file(&root, &admission.dependency, 16 * 1024 * 1024)?;
    let mut producers = Vec::new();
    let native_bytes = safe_read(&std::env::current_exe()?, 256 * 1024 * 1024)?;
    producers.push(CapturedProducer {
        id: "N".into(),
        tool_name: env!("CARGO_PKG_NAME").into(),
        version: env!("CARGO_PKG_VERSION").into(),
        position_encoding: "utf8".into(),
        executable_hash: digest(&native_bytes),
        executable_bytes: native_bytes,
        artifact_hash: None,
        artifact_bytes: None,
    });
    // A prebuilt SCIP index does not attest which process generated it. Only
    // capture the configured artifact and an explicitly supplied, readable
    // executable; metadata is checked, not treated as an execution receipt.
    if let Some(scip_path) = &options.scip_path {
        let input = admission
            .producers
            .iter()
            .find(|p| p.id != "N")
            .context("semantic executable admission missing")?;
        ensure!(
            admission.producers.len() == 1
                && !input.id.is_empty()
                && input.artifact.as_ref() == Some(scip_path),
            "semantic artifact differs from indexed artifact"
        );
        ensure!(
            !input.tool_name.is_empty()
                && !input.version.is_empty()
                && !input.position_encoding.is_empty(),
            "missing semantic producer metadata"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                fs::symlink_metadata(&input.executable)?
                    .permissions()
                    .mode()
                    & 0o111
                    != 0,
                "semantic producer path is not executable"
            );
        }
        let executable_bytes = safe_read(&input.executable, 256 * 1024 * 1024)?;
        let artifact_bytes = safe_read(scip_path, 256 * 1024 * 1024)?;
        let index = scip::types::Index::parse_from_bytes(&artifact_bytes)?;
        let tool = index
            .metadata
            .as_ref()
            .and_then(|m| m.tool_info.as_ref())
            .context("SCIP producer metadata missing")?;
        ensure!(
            tool.name == input.tool_name && tool.version == input.version,
            "SCIP producer metadata does not match admitted producer"
        );
        ensure!(
            input.position_encoding == "utf8"
                || input.position_encoding == "utf16"
                || input.position_encoding == "unicodeScalar",
            "unsupported position encoding"
        );
        let artifact_hash = digest(&artifact_bytes);
        for doc in &mut documents {
            for original in index
                .documents
                .iter()
                .filter(|d| d.relative_path == doc.key.path.as_str())
            {
                for occurrence in &original.occurrences {
                    let original_range: Vec<i32> = match &occurrence.typed_range {
                        Some(scip::types::occurrence::Typed_range::SingleLineRange(r)) => {
                            vec![r.line, r.start_character, r.end_character]
                        }
                        Some(scip::types::occurrence::Typed_range::MultiLineRange(r)) => {
                            vec![r.start_line, r.start_character, r.end_line, r.end_character]
                        }
                        None => occurrence.range.clone(),
                        Some(_) => anyhow::bail!("unsupported SCIP typed range"),
                    };
                    ensure!(
                        matches!(original_range.len(), 3 | 4)
                            && original_range.iter().all(|v| *v >= 0),
                        "invalid SCIP range"
                    );
                    doc.semantic_positions.push(CapturedPosition {
                        producer_id: input.id.clone(),
                        document: doc.key.clone(),
                        revision_id: String::new(),
                        position_encoding: input.position_encoding.clone(),
                        coordinates: original_range.iter().map(|v| *v as u64).collect(),
                        symbol: occurrence.symbol.clone(),
                        roles: occurrence.symbol_roles as u32,
                        artifact_hash: artifact_hash.clone(),
                    });
                }
            }
        }
        producers.push(CapturedProducer {
            id: input.id.clone(),
            tool_name: tool.name.clone(),
            version: input.version.clone(),
            position_encoding: input.position_encoding.clone(),
            executable_hash: digest(&executable_bytes),
            executable_bytes,
            artifact_hash: Some(artifact_hash),
            artifact_bytes: Some(artifact_bytes),
        });
    } else {
        ensure!(
            admission.producers.is_empty(),
            "semantic producer supplied without indexed artifact"
        );
    }
    let toolchain_hash = digest(&toolchain_bytes);
    let config_hash = digest(&config_bytes);
    let dependency_hash = digest(&dependency_bytes);
    // Length-prefixed parts prevent boundary ambiguity; no machine path or graph
    // index pin participates in this immutable identity.
    let mut revision = Sha256::new();
    for part in [
        b"baleyg-captured-revision-v1".as_slice(),
        admission.source_set_id.as_bytes(),
        admission.root_id.as_bytes(),
        &manifest_bytes,
        &crate::semantic_identity::canonical_json(&admission.languages)?,
        &crate::semantic_identity::canonical_json(&admission.dependency_source_sets)?,
        &serde_json::to_vec(&source_inputs)?,
        &toolchain_bytes,
        &config_bytes,
        &dependency_bytes,
        &serde_json::to_vec(
            &producers
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
        )?,
    ] {
        revision.update((part.len() as u64).to_be_bytes());
        revision.update(part);
    }
    let revision_id = format!("rev:v1:{}", hex::encode(revision.finalize()));
    for document in &mut documents {
        if document.key.language == Language::Javascript {
            identify_javascript(document, &revision_id)?;
        }
        for position in &mut document.semantic_positions {
            position.revision_id = revision_id.clone();
        }
    }
    before_verification();
    for (path, bytes) in observed_inputs {
        check(cancel)?;
        ensure!(
            capture_file(&root, path, options.max_file_bytes.min(256 * 1024 * 1024))? == bytes,
            "source changed during capture: {}",
            path.display()
        );
    }
    ensure!(
        capture_file(&root, &admission.toolchain, 16 * 1024 * 1024)? == toolchain_bytes
            && capture_file(&root, &admission.config, 16 * 1024 * 1024)? == config_bytes
            && capture_file(&root, &admission.dependency, 16 * 1024 * 1024)? == dependency_bytes,
        "basis inputs changed during capture"
    );
    for producer in &producers {
        if producer.id == "N" {
            ensure!(
                safe_read(&std::env::current_exe()?, 256 * 1024 * 1024)?
                    == producer.executable_bytes,
                "native executable changed during capture"
            );
        } else {
            let input = &admission.producers[0];
            ensure!(
                safe_read(&input.executable, 256 * 1024 * 1024)? == producer.executable_bytes
                    && safe_read(
                        options.scip_path.as_ref().context("missing SCIP path")?,
                        256 * 1024 * 1024
                    )? == *producer
                        .artifact_bytes
                        .as_ref()
                        .context("missing SCIP artifact")?,
                "semantic producer inputs changed during capture"
            );
        }
    }
    ensure!(
        discover_capture_inputs(&root, cancel)? == paths,
        "source set changed during capture"
    );
    identity.verify()?;
    check(cancel)?;
    Ok(CapturedRevision {
        source_set_id: admission.source_set_id.clone(),
        root_id: admission.root_id.clone(),
        revision_id,
        documents,
        manifest_bytes,
        source_inputs,
        toolchain_bytes,
        config_bytes,
        dependency_bytes,
        toolchain_hash,
        config_hash,
        dependency_hash,
        producers,
        dependency_source_sets: admission.dependency_source_sets.clone(),
    })
}

pub fn index_workspace(
    options: &IndexOptions,
    cancel: &CancelFlag,
    progress: impl Fn(IndexProgress) + Sync,
) -> Result<Graph> {
    check(cancel)?;
    ensure!(
        options.workspace_root.is_dir(),
        "workspace root is not a directory"
    );
    ensure!(
        !fs::symlink_metadata(&options.workspace_root)?
            .file_type()
            .is_symlink(),
        "workspace root is a symlink"
    );
    let workspace_root = fs::canonicalize(&options.workspace_root)?;
    let mut g = Graph::default();
    let mut paths = Vec::new();
    let mut walk = ignore::WalkBuilder::new(&workspace_root);
    let filter_root = workspace_root.clone();
    walk.require_git(false)
        .follow_links(false)
        .hidden(true)
        .filter_entry(move |e| {
            if e.depth() == 0 {
                return true;
            }
            match e.file_name().to_str() {
                Some(".git" | "node_modules" | ".venv" | ".baleyg") => false,
                Some("target" | "dist" | "build") => {
                    // These are legal Java package names inside conventional source
                    // trees, not build-output roots. Earlier artifact ancestors and
                    // .gitignore rules still exclude generated/dependency trees.
                    e.path()
                        .strip_prefix(&filter_root)
                        .ok()
                        .is_some_and(|relative| {
                            let parts: Vec<_> = relative.components().collect();
                            parts.windows(3).any(|p| {
                                p[0].as_os_str() == "src"
                                    && matches!(
                                        p[1].as_os_str().to_str(),
                                        Some("main" | "test" | "testFixtures")
                                    )
                                    && p[2].as_os_str() == "java"
                            })
                        })
                }
                _ => true,
            }
        });
    for entry in walk.build() {
        check(cancel)?;
        match entry {
            Ok(e)
                if e.file_type().is_some_and(|t| t.is_file())
                    && matches!(
                        e.path().extension().and_then(|x| x.to_str()),
                        Some("js" | "mjs" | "cjs" | "rs" | "java" | "py")
                    ) =>
            {
                paths.push(e.into_path())
            }
            Err(e) => diag(&mut g, None, "scan-error", e.to_string()),
            _ => {}
        }
        ensure!(
            paths.len() <= 100_000,
            "workspace exceeds 100000 source files"
        );
    }
    paths.sort();
    let total = paths.len();
    let mut bytes_total = 0usize;
    let mut hashes = BTreeMap::new();
    for (i, path) in paths.into_iter().enumerate() {
        check(cancel)?;
        let rel = path
            .strip_prefix(&workspace_root)?
            .to_str()
            .context("non-UTF8 source path")?
            .replace('\\', "/");
        match safe_read(&path, options.max_file_bytes.min(256 * 1024 * 1024))
            .and_then(|b| Ok((digest(&b), String::from_utf8(b)?)))
        {
            Ok((hash, text)) => {
                bytes_total += text.len();
                ensure!(
                    bytes_total <= 256 * 1024 * 1024,
                    "workspace source exceeds 256 MiB"
                );
                hashes.insert(rel.clone(), hash.clone());
                g.files.push(SourceFile {
                    path: rel,
                    hash,
                    language: match path.extension().and_then(|ext| ext.to_str()) {
                        Some("rs") => "rust",
                        Some("java") => "java",
                        Some("py") => "python",
                        _ => "javascript",
                    }
                    .into(),
                    text,
                });
            }
            Err(e) => diag(&mut g, Some(rel), "source-skipped", e.to_string()),
        }
        progress(IndexProgress {
            phase: "scan".into(),
            completed: i + 1,
            total,
        });
    }
    for config in [
        "package.json",
        "tsconfig.json",
        "jsconfig.json",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "bun.lock",
        "bun.lockb",
        "Cargo.toml",
        "Cargo.lock",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
        "gradle.properties",
        "pyproject.toml",
        "requirements.txt",
        "uv.lock",
        "poetry.lock",
        "Pipfile",
        "Pipfile.lock",
    ] {
        let p = workspace_root.join(config);
        if fs::symlink_metadata(&p).is_ok() {
            match safe_read(&p, options.max_file_bytes.min(16 * 1024 * 1024)) {
                Ok(b) => {
                    hashes.insert(config.into(), digest(&b));
                }
                Err(e) => diag(&mut g, Some(config.into()), "config-skipped", e.to_string()),
            }
        }
    }
    check(cancel)?;
    let mut documents = BTreeMap::new();
    let mut semantic = SemanticState::Unavailable;
    if let Some(path) = &options.scip_path {
        match safe_read(path, 256 * 1024 * 1024)
            .and_then(|b| Ok(scip::types::Index::parse_from_bytes(&b)?))
        {
            Ok(index) => {
                if let Some(manifest) = &options.manifest_path {
                    match safe_read(manifest, 16 * 1024 * 1024)
                        .and_then(|b| Ok(serde_json::from_slice::<BTreeMap<String, String>>(&b)?))
                    {
                        Ok(prior) => {
                            let keys: BTreeSet<_> =
                                prior.keys().chain(hashes.keys()).cloned().collect();
                            g.stats.changed_files = keys
                                .into_iter()
                                .filter(|p| prior.get(p) != hashes.get(p))
                                .collect();
                            semantic =
                                if g.stats.changed_files.is_empty() && g.diagnostics.is_empty() {
                                    SemanticState::Fresh
                                } else {
                                    SemanticState::Stale
                                };
                        }
                        Err(e) => diag(&mut g, None, "manifest-unavailable", e.to_string()),
                    }
                } else {
                    diag(
                        &mut g,
                        None,
                        "manifest-unavailable",
                        "SCIP requires a matching source hash manifest",
                    );
                }
                if semantic == SemanticState::Fresh {
                    for doc in index.documents {
                        documents.insert(doc.relative_path.clone(), doc);
                    }
                }
            }
            Err(e) => diag(&mut g, None, "scip-unavailable", e.to_string()),
        }
    }
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_javascript::LANGUAGE.into())?;
    let mut semantic_nodes = HashMap::new();
    for i in 0..g.files.len() {
        check(cancel)?;
        // Temporarily move the text, rather than duplicate every source buffer.
        let file = g.files[i].clone();
        if file.language != "javascript" {
            match file.language.as_str() {
                "rust" => crate::indexer_rust::extract(&mut g, &file, cancel)?,
                "java" => crate::indexer_java::extract(&mut g, &file, cancel)?,
                "python" => crate::indexer_python::extract(&mut g, &file, cancel)?,
                _ => unreachable!("source discovery returned an unsupported language"),
            }
            progress(IndexProgress {
                phase: "parse".into(),
                completed: i + 1,
                total: g.files.len(),
            });
            continue;
        }
        let mut cancelled = |_: &tree_sitter::ParseState| cancel.load(Ordering::Relaxed);
        let tree = parser.parse_with_options(
            &mut |offset, _| &file.text.as_bytes()[offset..],
            None,
            Some(tree_sitter::ParseOptions::new().progress_callback(&mut cancelled)),
        );
        check(cancel)?;
        let tree = tree.context("JavaScript parser failed")?;
        if tree.root_node().has_error() {
            g.stats.parse_error_files += 1;
            diag(
                &mut g,
                Some(file.path.clone()),
                "parse-error",
                "Tree-sitter recovered from invalid JavaScript; results may be incomplete",
            );
        }
        let source_set = crate::store::topology::WorkspaceIdentity::discover_unattached(
            Some(&workspace_root),
            &workspace_root,
        )?
        .record_id;
        let key = crate::model::v1::DocumentKey {
            source_set_id: crate::model::v1::Text::new(source_set).context("invalid source set")?,
            language: crate::model::v1::Language::Javascript,
            path: crate::model::v1::Path::new(file.path.clone()).context("invalid JS path")?,
        };
        let syntax = capture_syntax(crate::model::v1::Language::Javascript, file.text.as_bytes())?;
        let mut native = CapturedDocument {
            key,
            bytes: file.text.as_bytes().to_vec(),
            content_hash: file.hash.clone(),
            byte_length: file.text.len() as u64,
            native_candidates: native_candidates(&syntax),
            syntax,
            semantic_positions: Vec::new(),
            heritage: Vec::new(),
        };
        // A browser index has no admitted captured revision. Its occurrence IDs
        // are scoped to this source snapshot, not claimed as capture-revision IDs.
        identify_javascript(&mut native, &format!("rev:v1:{}", file.hash))?;
        let native_ids: HashMap<_, _> = native
            .native_candidates
            .iter()
            .filter_map(|w| {
                w.stable_id.as_ref().map(|id| {
                    (
                        (w.start_byte, w.end_byte, w.candidate_kind.clone()),
                        id.clone(),
                    )
                })
            })
            .collect();
        let mut ex = Extractor {
            native_ids: &native_ids,
            semantic_nodes: &mut semantic_nodes,
            g: &mut g,
            file: &file,
            doc: if tree.root_node().has_error() {
                None
            } else {
                documents.get(&file.path)
            },
            semantic: if tree.root_node().has_error() {
                SemanticState::Unavailable
            } else {
                semantic
            },
            functions: HashMap::new(),
            classes: HashMap::new(),
            cancel,
        };
        let module = crate::semantic_identity::syntax_id(
            &native.key.source_set_id,
            &native.key.path,
            crate::model::v1::Language::Javascript,
            &[],
            &crate::model::v1::Key {
                kind: crate::model::v1::Kind::Module,
                name: None,
                signature: None,
                ordinal: crate::model::v1::UInt::new(0).unwrap(),
            },
        )?
        .as_str()
        .to_owned();
        ex.g.nodes.push(Symbol {
            id: module.clone(),
            name: file.path.clone(),
            kind: SymbolKind::Module,
            path: file.path.clone(),
            range: range(tree.root_node()),
            parent: None,
            accessor: false,
            provenance: ex.provenance(false),
        });
        ex.declarations(tree.root_node(), &module, 0)?;
        ex.walk(tree.root_node(), &module, &[], 0)?;
        progress(IndexProgress {
            phase: "parse".into(),
            completed: i + 1,
            total: ex.g.files.len(),
        });
    }
    let by_id: HashMap<_, _> = g
        .nodes
        .iter()
        .map(|n| (n.id.clone(), (n.kind, n.accessor)))
        .collect();
    for c in &mut g.calls {
        check(cancel)?;
        if c.candidate_symbols.len() > 1 {
            c.resolution = Resolution::Ambiguous;
        } else if let Some(id) = c.candidate_symbols.first() {
            let native = semantic_nodes.get(id).unwrap_or(id);
            if by_id
                .get(native)
                .is_some_and(|(k, a)| *k != SymbolKind::Module && !a)
            {
                c.target = Some(native.clone());
                c.resolution = Resolution::Internal;
            } else if !by_id.contains_key(id) && id.ends_with("().") {
                c.target = Some(id.clone());
                c.resolution = Resolution::External;
            }
        }
        for callback in &mut c.callback_arguments {
            if let Some(native) = semantic_nodes.get(callback) {
                *callback = native.clone();
            }
        }
        c.callback_arguments.retain(|id| {
            by_id
                .get(id)
                .is_some_and(|(k, a)| *k != SymbolKind::Module && !a)
        });
    }
    g.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    g.regions.sort_by(|a, b| a.id.cmp(&b.id));
    g.calls
        .sort_by(|a, b| (&a.path, a.range.start_byte).cmp(&(&b.path, b.range.start_byte)));
    let mut ordinals = HashMap::new();
    for c in &mut g.calls {
        let n = ordinals.entry(c.caller.clone()).or_insert(0);
        *n += 1;
        c.ordinal = *n;
    }
    g.stats.files = g.files.len();
    g.stats.symbols = g.nodes.len();
    g.stats.calls = g.calls.len();
    g.stats.regions = g.regions.len();
    g.stats.semantic_state = semantic;
    for c in &g.calls {
        match c.resolution {
            Resolution::Internal => g.stats.internal += 1,
            Resolution::External => g.stats.external += 1,
            Resolution::Unresolved => g.stats.unresolved += 1,
            Resolution::Ambiguous => g.stats.ambiguous += 1,
        }
    }
    g.diagnostics
        .sort_by(|a, b| (&a.path, &a.code, &a.message).cmp(&(&b.path, &b.code, &b.message)));
    check(cancel)?;
    progress(IndexProgress {
        phase: "complete".into(),
        completed: g.files.len(),
        total: g.files.len(),
    });
    check(cancel)?;
    Ok(g)
}
fn range(n: Node<'_>) -> SourceRange {
    SourceRange {
        start_byte: n.start_byte(),
        end_byte: n.end_byte(),
        start_line: n.start_position().row + 1,
        start_column: n.start_position().column + 1,
        end_line: n.end_position().row + 1,
        end_column: n.end_position().column + 1,
    }
}
fn children(n: Node<'_>) -> Vec<Node<'_>> {
    let mut c = n.walk();
    n.named_children(&mut c).collect()
}
fn unwrap(mut n: Node<'_>) -> Node<'_> {
    while n.kind() == "parenthesized_expression" && n.named_child_count() == 1 {
        n = n.named_child(0).unwrap();
    }
    n
}
fn is_function(n: Node<'_>) -> bool {
    matches!(
        n.kind(),
        "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "generator_function_declaration"
            | "generator_function"
            | "method_definition"
    )
}
struct Extractor<'a> {
    g: &'a mut Graph,
    native_ids: &'a HashMap<(usize, usize, NativeCandidateKind), String>,
    semantic_nodes: &'a mut HashMap<String, String>,
    file: &'a SourceFile,
    doc: Option<&'a scip::types::Document>,
    semantic: SemanticState,
    functions: HashMap<usize, String>,
    classes: HashMap<usize, String>,
    cancel: &'a CancelFlag,
}
impl Extractor<'_> {
    fn native_id(&self, n: Node<'_>, kind: NativeCandidateKind) -> Option<String> {
        self.native_ids
            .get(&(n.start_byte(), n.end_byte(), kind))
            .cloned()
    }
    fn text(&self, n: Node<'_>) -> &str {
        &self.file.text[n.byte_range()]
    }
    fn provenance(&self, scip: bool) -> Provenance {
        Provenance {
            source: if scip {
                "scip+tree-sitter"
            } else {
                "tree-sitter"
            }
            .into(),
            semantic: self.semantic,
        }
    }
    fn symbols(&self, n: Option<Node<'_>>, definition: bool) -> Vec<String> {
        let (Some(n), Some(doc)) = (n, self.doc) else {
            return vec![];
        };
        let coord = |byte: usize| {
            let prefix = &self.file.text[..byte];
            let start = prefix.rfind('\n').map_or(0, |i| i + 1);
            let line = prefix.bytes().filter(|b| *b == b'\n').count() as i32;
            let s = &self.file.text[start..byte];
            let col = match doc.position_encoding.value() {
                1 => s.len(),
                3 => s.chars().count(),
                _ => s.encode_utf16().count(),
            };
            (line, col as i32)
        };
        let (sr, sc) = coord(n.start_byte());
        let (er, ec) = coord(n.end_byte());
        doc.occurrences
            .iter()
            .filter(|o| {
                (o.symbol_roles & 1 != 0) == definition
                    && !o.symbol.is_empty()
                    && match o.range.as_slice() {
                        [a, b, c] => *a == sr && *b == sc && sr == er && *c == ec,
                        [a, b, c, d] => [*a, *b, *c, *d] == [sr, sc, er, ec],
                        _ => false,
                    }
            })
            .map(|o| {
                if o.symbol.starts_with("local ") {
                    format!("local:{}:{}:{}", self.file.path, self.file.hash, o.symbol)
                } else {
                    o.symbol.clone()
                }
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    fn declarations(&mut self, n: Node<'_>, parent: &str, depth: usize) -> Result<()> {
        check(self.cancel)?;
        ensure!(depth < 512, "JavaScript nesting exceeds 512 levels");
        let mut owner = parent.to_owned();
        if is_function(n) || matches!(n.kind(), "class_declaration" | "class") {
            let mut name = n.child_by_field_name("name");
            if name.is_none()
                && let Some(p) = n.parent()
            {
                name = match p.kind() {
                    "variable_declarator" => p.child_by_field_name("name"),
                    "pair" => p.child_by_field_name("key"),
                    _ => None,
                };
            }
            let ids = self.symbols(name, true);
            let semantic = ids.len() == 1 && !self.g.nodes.iter().any(|node| node.id == ids[0]);
            let Some(id) = self.native_id(n, NativeCandidateKind::Declaration) else {
                // A declaration without an independently measured native name or
                // anonymous-expression shape cannot claim a syntax identity.
                for child in children(n) {
                    self.declarations(child, &owner, depth + 1)?;
                }
                return Ok(());
            };
            if semantic {
                self.semantic_nodes.insert(ids[0].clone(), id.clone());
            }
            let label = name.map(|v| self.text(v).to_owned()).unwrap_or_else(|| {
                format!(
                    "<callback@{}:{}>",
                    n.start_position().row + 1,
                    n.start_position().column + 1
                )
            });
            let accessor = n.kind() == "method_definition" && {
                let mut cur = n.walk();
                n.children(&mut cur)
                    .any(|c| matches!(c.kind(), "get" | "set"))
            };
            let kind = if n.kind() == "method_definition" {
                SymbolKind::Method
            } else if is_function(n) {
                SymbolKind::Function
            } else {
                SymbolKind::Class
            };
            self.g.nodes.push(Symbol {
                id: id.clone(),
                name: label,
                kind,
                path: self.file.path.clone(),
                range: range(n),
                parent: Some(parent.into()),
                accessor,
                provenance: self.provenance(semantic),
            });
            if is_function(n) {
                self.functions.insert(n.id(), id.clone());
            } else {
                self.classes.insert(n.id(), id.clone());
            }
            owner = id;
        }
        for child in children(n) {
            self.declarations(child, &owner, depth + 1)?;
        }
        Ok(())
    }
    fn region(&mut self, n: Node<'_>, kind: &str, context: &[String], owner: &str) -> Vec<String> {
        let Some(id) = self.native_id(n, NativeCandidateKind::ControlRegion) else {
            return context.to_vec();
        };
        self.g.regions.push(ControlRegion {
            id: id.clone(),
            kind: kind.into(),
            label: self.text(n).chars().take(140).collect(),
            parent: context.last().cloned(),
            owner: owner.into(),
            path: self.file.path.clone(),
            range: range(n),
        });
        let mut result = context.to_vec();
        result.push(id);
        result
    }
    fn walk(&mut self, n: Node<'_>, caller: &str, context: &[String], depth: usize) -> Result<()> {
        check(self.cancel)?;
        ensure!(depth < 512, "JavaScript nesting exceeds 512 levels");
        let mut caller = caller.to_owned();
        let mut context = context.to_vec();
        let mut skip = None;
        if let Some(id) = self.functions.get(&n.id()).cloned() {
            if n.kind() == "method_definition" {
                skip = n.child_by_field_name("name");
                if let Some(key) = skip {
                    self.walk(key, &caller, &context, depth + 1)?;
                }
            }
            caller = id;
            context.clear();
        }
        // Computed field keys and static initializers run at class definition.
        // Instance values run later, during construction: retain that evidence on
        // the class with a boundary region, never as a call from the enclosing fn.
        if n.kind() == "field_definition" {
            if let Some(key) = n.child_by_field_name("property") {
                self.walk(key, &caller, &context, depth + 1)?;
            }
            if let Some(value) = n.child_by_field_name("value") {
                let mut cursor = n.walk();
                let is_static = n.children(&mut cursor).any(|c| c.kind() == "static");
                if is_static {
                    self.walk(value, &caller, &context, depth + 1)?;
                } else if let Some(owner) = n
                    .parent()
                    .and_then(|body| body.parent())
                    .and_then(|class| self.classes.get(&class.id()))
                    .cloned()
                {
                    let region = self.region(value, "instance-initializer", &[], &owner);
                    self.walk(value, &owner, &region, depth + 1)?;
                    diag(
                        self.g,
                        Some(self.file.path.clone()),
                        "instance-initializer-boundary",
                        format!(
                            "Instance field at line {} is owned by the class; construction timing is not modeled",
                            n.start_position().row + 1
                        ),
                    );
                } else {
                    diag(
                        self.g,
                        Some(self.file.path.clone()),
                        "unsupported-field-owner",
                        "Instance field has no class owner; initializer omitted rather than attributed to enclosing code",
                    );
                }
            }
            return Ok(());
        }
        if matches!(n.kind(), "if_statement" | "ternary_expression") {
            if let Some(c) = n.child_by_field_name("condition") {
                self.walk(c, &caller, &context, depth + 1)?;
            }
            for (field, kind) in if n.kind() == "if_statement" {
                [("consequence", "if"), ("alternative", "else")]
            } else {
                [
                    ("consequence", "conditional-true"),
                    ("alternative", "conditional-false"),
                ]
            } {
                if let Some(c) = n.child_by_field_name(field) {
                    let region = self.region(c, kind, &context, &caller);
                    self.walk(c, &caller, &region, depth + 1)?;
                }
            }
            return Ok(());
        }
        if n.kind() == "binary_expression"
            && n.child_by_field_name("operator")
                .is_some_and(|o| matches!(self.text(o), "&&" | "||" | "??"))
        {
            if let Some(c) = n.child_by_field_name("left") {
                self.walk(c, &caller, &context, depth + 1)?;
            }
            if let Some(c) = n.child_by_field_name("right") {
                let r = self.region(c, "short-circuit", &context, &caller);
                self.walk(c, &caller, &r, depth + 1)?;
            }
            return Ok(());
        }
        if matches!(
            n.kind(),
            "for_statement" | "for_in_statement" | "while_statement" | "do_statement"
        ) {
            skip = n.child_by_field_name(if n.kind() == "for_in_statement" {
                "right"
            } else {
                "initializer"
            });
            if let Some(c) = skip {
                self.walk(c, &caller, &context, depth + 1)?;
            }
            context = self.region(n, "loop", &context, &caller);
        }
        if matches!(
            n.kind(),
            "try_statement"
                | "catch_clause"
                | "finally_clause"
                | "switch_statement"
                | "switch_case"
                | "switch_default"
        ) {
            context = self.region(n, n.kind(), &context, &caller);
        }
        if matches!(n.kind(), "call_expression" | "new_expression") {
            let callee = n
                .child_by_field_name(if n.kind() == "new_expression" {
                    "constructor"
                } else {
                    "function"
                })
                .map(unwrap);
            let token = callee.and_then(|c| {
                if c.kind() == "member_expression" {
                    c.child_by_field_name("property")
                } else {
                    Some(c)
                }
            });
            let candidates = self.symbols(token, false);
            if callee.is_some_and(|c| {
                c.kind() == "subscript_expression" || matches!(self.text(c), "eval" | "import")
            }) {
                diag(
                    self.g,
                    Some(self.file.path.clone()),
                    "dynamic-call",
                    format!(
                        "Dynamic call at line {} is a lexical site, not runtime target inference",
                        n.start_position().row + 1
                    ),
                );
            }
            let mut callbacks = Vec::new();
            if let Some(args) = n.child_by_field_name("arguments") {
                for arg in children(args).into_iter().map(unwrap) {
                    if let Some(id) = self.functions.get(&arg.id()) {
                        callbacks.push(id.clone());
                    } else if matches!(arg.kind(), "identifier" | "member_expression") {
                        callbacks.extend(self.symbols(
                            if arg.kind() == "member_expression" {
                                arg.child_by_field_name("property")
                            } else {
                                Some(arg)
                            },
                            false,
                        ));
                    }
                }
            }
            self.g.calls.push(CallSite {
                id: self
                    .native_id(n, NativeCandidateKind::Invocation)
                    .context("JavaScript graph call lacks native occurrence identity")?,
                caller: caller.clone(),
                callee_text: callee
                    .map(|v| self.text(v).to_owned())
                    .unwrap_or_else(|| "<unknown>".into()),
                path: self.file.path.clone(),
                range: range(n),
                target: None,
                candidate_symbols: candidates,
                resolution: Resolution::Unresolved,
                ordinal: 0,
                regions: context.clone(),
                callback_arguments: callbacks,
                provenance: self.provenance(self.doc.is_some()),
            });
        }
        for child in children(n) {
            if skip.is_none_or(|s| s.id() != child.id()) {
                self.walk(child, &caller, &context, depth + 1)?;
            }
        }
        Ok(())
    }
}
