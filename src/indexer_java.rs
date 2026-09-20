//! Java syntax only. No class loading, build tools, annotation processing or dispatch resolution.
use crate::model::*;
use anyhow::{Context, Result, ensure};
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
pub(crate) fn extract(g: &mut Graph, file: &SourceFile, cancel: &CancelFlag) -> Result<()> {
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
    let module = format!("module:{}", file.path);
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
    }
    .walk(tree.root_node(), &module, &[], 0)
}
struct Extractor<'a> {
    g: &'a mut Graph,
    file: &'a SourceFile,
    cancel: &'a CancelFlag,
    visits: usize,
}
impl Extractor<'_> {
    fn text(&self, n: Node<'_>) -> &str {
        &self.file.text[n.byte_range()]
    }
    fn id(&self, n: Node<'_>, prefix: &str) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}",
            prefix,
            self.file.path,
            self.file.hash,
            n.start_byte(),
            n.end_byte(),
            n.kind()
        )
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
