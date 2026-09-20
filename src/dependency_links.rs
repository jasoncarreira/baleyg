//! Presentation-only links to local dependency declarations.
//!
//! A path match is a syntax candidate, never Rust name/type/dispatch resolution.
//! This module reads only the cached workspace file, not dependency source bodies.
use crate::{
    behavior::{Participant, SequenceStep, SequenceView},
    dependencies::{Catalog, CatalogSymbol},
    model::{Resolution, SourceFile},
};
use std::collections::{HashMap, HashSet};
use tree_sitter::Node;

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_VISITS: usize = 50_000;
const MAX_DEPTH: usize = 128;
const MAX_PARTICIPANTS: usize = 20;

type Span = (usize, usize);
struct Import {
    path: Vec<String>,
    scope: Span,
}
#[derive(Default)]
struct Syntax {
    calls: HashMap<Span, Vec<String>>,
    imports: HashMap<String, Import>,
    blocked: HashSet<String>,
    glob_scopes: Vec<Span>,
}

/// Add terminal candidate participants without changing measured call evidence.
/// Failure, ambiguity, stale revisions and resource limits retain the original view.
pub fn annotate(view: &mut SequenceView, file: &SourceFile, catalog: &Catalog) {
    if file.language != "rust"
        || view.revision != catalog.workspace_revision
        || view.seed.path != file.path
        || file.text.len() > MAX_BYTES
    {
        return;
    }
    let Some(syntax) = syntax(file) else { return };
    let mut types: HashMap<String, Option<&CatalogSymbol>> = HashMap::new();
    let packages: HashMap<_, _> = catalog
        .packages
        .iter()
        .map(|p| (p.id.as_str(), p))
        .collect();
    // A Cargo alias is usable only when it identifies one package. In
    // particular, a second version with no matching declaration still makes
    // that prefix ambiguous; catalog incompleteness is not disambiguation.
    let mut prefixes: HashMap<String, HashSet<&str>> = HashMap::new();
    for package in &catalog.packages {
        for alias in &package.aliases {
            let alias = alias.replace('-', "_");
            if identifier(&alias) {
                prefixes.entry(alias).or_default().insert(&package.id);
            }
        }
        if package.source == "stdlib" && matches!(package.name.as_str(), "std" | "core" | "alloc") {
            prefixes
                .entry(package.name.clone())
                .or_default()
                .insert(&package.id);
        }
    }
    for symbol in &catalog.symbols {
        if !matches!(symbol.kind.as_str(), "struct" | "enum" | "trait") {
            continue;
        }
        let Some(package) = packages.get(symbol.package_id.as_str()) else {
            continue;
        };
        let Some((crate_name, tail)) = symbol.qualified_name.split_once("::") else {
            continue;
        };
        // Crate prefixes come from declarations, not the Cargo package name: a
        // package may declare a differently named [lib]. Only known direct
        // aliases or configured stdlib names grant a candidate call prefix.
        // A transitive crate's matching name alone never grants one.
        let mut aliases = package.aliases.clone();
        if package.source == "stdlib"
            && package.name == crate_name
            && matches!(crate_name, "std" | "core" | "alloc")
        {
            aliases.push(crate_name.into());
        }
        let mut paths = HashSet::new();
        for alias in aliases {
            let alias = alias.replace('-', "_");
            if prefixes
                .get(&alias)
                .is_some_and(|owners| owners.len() == 1 && owners.contains(package.id.as_str()))
            {
                paths.insert(format!("{alias}::{tail}"));
            }
        }
        for path in paths {
            types
                .entry(path)
                .and_modify(|entry| *entry = None)
                .or_insert(Some(symbol));
        }
    }

    let mut pending: Vec<&mut SequenceStep> = view.steps.iter_mut().collect();
    let mut visits = 0;
    while let Some(step) = pending.pop() {
        visits += 1;
        if visits > MAX_VISITS {
            break;
        }
        // Groups, including collapsed fluent chains, keep measured children.
        pending.extend(step.children.iter_mut());
        pending.extend(step.alternate.iter_mut());
        if step.call_id.is_none()
            || step.path != file.path
            || step.resolution != Some(Resolution::Unresolved)
        {
            continue;
        }
        let span = (step.range.start_byte, step.range.end_byte);
        let Some(path) = syntax.calls.get(&span) else {
            continue;
        };
        let Some(owner) = candidate_owner(path, span, &syntax) else {
            continue;
        };
        let Some(Some(symbol)) = types.get(&owner) else {
            continue;
        };
        let id = format!("dep-type:{}", symbol.id);
        if !view.participants.iter().any(|p| p.id == id) {
            if view.participants.len() >= MAX_PARTICIPANTS {
                continue;
            }
            let package = packages[symbol.package_id.as_str()];
            view.participants.push(Participant {
                id: id.clone(),
                label: symbol.name.clone(),
                kind: "externalCandidate".into(),
                identification: format!(
                    "Syntax candidate only; not resolved dispatch. Package {} {} ({}); source {} [{}]. Terminal dependency declaration; no behavior expansion.",
                    package.name, package.version, package.source, symbol.path, symbol.source_ref,
                ),
            });
        }
        step.target = Some(id);
    }
}

