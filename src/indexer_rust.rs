//! Syntax-only Rust adapter. Never invokes workspace tools or expands macros.
//! Names and containers are lexical hints, not type, trait, cfg or module resolution.
use crate::model::v1::{self, Key, Kind, Language, Text, UInt};
use crate::model::*;
use crate::semantic_identity::{self as identity, OccurrenceKind};
use anyhow::{Context, Result, ensure};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use tree_sitter::Node;

fn provenance() -> Provenance {
    Provenance {
        source: "tree-sitter".into(),
        semantic: SemanticState::Unavailable,
    }
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
struct RustIds {
    module: String,
    ids: HashMap<(usize, usize, &'static str), String>,
    owners: HashMap<(usize, usize, &'static str), String>,
    paths: HashMap<String, Vec<String>>,
}

fn declaration(n: Node<'_>, bytes: &[u8]) -> Result<Option<Key>> {
    let kind = match n.kind() {
        "function_item" => {
            if n.parent().is_some_and(|p| {
                p.kind() == "declaration_list"
                    && p.parent()
                        .is_some_and(|p| matches!(p.kind(), "impl_item" | "trait_item"))
            }) {
                Kind::Method
            } else {
                Kind::Function
            }
        }
        "closure_expression" => Kind::AnonymousFunction,
        "impl_item" | "trait_item" | "struct_item" | "enum_item" | "union_item" | "mod_item" => {
            Kind::Type
        }
        _ => return Ok(None),
    };
    let name = if n.kind() == "impl_item" {
        let ty = n.child_by_field_name("type").context("impl has no type")?;
        let tr = n
            .child_by_field_name("trait")
            .map(|t| format!("{} for ", String::from_utf8_lossy(&bytes[t.byte_range()])))
            .unwrap_or_default();
        Some(
            Text::new(format!(
                "impl {tr}{}",
                String::from_utf8_lossy(&bytes[ty.byte_range()])
            ))
            .context("invalid Rust impl name")?,
        )
    } else {
        n.child_by_field_name("name")
            .map(|v| {
                Text::new(std::str::from_utf8(&bytes[v.byte_range()])?.to_owned())
                    .context("invalid Rust name")
            })
            .transpose()?
    };
    Ok(Some(Key {
        kind,
        name,
        signature: None,
        ordinal: UInt::new(0).unwrap(),
    }))
}
fn region(n: Node<'_>) -> bool {
    matches!(
        n.kind(),
        "if_expression"
            | "match_arm"
            | "loop_expression"
            | "while_expression"
            | "for_expression"
            | "async_block"
            | "unsafe_block"
    )
}
fn skipped(n: Node<'_>) -> bool {
    matches!(
        n.kind(),
        "macro_invocation"
            | "macro_definition"
            | "token_tree"
            | "attribute_item"
            | "inner_attribute_item"
    )
}
impl RustIds {
    fn new(root: Node<'_>, file: &SourceFile, revision: &str, source_set: &str) -> Result<Self> {
        let path = v1::Path::new(file.path.clone()).context("invalid Rust path")?;
        let source_set = Text::new(source_set.to_owned()).context("invalid source set")?;
        let module_key = Key {
            kind: Kind::Module,
            name: None,
            signature: None,
            ordinal: UInt::new(0).unwrap(),
        };
        let module = identity::syntax_id(&source_set, &path, Language::Rust, &[], &module_key)?
            .as_str()
            .to_owned();
        let mut decl = Vec::<(usize, usize, Key, Vec<usize>)>::new();
        fn collect(
            n: Node<'_>,
            bytes: &[u8],
            parents: Vec<usize>,
            out: &mut Vec<(usize, usize, Key, Vec<usize>)>,
        ) -> Result<()> {
            if skipped(n) {
                return Ok(());
            }
            let mut parents = parents;
            if let Some(key) = declaration(n, bytes)? {
                out.push((n.start_byte(), n.end_byte(), key, parents.clone()));
                parents.push(out.len() - 1);
            }
            let mut cursor = n.walk();
            for child in n.named_children(&mut cursor) {
                collect(child, bytes, parents.clone(), out)?;
            }
            Ok(())
        }
        collect(root, file.text.as_bytes(), vec![], &mut decl)?;
        let mut ids = HashMap::new();
        let mut keys = HashMap::<usize, Key>::new();
        let mut resolved = HashMap::<usize, String>::new();
        let mut paths = HashMap::from([(module.clone(), vec![module.clone()])]);
        let mut collisions = identity::CollisionRegistry::default();
        for depth in 0..=decl.iter().map(|d| d.3.len()).max().unwrap_or(0) {
            let indexes: Vec<_> = (0..decl.len())
                .filter(|&i| decl[i].3.len() == depth)
                .collect();
            let entries: Vec<_> = indexes
                .iter()
                .map(|&i| {
                    let (start, end, key, parents) = &decl[i];
                    let ancestors = std::iter::once(module_key.clone())
                        .chain(parents.iter().map(|p| keys[p].clone()))
                        .collect();
                    (ancestors, key.clone(), *start as u64, *end as u64)
                })
                .collect();
            for (&i, ordinal) in indexes.iter().zip(identity::sibling_ordinals(&entries)?) {
                let (start, end, key, parents) = &decl[i];
                let mut key = key.clone();
                key.ordinal = ordinal;
                let ancestors: Vec<_> = std::iter::once(module_key.clone())
                    .chain(parents.iter().map(|p| keys[p].clone()))
                    .collect();
                let digest =
                    identity::syntax_digest(&source_set, &path, Language::Rust, &ancestors, &key)?;
                let id = identity::syntax_id(&source_set, &path, Language::Rust, &ancestors, &key)?;
                collisions.syntax(&id, digest.input)?;
                let id = id.as_str().to_owned();
                ids.insert((*start, *end, "syntax"), id.clone());
                paths.insert(
                    id.clone(),
                    std::iter::once(module.clone())
                        .chain(parents.iter().map(|p| resolved[p].clone()))
                        .chain(std::iter::once(id.clone()))
                        .collect(),
                );
                resolved.insert(i, id);
                keys.insert(i, key);
            }
        }
        let mut occ = Vec::<(usize, usize, &'static str, String, OccurrenceKind)>::new();
        fn visit(
            n: Node<'_>,
            decl: &[(usize, usize, Key, Vec<usize>)],
            resolved: &HashMap<usize, String>,
            owner: String,
            out: &mut Vec<(usize, usize, &'static str, String, OccurrenceKind)>,
        ) {
            if skipped(n) {
                return;
            }
            let owner = decl
                .iter()
                .enumerate()
                .find(|(_, d)| d.0 == n.start_byte() && d.1 == n.end_byte())
                .and_then(|(i, _)| resolved.get(&i))
                .cloned()
                .unwrap_or(owner);
            if region(n) {
                out.push((
                    n.start_byte(),
                    n.end_byte(),
                    "region",
                    owner.clone(),
                    OccurrenceKind::Control,
                ));
            }
            if matches!(n.kind(), "call_expression" | "method_call_expression") {
                out.push((
                    n.start_byte(),
                    n.end_byte(),
                    "call",
                    owner.clone(),
                    OccurrenceKind::Call,
                ));
            }
            let mut cursor = n.walk();
            for child in n.named_children(&mut cursor) {
                visit(child, decl, resolved, owner.clone(), out);
            }
        }
        visit(root, &decl, &resolved, module.clone(), &mut occ);
        let entries: Vec<_> = occ
            .iter()
            .map(|(start, end, _, owner, kind)| {
                (
                    v1::SyntaxId::new(owner.clone()).unwrap(),
                    *kind,
                    *start as u64,
                    *end as u64,
                )
            })
            .collect();
        let revision = Text::new(revision.to_owned()).context("invalid revision")?;
        let mut owners = HashMap::new();
        for ((start, end, prefix, owner, kind), ordinal) in occ
            .into_iter()
            .zip(identity::occurrence_ordinals(&entries)?)
        {
            owners.insert((start, end, prefix), owner.clone());
            let owner = v1::SyntaxId::new(owner).context("invalid owner ID")?;
            let digest = identity::occurrence_digest(&revision, &owner, kind, ordinal)?;
            let id = identity::occurrence_id(&revision, &owner, kind, ordinal)?;
            collisions.occurrence(&id, digest.input)?;
            ids.insert((start, end, prefix), id.as_str().to_owned());
        }
        Ok(Self {
            module,
            ids,
            owners,
            paths,
        })
    }
}

pub(crate) fn identify_document(
    document: &mut crate::indexer::CapturedDocument,
    revision: &str,
) -> Result<()> {
    use crate::indexer::NativeCandidateKind;
    let source = std::str::from_utf8(&document.bytes)?;
    let file = SourceFile {
        path: document.key.path.as_str().to_owned(),
        hash: document.content_hash.clone(),
        language: "rust".into(),
        text: source.into(),
    };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;
    let tree = parser.parse(source, None).context("Rust parser failed")?;
    let ids = RustIds::new(
        tree.root_node(),
        &file,
        revision,
        document.key.source_set_id.as_str(),
    )?;
    document.native_candidates.retain(|w| {
        !matches!(
            w.candidate_kind,
            NativeCandidateKind::ControlRegion | NativeCandidateKind::Invocation
        ) || ids.ids.contains_key(&(
            w.start_byte,
            w.end_byte,
            if w.candidate_kind == NativeCandidateKind::Invocation {
                "call"
            } else {
                "region"
            },
        ))
    });
    for witness in &mut document.native_candidates {
        let prefix = match witness.candidate_kind {
            NativeCandidateKind::Declaration => "syntax",
            NativeCandidateKind::Invocation => "call",
            NativeCandidateKind::ControlRegion => "region",
            NativeCandidateKind::Occurrence => continue,
        };
        witness.stable_id = ids
            .ids
            .get(&(witness.start_byte, witness.end_byte, prefix))
            .cloned();
        witness.ancestor_ids = if prefix == "syntax" {
            witness
                .stable_id
                .as_ref()
                .and_then(|id| ids.paths.get(id))
                .map(|p| p[..p.len() - 1].to_vec())
                .unwrap_or_else(|| vec![ids.module.clone()])
        } else {
            ids.owners
                .get(&(witness.start_byte, witness.end_byte, prefix))
                .and_then(|owner| ids.paths.get(owner))
                .cloned()
                .unwrap_or_else(|| vec![ids.module.clone()])
        };
        if witness.candidate_kind == NativeCandidateKind::Invocation
            && let Some(node) =
                find_invocation(tree.root_node(), witness.start_byte, witness.end_byte)
            && let Some((start, end, spelling)) = measured_member_name(node, &document.bytes)
        {
            witness.token_start_byte = start;
            witness.token_end_byte = end;
            witness.token_bytes = document.bytes[start..end].to_vec();
            witness.name_bytes = witness.token_bytes.clone();
            witness.spelling = Some(spelling);
            witness.verified_member_token = true;
        }
    }
    Ok(())
}
fn find_invocation(n: Node<'_>, start: usize, end: usize) -> Option<Node<'_>> {
    if matches!(n.kind(), "call_expression" | "method_call_expression")
        && n.start_byte() == start
        && n.end_byte() == end
    {
        return Some(n);
    }
    let mut cursor = n.walk();
    for child in n.named_children(&mut cursor) {
        if child.start_byte() <= start
            && end <= child.end_byte()
            && let Some(found) = find_invocation(child, start, end)
        {
            return Some(found);
        }
    }
    None
}
fn measured_member_name(n: Node<'_>, bytes: &[u8]) -> Option<(usize, usize, String)> {
    if n.kind() != "call_expression" {
        return None;
    }
    let function = n.child_by_field_name("function")?;
    if function.kind() != "field_expression" {
        return None;
    }
    function.child_by_field_name("value")?;
    let name = function.child_by_field_name("field")?;
    if name.kind() != "field_identifier" || name.end_byte() > bytes.len() {
        return None;
    }
    let raw = std::str::from_utf8(&bytes[name.byte_range()]).ok()?;
    let spelling = raw.strip_prefix("r#").unwrap_or(raw).to_owned();
    Some((name.start_byte(), name.end_byte(), spelling))
}
#[cfg(test)]
pub(crate) fn extract(g: &mut Graph, file: &SourceFile, cancel: &CancelFlag) -> Result<()> {
    extract_with_limits(
        g,
        file,
        cancel,
        usize::MAX,
        usize::MAX,
        &file.hash,
        "standalone",
    )
}
/// External browsing bounds the extraction walk, not tree-sitter parsing time.
/// Kept separate so workspace indexing retains its existing behavior.
pub(crate) fn extract_external(
    g: &mut Graph,
    file: &SourceFile,
    cancel: &CancelFlag,
) -> Result<()> {
    extract_with_limits(g, file, cancel, 50_000, 8_000, &file.hash, "standalone")
}
pub(crate) fn extract_with_identity(
    g: &mut Graph,
    file: &SourceFile,
    cancel: &CancelFlag,
    revision: &str,
    source_set: &str,
) -> Result<()> {
    extract_with_limits(
        g,
        file,
        cancel,
        usize::MAX,
        usize::MAX,
        revision,
        source_set,
    )
}
fn extract_with_limits(
    g: &mut Graph,
    file: &SourceFile,
    cancel: &CancelFlag,
    max_visits: usize,
    max_records: usize,
    revision: &str,
    source_set: &str,
) -> Result<()> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;
    let mut cancelled = |_: &tree_sitter::ParseState| cancel.load(Ordering::Relaxed);
    let tree = parser.parse_with_options(
        &mut |offset, _| &file.text.as_bytes()[offset..],
        None,
        Some(tree_sitter::ParseOptions::new().progress_callback(&mut cancelled)),
    );
    ensure!(!cancel.load(Ordering::Relaxed), "indexing cancelled");
    let tree = tree.context("Rust parser failed")?;
    if tree.root_node().has_error() {
        g.stats.parse_error_files += 1;
        g.diagnostics.push(Diagnostic {
            path: Some(file.path.clone()),
            code: "parse-error".into(),
            message: "Tree-sitter recovered from invalid Rust; results may be incomplete".into(),
        });
    }
    g.diagnostics.push(Diagnostic { path: Some(file.path.clone()), code: "rust-lexical-only".into(),
        message: "Rust syntax only: attributes/cfg are not evaluated, modules and names are not resolved, macros are opaque; async bodies describe possible execution when polled".into() });
    let ids = RustIds::new(tree.root_node(), file, revision, source_set)?;
    let module = ids.module.clone();
    g.nodes.push(Symbol {
        id: module.clone(),
        name: file.path.clone(),
        kind: SymbolKind::Module,
        path: file.path.clone(),
        range: range(tree.root_node()),
        parent: None,
        accessor: false,
        provenance: provenance(),
    });
    Extractor {
        g,
        file,
        cancel,
        visits: 0,
        max_visits,
        max_records,
        ids: &ids,
    }
    .walk(tree.root_node(), &module, &[], 0)
}
struct Extractor<'a> {
    g: &'a mut Graph,
    file: &'a SourceFile,
    cancel: &'a CancelFlag,
    visits: usize,
    max_visits: usize,
    max_records: usize,
    ids: &'a RustIds,
}
impl Extractor<'_> {
    fn text(&self, n: Node<'_>) -> &str {
        &self.file.text[n.byte_range()]
    }
    fn id(&self, n: Node<'_>, prefix: &'static str) -> String {
        self.ids
            .ids
            .get(&(n.start_byte(), n.end_byte(), prefix))
            .cloned()
            .expect("measured Rust candidate identity")
    }
    fn walk(&mut self, n: Node<'_>, owner: &str, regions: &[String], depth: usize) -> Result<()> {
        ensure!(!self.cancel.load(Ordering::Relaxed), "indexing cancelled");
        ensure!(depth < 512, "Rust nesting exceeds 512 levels");
        // Each node adds at most one symbol, call, or region. Check before processing
        // even inert nodes so both traversal and record allocation remain bounded.
        if self.visits >= self.max_visits
            || self.g.nodes.len() + self.g.calls.len() + self.g.regions.len() >= self.max_records
        {
            self.g.diagnostics.push(Diagnostic {
                path: Some(self.file.path.clone()),
                code: "rust-extraction-truncated".into(),
                message: "Rust extraction truncated at 50000 AST visits or 8000 records; definitions are partial candidates.".into(),
            });
            anyhow::bail!("Rust extraction budget exceeded");
        }
        self.visits += 1;
        // Token trees and attributes can contain call-shaped text, never runtime calls.
        if matches!(
            n.kind(),
            "macro_invocation"
                | "macro_definition"
                | "token_tree"
                | "attribute_item"
                | "inner_attribute_item"
        ) {
            return Ok(());
        }
        let callable = matches!(n.kind(), "function_item" | "closure_expression");
        let container = matches!(
            n.kind(),
            "impl_item" | "trait_item" | "struct_item" | "enum_item" | "union_item" | "mod_item"
        );
        let mut owner = owner.to_owned();
        let mut regions = regions.to_vec();
        if callable || container {
            let name = n
                .child_by_field_name("name")
                .map(|v| self.text(v).to_owned())
                .unwrap_or_else(|| {
                    if n.kind() == "impl_item" {
                        let ty = n
                            .child_by_field_name("type")
                            .map(|v| self.text(v))
                            .unwrap_or("?");
                        let tr = n
                            .child_by_field_name("trait")
                            .map(|v| format!("{} for ", self.text(v)))
                            .unwrap_or_default();
                        format!("impl {tr}{ty}")
                    } else {
                        format!(
                            "<closure@{}:{}>",
                            n.start_position().row + 1,
                            n.start_position().column + 1
                        )
                    }
                });
            let method = n.kind() == "function_item"
                && n.parent()
                    .and_then(|p| p.parent())
                    .is_some_and(|p| matches!(p.kind(), "impl_item" | "trait_item"));
            let kind = if method {
                SymbolKind::Method
            } else if callable {
                SymbolKind::Function
            } else if n.kind() == "mod_item" {
                SymbolKind::Module
            } else {
                SymbolKind::Class
            };
            let id = self.id(n, "syntax");
            self.g.nodes.push(Symbol {
                id: id.clone(),
                name,
                kind,
                path: self.file.path.clone(),
                range: range(n),
                parent: Some(owner),
                accessor: false,
                provenance: provenance(),
            });
            owner = id;
            // A nested callable does not inherit the execution context of its definition.
            regions.clear();
        }
        if matches!(
            n.kind(),
            "if_expression"
                | "match_arm"
                | "loop_expression"
                | "while_expression"
                | "for_expression"
                | "async_block"
                | "unsafe_block"
        ) {
            let id = self.id(n, "region");
            self.g.regions.push(ControlRegion {
                id: id.clone(),
                kind: n.kind().into(),
                label: self.text(n).chars().take(140).collect(),
                parent: regions.last().cloned(),
                owner: owner.clone(),
                path: self.file.path.clone(),
                range: range(n),
            });
            regions.push(id);
        }
        if matches!(n.kind(), "call_expression" | "method_call_expression") {
            let callee = n
                .child_by_field_name(if n.kind() == "method_call_expression" {
                    "method"
                } else {
                    "function"
                })
                .map(|v| self.text(v).to_owned())
                .unwrap_or_default();
            let mut callbacks = Vec::new();
            if let Some(args) = n.child_by_field_name("arguments") {
                let mut cur = args.walk();
                for mut arg in args.named_children(&mut cur) {
                    while arg.kind() == "parenthesized_expression" && arg.named_child_count() == 1 {
                        arg = arg.named_child(0).unwrap();
                    }
                    if arg.kind() == "closure_expression" {
                        callbacks.push(self.id(arg, "syntax"));
                    }
                }
            }
            self.g.calls.push(CallSite {
                id: self.id(n, "call"),
                caller: owner.clone(),
                callee_text: callee,
                path: self.file.path.clone(),
                range: range(n),
                target: None,
                candidate_symbols: vec![],
                resolution: Resolution::Unresolved,
                ordinal: 0,
                regions: regions.clone(),
                callback_arguments: callbacks,
                provenance: provenance(),
            });
        }
        let mut cur = n.walk();
        for child in n.named_children(&mut cur) {
            self.walk(child, &owner, &regions, depth + 1)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod extraction_budget_tests {
    use super::*;
    use std::sync::{Arc, atomic::AtomicBool};
    fn file(text: String) -> SourceFile {
        SourceFile {
            path: "fixture.rs".into(),
            hash: "fixture".into(),
            language: "rust".into(),
            text,
        }
    }
    #[test]
    fn external_records_are_bounded_but_workspace_extraction_is_unchanged() {
        let file = file((0..8100).map(|i| format!("fn f{i}() {{}}\n")).collect());
        let cancel = Arc::new(AtomicBool::new(false));
        let mut limited = Graph::default();
        assert!(extract_external(&mut limited, &file, &cancel).is_err());
        assert_eq!(
            limited.nodes.len() + limited.calls.len() + limited.regions.len(),
            8000
        );
        assert!(
            limited
                .diagnostics
                .iter()
                .any(|d| d.code == "rust-extraction-truncated")
        );
        let mut full = Graph::default();
        extract(&mut full, &file, &cancel).unwrap();
        assert_eq!(full.nodes.len(), 8101);
    }
    #[test]
    fn external_visits_are_bounded_even_when_records_are_sparse() {
        let file = file(format!("fn sparse() {{ {} }}", ";".repeat(60_000)));
        let mut graph = Graph::default();
        assert!(extract_external(&mut graph, &file, &Arc::new(AtomicBool::new(false))).is_err());
        assert_eq!(graph.nodes.len(), 2);
        assert!(
            graph
                .diagnostics
                .iter()
                .any(|d| d.code == "rust-extraction-truncated")
        );
    }
}
