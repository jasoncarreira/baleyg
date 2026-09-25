//! Java syntax only. No class loading, build tools, annotation processing or dispatch resolution.
use crate::model::v1::{self, Key, Kind, Language, Signature, Text, UInt};
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
// The class-catalog's in-memory fixture has no admitted source-set snapshot.
// Production indexing always uses extract_with_identity with captured context.
#[cfg(test)]
pub(crate) fn extract(g: &mut Graph, file: &SourceFile, cancel: &CancelFlag) -> Result<()> {
    extract_with_identity(g, file, cancel, &file.hash, "standalone")
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
    parser.set_language(&tree_sitter_java::LANGUAGE.into())?;
    let mut cancelled = |_: &tree_sitter::ParseState| cancel.load(Ordering::Relaxed);
    let tree = parser.parse_with_options(
        &mut |offset, _| &file.text.as_bytes()[offset..],
        None,
        Some(tree_sitter::ParseOptions::new().progress_callback(&mut cancelled)),
    );
    ensure!(!cancel.load(Ordering::Relaxed), "indexing cancelled");
    let tree = tree.context("Java parser failed")?;
    if tree.root_node().has_error() {
        g.stats.parse_error_files += 1;
        g.diagnostics.push(Diagnostic {
            path: Some(file.path.clone()),
            code: "parse-error".into(),
            message: "Tree-sitter recovered from invalid Java; results may be incomplete".into(),
        });
    }
    g.diagnostics.push(Diagnostic { path: Some(file.path.clone()), code: "java-lexical-only".into(),
        message: "Java syntax only: types, overloads, virtual dispatch, imports, annotations/Lombok, reflection and generated methods are not resolved. Initializers remain class-owned; no implicit constructor calls are generated.".into() });
    let ids = JavaIds::new(tree.root_node(), file, revision, source_set)?;
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
        ids: &ids,
    }
    .walk(tree.root_node(), &module, &[], 0)
}
struct JavaIds {
    module: String,
    ids: HashMap<(usize, usize, &'static str), String>,
}

fn declaration(n: Node<'_>, bytes: &[u8]) -> Result<Option<Key>> {
    let container = matches!(
        n.kind(),
        "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration"
    );
    let method = matches!(
        n.kind(),
        "method_declaration" | "annotation_type_element_declaration"
    );
    let constructor = matches!(
        n.kind(),
        "constructor_declaration" | "compact_constructor_declaration"
    );
    let lambda = n.kind() == "lambda_expression";
    let anonymous = n.kind() == "class_body"
        && n.parent()
            .is_some_and(|p| matches!(p.kind(), "object_creation_expression" | "enum_constant"));
    if !container && !method && !constructor && !lambda && !anonymous {
        return Ok(None);
    }
    let name = if lambda || anonymous {
        None
    } else {
        let name = n
            .child_by_field_name("name")
            .context("Java declaration has no name")?;
        Some(
            Text::new(std::str::from_utf8(&bytes[name.byte_range()])?.to_owned())
                .context("invalid Java declaration name")?,
        )
    };
    let signature = if method || constructor {
        let mut parameter_types = Vec::new();
        let mut variadic = false;
        if let Some(parameters) = n.child_by_field_name("parameters") {
            let mut cursor = parameters.walk();
            for parameter in parameters.named_children(&mut cursor) {
                if !matches!(
                    parameter.kind(),
                    "formal_parameter" | "spread_parameter" | "receiver_parameter"
                ) {
                    continue;
                }
                // A spread parameter contains an ordinary type node and a separate ellipsis.
                // The exact type spelling is the measured source slice, not the parameter name.
                if let Some(ty) = parameter.child_by_field_name("type").or_else(|| {
                    let mut cursor = parameter.walk();
                    parameter
                        .named_children(&mut cursor)
                        .find(|c| c.kind() != "identifier" && c.kind() != "modifiers")
                }) {
                    parameter_types.push(
                        Text::new(std::str::from_utf8(&bytes[ty.byte_range()])?.to_owned())
                            .context("invalid Java parameter type")?,
                    );
                }
                variadic = parameter.kind() == "spread_parameter";
            }
        }
        let type_parameter_count = n
            .child_by_field_name("type_parameters")
            .map_or(0, |params| {
                let mut cursor = params.walk();
                params.named_children(&mut cursor).count() as u64
            });
        Some(Signature {
            parameter_types,
            type_parameter_count: UInt::new(type_parameter_count).unwrap(),
            variadic,
        })
    } else {
        None
    };
    Ok(Some(Key {
        kind: if container {
            Kind::Type
        } else if anonymous || lambda {
            Kind::AnonymousFunction
        } else if constructor {
            Kind::Constructor
        } else {
            Kind::Method
        },
        name,
        signature,
        ordinal: UInt::new(0).unwrap(),
    }))
}

impl JavaIds {
    fn new(root: Node<'_>, file: &SourceFile, revision: &str, source_set: &str) -> Result<Self> {
        let path = v1::Path::new(file.path.clone()).context("invalid Java path")?;
        let source_set = Text::new(source_set.to_owned()).context("invalid source set")?;
        let module_key = Key {
            kind: Kind::Module,
            name: None,
            signature: None,
            ordinal: UInt::new(0).unwrap(),
        };
        let module = identity::syntax_id(&source_set, &path, Language::Java, &[], &module_key)?
            .as_str()
            .to_owned();
        let mut declarations = Vec::new();
        fn collect(
            n: Node<'_>,
            bytes: &[u8],
            ancestors: Vec<usize>,
            out: &mut Vec<(usize, usize, Key, Vec<usize>)>,
        ) -> Result<()> {
            if matches!(
                n.kind(),
                "annotation" | "marker_annotation" | "annotation_argument_list" | "modifiers"
            ) {
                return Ok(());
            }
            let mut ancestors = ancestors;
            if let Some(key) = declaration(n, bytes)? {
                out.push((n.start_byte(), n.end_byte(), key, ancestors.clone()));
                ancestors.push(out.len() - 1);
            }
            let mut cursor = n.walk();
            for child in n.named_children(&mut cursor) {
                collect(child, bytes, ancestors.clone(), out)?;
            }
            Ok(())
        }
        collect(root, file.text.as_bytes(), vec![], &mut declarations)?;
        let mut ids = HashMap::new();
        let mut resolved = HashMap::<usize, Key>::new();
        let mut resolved_ids = HashMap::<usize, String>::new();
        let mut collisions = identity::CollisionRegistry::default();
        let max_depth = declarations.iter().map(|d| d.3.len()).max().unwrap_or(0);
        for depth in 0..=max_depth {
            let indexes: Vec<_> = (0..declarations.len())
                .filter(|&i| declarations[i].3.len() == depth)
                .collect();
            let entries: Vec<_> = indexes
                .iter()
                .map(|&i| {
                    let (start, end, key, parents) = &declarations[i];
                    let mut ancestors = vec![module_key.clone()];
                    ancestors.extend(parents.iter().map(|p| resolved[p].clone()));
                    (ancestors, key.clone(), *start as u64, *end as u64)
                })
                .collect();
            for (&i, ordinal) in indexes.iter().zip(identity::sibling_ordinals(&entries)?) {
                let (start, end, key, parents) = &declarations[i];
                let mut key = key.clone();
                key.ordinal = ordinal;
                let mut ancestors = vec![module_key.clone()];
                ancestors.extend(parents.iter().map(|p| resolved[p].clone()));
                let digest =
                    identity::syntax_digest(&source_set, &path, Language::Java, &ancestors, &key)?;
                let id = identity::syntax_id(&source_set, &path, Language::Java, &ancestors, &key)?;
                collisions.syntax(&id, digest.input)?;
                ids.insert((*start, *end, "syntax"), id.as_str().to_owned());
                resolved_ids.insert(i, id.as_str().to_owned());
                resolved.insert(i, key);
            }
        }
        let mut occurrences = Vec::new();
        fn visit(
            n: Node<'_>,
            decl: &[(usize, usize, Key, Vec<usize>)],
            resolved_ids: &HashMap<usize, String>,
            module: &str,
            owner: String,
            out: &mut Vec<(usize, usize, &'static str, String, OccurrenceKind)>,
        ) {
            if matches!(
                n.kind(),
                "annotation" | "marker_annotation" | "annotation_argument_list" | "modifiers"
            ) {
                return;
            }
            let owner = decl
                .iter()
                .enumerate()
                .find(|(_, d)| d.0 == n.start_byte() && d.1 == n.end_byte())
                .and_then(|(i, _)| resolved_ids.get(&i))
                .cloned()
                .unwrap_or(owner);
            let _ = module;
            let control = matches!(
                n.kind(),
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
            );
            if control {
                out.push((
                    n.start_byte(),
                    n.end_byte(),
                    "region",
                    owner.clone(),
                    OccurrenceKind::Control,
                ));
            }
            if matches!(
                n.kind(),
                "method_invocation"
                    | "object_creation_expression"
                    | "explicit_constructor_invocation"
            ) {
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
                visit(child, decl, resolved_ids, module, owner.clone(), out);
            }
        }
        visit(
            root,
            &declarations,
            &resolved_ids,
            &module,
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
        let revision = Text::new(revision.to_owned()).context("invalid Java revision")?;
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
    let source = std::str::from_utf8(&document.bytes).context("invalid Java source bytes")?;
    let file = SourceFile {
        path: document.key.path.as_str().to_owned(),
        hash: document.content_hash.clone(),
        language: "java".into(),
        text: source.to_owned(),
    };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_java::LANGUAGE.into())?;
    let tree = parser.parse(source, None).context("Java parser failed")?;
    let ids = JavaIds::new(
        tree.root_node(),
        &file,
        revision,
        document.key.source_set_id.as_str(),
    )?;
    fn heritage(n: Node<'_>, document: &mut crate::indexer::CapturedDocument) {
        if matches!(
            n.kind(),
            "class_declaration" | "interface_declaration" | "record_declaration"
        ) && let (Some(name), Some(base)) = (
            n.child_by_field_name("name"),
            n.child_by_field_name("superclass"),
        ) {
            let mut cursor = base.walk();
            if let Some(ty) = base.named_children(&mut cursor).next() {
                let class_node_id = document
                    .syntax
                    .iter()
                    .find(|candidate| {
                        candidate.start_byte == n.start_byte()
                            && candidate.end_byte == n.end_byte()
                            && candidate.kind == n.kind()
                    })
                    .map(|candidate| candidate.id);
                if let Some(class_node_id) = class_node_id {
                    document
                        .heritage
                        .push(crate::indexer::CapturedHeritageWitness {
                            class_node_id,
                            owner_id: class_node_id,
                            subclass_name_start: name.start_byte(),
                            subclass_name_end: name.end_byte(),
                            base_start: ty.start_byte(),
                            base_end: ty.end_byte(),
                            base_bytes: document.bytes[ty.byte_range()].to_vec(),
                        });
                }
            }
        }
        let mut cursor = n.walk();
        for child in n.named_children(&mut cursor) {
            heritage(child, document);
        }
    }
    heritage(tree.root_node(), document);
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
            && witness.node_kind == "method_invocation"
            && let Some(node) = find_node(tree.root_node(), witness.start_byte, witness.end_byte)
            && let Some(name) = measured_member_name(node, &document.bytes)
        {
            witness.token_start_byte = name.0;
            witness.token_end_byte = name.1;
            witness.token_bytes = document.bytes[name.0..name.1].to_vec();
            witness.name_bytes = witness.token_bytes.clone();
            witness.spelling = Some(name.2);
            witness.verified_member_token = true;
        }
    }
    Ok(())
}

fn find_node(n: Node<'_>, start: usize, end: usize) -> Option<Node<'_>> {
    if n.start_byte() == start && n.end_byte() == end && n.kind() == "method_invocation" {
        return Some(n);
    }
    let mut cursor = n.walk();
    for child in n.named_children(&mut cursor) {
        if child.start_byte() <= start
            && end <= child.end_byte()
            && let Some(found) = find_node(child, start, end)
        {
            return Some(found);
        }
    }
    None
}

fn measured_member_name(n: Node<'_>, bytes: &[u8]) -> Option<(usize, usize, String)> {
    // A method invocation's own `name` field is a measured identifier token.
    // Neither the receiver expression nor an inferred target participates.
    n.child_by_field_name("object")?;
    let name = n.child_by_field_name("name")?;
    if name.kind() != "identifier" || name.end_byte() > bytes.len() {
        return None;
    }
    let raw = std::str::from_utf8(&bytes[name.byte_range()]).ok()?;
    let spelling = identity::lookup_key(Language::Java, raw).ok()?;
    Some((name.start_byte(), name.end_byte(), spelling))
}

struct Extractor<'a> {
    g: &'a mut Graph,
    file: &'a SourceFile,
    cancel: &'a CancelFlag,
    visits: usize,
    ids: &'a JavaIds,
}
impl Extractor<'_> {
    fn text(&self, n: Node<'_>) -> &str {
        &self.file.text[n.byte_range()]
    }
    fn id(&self, n: Node<'_>, prefix: &str) -> String {
        self.ids
            .ids
            .get(&(n.start_byte(), n.end_byte(), prefix))
            .cloned()
            .expect("measured Java candidate identity")
    }
    fn walk(&mut self, n: Node<'_>, owner: &str, regions: &[String], depth: usize) -> Result<()> {
        ensure!(!self.cancel.load(Ordering::Relaxed), "indexing cancelled");
        ensure!(
            depth < 512 && self.visits < 1_000_000,
            "Java extraction nesting/work budget exceeded (512 levels / 1000000 nodes)"
        );
        self.visits += 1;
        if matches!(
            n.kind(),
            "annotation" | "marker_annotation" | "annotation_argument_list" | "modifiers"
        ) {
            return Ok(());
        }
        let callable = matches!(
            n.kind(),
            "method_declaration"
                | "constructor_declaration"
                | "compact_constructor_declaration"
                | "lambda_expression"
                | "annotation_type_element_declaration"
        );
        let anonymous = n.kind() == "class_body"
            && n.parent().is_some_and(|p| {
                matches!(p.kind(), "object_creation_expression" | "enum_constant")
            });
        let container = anonymous
            || matches!(
                n.kind(),
                "class_declaration"
                    | "interface_declaration"
                    | "enum_declaration"
                    | "record_declaration"
                    | "annotation_type_declaration"
            );
        let mut owner = owner.to_owned();
        let mut regions = regions.to_vec();
        if callable || container {
            let name = n
                .child_by_field_name("name")
                .map(|v| self.text(v).to_owned())
                .unwrap_or_else(|| {
                    format!(
                        "<{}@{}:{}>",
                        if anonymous { "anonymous" } else { "lambda" },
                        n.start_position().row + 1,
                        n.start_position().column + 1
                    )
                });
            let id = self.id(n, "syntax");
            self.g.nodes.push(Symbol {
                id: id.clone(),
                name,
                kind: if container {
                    SymbolKind::Class
                } else if n.kind() == "lambda_expression" {
                    SymbolKind::Function
                } else {
                    SymbolKind::Method
                },
                path: self.file.path.clone(),
                range: range(n),
                parent: Some(owner),
                accessor: false,
                provenance: provenance(),
            });
            owner = id;
            regions.clear();
            // Parameters, annotation defaults and method annotations are not invocation behavior.
            if callable {
                if let Some(body) = n.child_by_field_name("body") {
                    self.walk(body, &owner, &regions, depth + 1)?;
                }
                return Ok(());
            }
        }
        if matches!(
            n.kind(),
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
        if matches!(
            n.kind(),
            "method_invocation" | "object_creation_expression" | "explicit_constructor_invocation"
        ) {
            let callee = if n.kind() == "object_creation_expression" {
                format!(
                    "new {}",
                    n.child_by_field_name("type")
                        .map(|v| self.text(v))
                        .unwrap_or("?")
                )
            } else {
                let name = n
                    .child_by_field_name("name")
                    .or_else(|| n.child_by_field_name("constructor"))
                    .map(|v| self.text(v))
                    .unwrap_or("?");
                n.child_by_field_name("object")
                    .map(|v| format!("{}.{name}", self.text(v)))
                    .unwrap_or_else(|| name.into())
            };
            let mut callbacks = Vec::new();
            if let Some(args) = n.child_by_field_name("arguments") {
                let mut cursor = args.walk();
                for mut arg in args.named_children(&mut cursor) {
                    while arg.kind() == "parenthesized_expression" && arg.named_child_count() == 1 {
                        arg = arg.named_child(0).unwrap();
                    }
                    if arg.kind() == "lambda_expression" {
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
        let mut cursor = n.walk();
        for child in n.named_children(&mut cursor) {
            self.walk(child, &owner, &regions, depth + 1)?;
        }
        Ok(())
    }
}