fn candidate_owner(path: &[String], span: Span, syntax: &Syntax) -> Option<String> {
    // A glob can shadow any name, but only in its lexical scope. A test
    // module's `use super::*` must not suppress candidates in sibling code.
    if syntax
        .glob_scopes
        .iter()
        .any(|scope| span.0 >= scope.0 && span.1 <= scope.1)
    {
        return None;
    }
    let (callee, owner) = path.split_last()?;
    if callee.is_empty() || owner.is_empty() {
        return None;
    }
    let first = &owner[0];
    if workspace_prefix(first) || syntax.blocked.contains(first) {
        return None;
    }
    let mut expanded = owner.to_vec();
    if let Some(import) = syntax.imports.get(first) {
        if span.0 < import.scope.0 || span.1 > import.scope.1 {
            return None;
        }
        expanded = import.path.clone();
        expanded.extend_from_slice(&owner[1..]);
        let root = expanded.first()?;
        if workspace_prefix(root) || syntax.blocked.contains(root) {
            return None;
        }
        // Chained imports would require name resolution; do not attempt it.
        if root != first && syntax.imports.contains_key(root) {
            return None;
        }
    }
    // Never globally match a bare type name, even when the catalog has only one.
    (expanded.len() >= 2).then(|| expanded.join("::"))
}

