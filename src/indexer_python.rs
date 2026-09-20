//! Python syntax only. Never imports modules or evaluates definitions.
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
    Extractor { g, file, cancel }.walk(tree.root_node(), &module, false, &[], 0)
}
struct Extractor<'a> {
    g: &'a mut Graph,
    file: &'a SourceFile,
    cancel: &'a CancelFlag,
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
