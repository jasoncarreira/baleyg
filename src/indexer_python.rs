//! Python syntax only. Never imports modules or evaluates definitions.
use crate::model::v1::{self, Key, Kind, Language, Text, UInt};
use crate::model::*;
use crate::semantic_identity::{self as identity, OccurrenceKind};
use anyhow::{Context, Result, ensure};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use tree_sitter::Node;

struct PythonIds {
    module: String,
    ids: HashMap<(usize, usize, &'static str), String>,
}

fn declaration(n: Node<'_>, bytes: &[u8]) -> Result<Option<Key>> {
    let kind = match n.kind() {
        "class_definition" => Kind::Type,
        "function_definition" => Kind::Function,
        "lambda" => Kind::AnonymousFunction,
        _ => return Ok(None),
    };
    let name = n
        .child_by_field_name("name")
        .map(|name| {
            Text::new(std::str::from_utf8(&bytes[name.byte_range()])?.to_owned())
                .context("invalid Python declaration name")
        })
        .transpose()?;
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
            | "generator_expression"
    )
}

impl PythonIds {
    fn new(root: Node<'_>, file: &SourceFile, revision: &str, source_set: &str) -> Result<Self> {
        let path = v1::Path::new(file.path.clone()).context("invalid Python path")?;
        let source_set = Text::new(source_set.to_owned()).context("invalid source set")?;
        let module_key = Key {
            kind: Kind::Module,
            name: None,
            signature: None,
            ordinal: UInt::new(0).unwrap(),
        };
        let module = identity::syntax_id(&source_set, &path, Language::Python, &[], &module_key)?
            .as_str()
            .to_owned();
        let mut declarations = Vec::<(usize, usize, Key, Vec<usize>)>::new();
        fn collect(
            n: Node<'_>,
            bytes: &[u8],
            parents: Vec<usize>,
            out: &mut Vec<(usize, usize, Key, Vec<usize>)>,
        ) -> Result<()> {
            if matches!(n.kind(), "type" | "type_parameter" | "type_alias_statement") {
                return Ok(());
            }
            let mut parents = parents;
            if let Some(key) = declaration(n, bytes)? {
                let mut key = key;
                if key.kind == Kind::Function
                    && parents.last().is_some_and(|&p| out[p].2.kind == Kind::Type)
                {
                    key.kind = Kind::Method;
                }
                out.push((n.start_byte(), n.end_byte(), key, parents.clone()));
                parents.push(out.len() - 1);
            }
            let mut cursor = n.walk();
            for child in n.named_children(&mut cursor) {
                collect(child, bytes, parents.clone(), out)?;
            }
            Ok(())
        }
        collect(root, file.text.as_bytes(), vec![], &mut declarations)?;
        let mut ids = HashMap::new();
        let mut keys = HashMap::<usize, Key>::new();
        let mut owner_ids = HashMap::<usize, String>::new();
        let mut collisions = identity::CollisionRegistry::default();
        for depth in 0..=declarations.iter().map(|d| d.3.len()).max().unwrap_or(0) {
            let indexes: Vec<_> = (0..declarations.len())
                .filter(|&i| declarations[i].3.len() == depth)
                .collect();
            let entries: Vec<_> = indexes
                .iter()
                .map(|&i| {
                    let (start, end, key, parents) = &declarations[i];
                    let ancestors: Vec<_> = parents.iter().map(|p| keys[p].clone()).collect();
                    (ancestors, key.clone(), *start as u64, *end as u64)
                })
                .collect();
            for (&i, ordinal) in indexes.iter().zip(identity::sibling_ordinals(&entries)?) {
                let (start, end, original, parents) = &declarations[i];
                let mut key = original.clone();
                key.ordinal = ordinal;
                let ancestors: Vec<_> = parents.iter().map(|p| keys[p].clone()).collect();
                let digest = identity::syntax_digest(
                    &source_set,
                    &path,
                    Language::Python,
                    &ancestors,
                    &key,
                )?;
                let id =
                    identity::syntax_id(&source_set, &path, Language::Python, &ancestors, &key)?;
                collisions.syntax(&id, digest.input)?;
                ids.insert((*start, *end, "syntax"), id.as_str().to_owned());
                owner_ids.insert(i, id.as_str().to_owned());
                keys.insert(i, key);
            }
        }
        let mut occurrences = Vec::<(usize, usize, &'static str, String, OccurrenceKind)>::new();
        fn visit(
            n: Node<'_>,
            declarations: &[(usize, usize, Key, Vec<usize>)],
            owner_ids: &HashMap<usize, String>,
            owner: String,
            out: &mut Vec<(usize, usize, &'static str, String, OccurrenceKind)>,
        ) {
            if matches!(n.kind(), "type" | "type_parameter" | "type_alias_statement") {
                return;
            }
            let owner = declarations
                .iter()
                .enumerate()
                .find(|(_, d)| d.0 == n.start_byte() && d.1 == n.end_byte())
                .and_then(|(i, _)| owner_ids.get(&i))
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
            if n.kind() == "call" {
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
                visit(child, declarations, owner_ids, owner.clone(), out);
            }
        }
        visit(
            root,
            &declarations,
            &owner_ids,
            module.clone(),
            &mut occurrences,
        );
        let entries: Vec<_> = occurrences
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
        let revision = Text::new(revision.to_owned()).context("invalid Python revision")?;
        for ((start, end, prefix, owner, kind), ordinal) in occurrences
            .into_iter()
            .zip(identity::occurrence_ordinals(&entries)?)
        {
            let owner = v1::SyntaxId::new(owner).unwrap();
            let digest = identity::occurrence_digest(&revision, &owner, kind, ordinal)?;
            let id = identity::occurrence_id(&revision, &owner, kind, ordinal)?;
            collisions.occurrence(&id, digest.input)?;
            ids.insert((start, end, prefix), id.as_str().to_owned());
        }
        Ok(Self { module, ids })
    }
}

pub(crate) fn identify_document(
    document: &mut crate::indexer::CapturedDocument,
    revision: &str,
) -> Result<()> {
    use crate::indexer::NativeCandidateKind;
    let source = std::str::from_utf8(&document.bytes).context("invalid Python source bytes")?;
    let file = SourceFile {
        path: document.key.path.as_str().to_owned(),
        hash: document.content_hash.clone(),
        language: "python".into(),
        text: source.to_owned(),
    };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_python::LANGUAGE.into())?;
    let tree = parser.parse(source, None).context("Python parser failed")?;
    let ids = PythonIds::new(
        tree.root_node(),
        &file,
        revision,
        document.key.source_set_id.as_str(),
    )?;
    document.native_candidates.retain(|w| {
        w.candidate_kind != NativeCandidateKind::ControlRegion
            || ids.ids.contains_key(&(w.start_byte, w.end_byte, "region"))
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
        let mut ancestors: Vec<_> = ids
            .ids
            .iter()
            .filter(|((start, end, kind), _)| {
                *kind == "syntax"
                    && *start <= witness.start_byte
                    && witness.end_byte <= *end
                    && (*start, *end) != (witness.start_byte, witness.end_byte)
            })
            .map(|((start, end, _), id)| (*start, *end, id.clone()))
            .collect();
        ancestors.sort_by_key(|(start, end, _)| (*start, usize::MAX - *end));
        witness.ancestor_ids = std::iter::once(ids.module.clone())
            .chain(ancestors.into_iter().map(|(_, _, id)| id))
            .collect();
        if witness.candidate_kind == NativeCandidateKind::Invocation
            && witness.node_kind == "call"
            && let Some(node) = find_call(tree.root_node(), witness.start_byte, witness.end_byte)
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

fn find_call(n: Node<'_>, start: usize, end: usize) -> Option<Node<'_>> {
    if n.kind() == "call" && n.start_byte() == start && n.end_byte() == end {
        return Some(n);
    }
    let mut cursor = n.walk();
    for child in n.named_children(&mut cursor) {
        if child.start_byte() <= start
            && end <= child.end_byte()
            && let Some(found) = find_call(child, start, end)
        {
            return Some(found);
        }
    }
    None
}

fn measured_member_name(n: Node<'_>, bytes: &[u8]) -> Option<(usize, usize, String)> {
    let function = n.child_by_field_name("function")?;
    if function.kind() != "attribute" {
        return None;
    }
    function.child_by_field_name("object")?;
    let name = function.child_by_field_name("attribute")?;
    if name.kind() != "identifier" || name.end_byte() > bytes.len() {
        return None;
    }
    let raw = std::str::from_utf8(&bytes[name.byte_range()]).ok()?;
    Some((name.start_byte(), name.end_byte(), raw.to_owned()))
}

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
pub(crate) fn extract_with_identity(
    g: &mut Graph,
    file: &SourceFile,
    cancel: &CancelFlag,
    revision: &str,
    source_set: &str,
) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "indexing cancelled");
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_python::LANGUAGE.into())?;
    let mut cancelled = |_: &tree_sitter::ParseState| cancel.load(Ordering::Relaxed);
    let tree = parser.parse_with_options(
        &mut |offset, _| &file.text.as_bytes()[offset..],
        None,
        Some(tree_sitter::ParseOptions::new().progress_callback(&mut cancelled)),
    );
    ensure!(!cancel.load(Ordering::Relaxed), "indexing cancelled");
    let tree = tree.context("Python parser failed")?;
    if tree.root_node().has_error() {
        g.stats.parse_error_files += 1;
        g.diagnostics.push(Diagnostic {
            path: Some(file.path.clone()),
            code: "parse-error".into(),
            message: "Tree-sitter recovered invalid Python; syntax results may be incomplete."
                .into(),
        });
    }
    g.diagnostics.push(Diagnostic { path: Some(file.path.clone()), code: "python-lexical-only".into(),
        message: "Python syntax only: imports, types, descriptors and dispatch unresolved. Definition defaults/decorators belong to enclosing scope; annotation expressions are omitted from the call graph (evaluation depends on scope, Python version and future imports). Comprehension/try/with/match behavior is bounded explicitly.".into() });
    let ids = PythonIds::new(tree.root_node(), file, revision, source_set)?;
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
        ids: &ids,
    }
    .walk(tree.root_node(), &module, false, &[], 0)
}
struct Extractor<'a> {
    g: &'a mut Graph,
    file: &'a SourceFile,
    cancel: &'a CancelFlag,
    ids: &'a PythonIds,
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
            .expect("measured Python candidate identity")
    }
    fn walk(
        &mut self,
        n: Node<'_>,
        owner: &str,
        class_scope: bool,
        regions: &[String],
        depth: usize,
    ) -> Result<()> {
        ensure!(!self.cancel.load(Ordering::Relaxed), "indexing cancelled");
        ensure!(depth < 512, "Python nesting exceeds 512 levels");
        // Annotation evaluation differs across Python versions and future imports.
        // Never record it as an invocation-body call.
        if matches!(n.kind(), "type" | "type_parameter" | "type_alias_statement") {
            return Ok(());
        }
        if matches!(
            n.kind(),
            "function_definition" | "lambda" | "class_definition"
        ) {
            let class = n.kind() == "class_definition";
            let id = self.id(n, "syntax");
            let name = n
                .child_by_field_name("name")
                .map(|c| self.text(c).to_owned())
                .unwrap_or_else(|| {
                    format!(
                        "<lambda@{}:{}>",
                        n.start_position().row + 1,
                        n.start_position().column + 1
                    )
                });
            self.g.nodes.push(Symbol {
                id: id.clone(),
                name,
                kind: if class {
                    SymbolKind::Class
                } else if class_scope && n.kind() != "lambda" {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                },
                path: self.file.path.clone(),
                range: range(n),
                parent: Some(owner.into()),
                accessor: false,
                provenance: provenance(),
            });
            let body = n.child_by_field_name("body");
            let mut cursor = n.walk();
            for child in n.named_children(&mut cursor) {
                if Some(child) == body {
                    self.walk(child, &id, class, &[], depth + 1)?;
                } else {
                    // Defaults and superclass expressions run in the defining scope.
                    self.walk(child, owner, class_scope, regions, depth + 1)?;
                }
            }
            return Ok(());
        }
        let mut regions = regions.to_vec();
        if matches!(
            n.kind(),
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
                | "generator_expression"
        ) {
            let id = self.id(n, "region");
            self.g.regions.push(ControlRegion {
                id: id.clone(),
                kind: n.kind().into(),
                label: self.text(n).chars().take(140).collect(),
                parent: regions.last().cloned(),
                owner: owner.into(),
                path: self.file.path.clone(),
                range: range(n),
            });
            regions.push(id);
        }
        if n.kind() == "call" {
            let callee = n
                .child_by_field_name("function")
                .map(|c| self.text(c).to_owned())
                .unwrap_or_default();
            let mut callbacks = vec![];
            if let Some(args) = n.child_by_field_name("arguments") {
                let mut cur = args.walk();
                for mut arg in args.named_children(&mut cur) {
                    if arg.kind() == "keyword_argument" {
                        arg = arg.child_by_field_name("value").unwrap_or(arg);
                    }
                    while arg.kind() == "parenthesized_expression" && arg.named_child_count() == 1 {
                        arg = arg.named_child(0).unwrap();
                    }
                    if arg.kind() == "lambda" {
                        callbacks.push(self.id(arg, "syntax"));
                    }
                }
            }
            self.g.calls.push(CallSite {
                id: self.id(n, "call"),
                caller: owner.into(),
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
        let mut cursor = n.walk();
        for child in n.named_children(&mut cursor) {
            self.walk(child, owner, class_scope, &regions, depth + 1)?;
        }
        Ok(())
    }
}