fn workspace_prefix(name: &str) -> bool {
    matches!(name, "crate" | "self" | "super" | "Self")
}
fn identifier(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn path(node: Node<'_>, text: &str) -> Option<Vec<String>> {
    if !matches!(
        node.kind(),
        "identifier" | "scoped_identifier" | "crate" | "self" | "super"
    ) {
        return None;
    }
    let value = text[node.byte_range()].trim().trim_start_matches("::");
    let parts: Vec<_> = value.split("::").map(str::trim).collect();
    if parts.is_empty() || !parts.iter().all(|p| identifier(p)) {
        return None;
    }
    Some(parts.into_iter().map(str::to_owned).collect())
}

fn syntax(file: &SourceFile) -> Option<Syntax> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(&file.text, None)?;
    let root = tree.root_node();
    // Incomplete syntax can conceal a binding, so do not annotate recovered trees.
    if root.has_error() {
        return None;
    }
    let mut result = Syntax::default();
    let mut stack = vec![(root, 0, false, (0, file.text.len()))];
    let mut visits = 0;
    while let Some((node, depth, binding, scope)) = stack.pop() {
        visits += 1;
        if visits > MAX_VISITS || depth > MAX_DEPTH {
            return None;
        }
        let kind = node.kind();
        if matches!(kind, "macro_invocation" | "macro_definition" | "token_tree") {
            // Macro token trees are opaque, not parsed as apparent bindings.
            continue;
        }
        if kind == "use_declaration" {
            imports(
                node.child_by_field_name("argument")?,
                scope,
                &file.text,
                &mut result,
                &mut visits,
            )?;
            continue;
        }
        if binding
            && matches!(
                kind,
                "identifier" | "type_identifier" | "shorthand_field_identifier"
            )
        {
            result
                .blocked
                .insert(file.text[node.byte_range()].to_owned());
        }
        if matches!(
            kind,
            "mod_item"
                | "struct_item"
                | "enum_item"
                | "union_item"
                | "trait_item"
                | "type_item"
                | "associated_type"
                | "function_item"
                | "function_signature_item"
                | "const_item"
                | "static_item"
                | "type_parameter"
                | "const_parameter"
                | "extern_crate_declaration"
        ) {
            for field in ["name", "alias"] {
                if let Some(name) = node.child_by_field_name(field) {
                    result
                        .blocked
                        .insert(file.text[name.byte_range()].to_owned());
                }
            }
        }
        if kind == "call_expression"
            && let Some(mut function) = node.child_by_field_name("function")
        {
            // Strip callee turbofish only, never infer a method receiver type.
            if function.kind() == "generic_function" {
                function = function.child_by_field_name("function")?;
            }
            if let Some(path) = path(function, &file.text) {
                result
                    .calls
                    .insert((node.start_byte(), node.end_byte()), path);
            }
        }
        let scope = if matches!(kind, "block" | "declaration_list") {
            (node.start_byte(), node.end_byte())
        } else {
            scope
        };
        let pattern = node.child_by_field_name("pattern");
        let condition = (kind == "match_pattern")
            .then(|| node.child_by_field_name("condition"))
            .flatten();
        let type_node = node.child_by_field_name("type");
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if stack.len() + visits >= MAX_VISITS {
                return None;
            }
            stack.push((
                child,
                depth + 1,
                // Types, qualified pattern paths and match guards are name
                // references, not bindings. In particular, `Err(e) if
                // e.kind() == std::io::ErrorKind::NotFound` does not bind std.
                (binding
                    && condition != Some(child)
                    && type_node != Some(child)
                    && !matches!(kind, "scoped_identifier" | "scoped_type_identifier"))
                    || pattern == Some(child)
                    || kind == "closure_parameters",
                scope,
            ));
        }
    }
    Some(result)
}

fn imports(
    node: Node<'_>,
    scope: Span,
    text: &str,
    syntax: &mut Syntax,
    visits: &mut usize,
) -> Option<()> {
    let mut stack = vec![(node, Vec::<String>::new(), 0)];
    while let Some((node, prefix, depth)) = stack.pop() {
        *visits += 1;
        if *visits > MAX_VISITS || depth > MAX_DEPTH {
            return None;
        }
        match node.kind() {
            "scoped_use_list" => {
                let mut prefix = prefix;
                if let Some(root) = node.child_by_field_name("path") {
                    prefix.extend(path(root, text)?);
                }
                stack.push((node.child_by_field_name("list")?, prefix, depth + 1));
            }
            "use_list" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if stack.len() + *visits >= MAX_VISITS {
                        return None;
                    }
                    stack.push((child, prefix.clone(), depth + 1));
                }
            }
            "use_as_clause" => {
                let alias = node.child_by_field_name("alias")?;
                syntax.blocked.insert(text[alias.byte_range()].to_owned());
            }
            // A glob may introduce any name, including a crate prefix, in
            // this lexical scope. Keep sibling scopes available for matching.
            "use_wildcard" => syntax.glob_scopes.push(scope),
            _ => {
                let mut full = prefix;
                full.extend(path(node, text)?);
                // `self` within a group imports the prefix module itself.
                if full.last().is_some_and(|s| s == "self") {
                    full.pop();
                }
                let name = full.last()?.clone();
                match syntax.imports.entry(name) {
                    std::collections::hash_map::Entry::Occupied(entry) => {
                        syntax.blocked.insert(entry.key().clone());
                    }
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(Import { path: full, scope });
                    }
                }
            }
        }
    }
    Some(())
}
