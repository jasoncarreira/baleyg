//! Cached-source, declaration-only class projection. Links are scoped syntax candidates,
//! never compiler resolution. No source reads, body inference, or dependency loading.
use crate::model::{CancelFlag, SourceFile, SourceRange, Symbol, SymbolKind};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::Ordering,
};
use tree_sitter::Node;

const FILE_BYTES: usize = 2 * 1024 * 1024;
const TOTAL_BYTES: usize = 256 * 1024 * 1024;
const VISITS: usize = 100_000;
const DEPTH: usize = 64;
const FILE_CLASSES: usize = 1_000;
const CLASSES: usize = 20_000;
const MEMBERS: usize = 256;
const FILE_REFS: usize = 8_192;
const RECORDS: usize = 250_000;
const TEXT: usize = 2_048;
const OUTPUT_TEXT: usize = 64 * 1024 * 1024;
const REGISTRY_TEXT: usize = 32 * 1024 * 1024;
const CANDIDATE_TEXT: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClassMember {
    pub name: String,
    pub type_hint: Option<String>,
    pub symbol_id: Option<String>,
    pub path: String,
    pub range: SourceRange,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClassDefinition {
    pub symbol: Symbol,
    pub qualified_name: String,
    pub language: String,
    pub declaration_kind: String,
    pub fields: Vec<ClassMember>,
    pub methods: Vec<ClassMember>,
    pub truncated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClassRelation {
    pub id: String,
    pub owner: String,
    pub target: Option<String>,
    pub type_name: String,
    pub kind: String,
    pub path: String,
    pub range: SourceRange,
    pub candidate_ids: Vec<String>,
    pub match_kind: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub classes: Vec<ClassDefinition>,
    pub relations: Vec<ClassRelation>,
    pub warnings: Vec<String>,
    pub truncated: bool,
}
#[derive(Default)]
struct Scope {
    parent: Option<usize>,
    class_namespace: bool,
    // None is an explicit non-type/shadow binding. Multiple entries stay ambiguous.
    bindings: BTreeMap<String, Vec<Option<String>>>,
    blocked: BTreeSet<String>,
    wildcard: bool,
}
struct Pending {
    relation: ClassRelation,
    scope: usize,
    blocked: BTreeSet<String>,
    module: String,
    language: String,
}
struct Builder<'a> {
    catalog: Catalog,
    scopes: Vec<Scope>,
    pending: Vec<Pending>,
    symbols: BTreeMap<(&'a str, usize, usize), Vec<&'a Symbol>>,
    cancel: &'a CancelFlag,
    records: usize,
    text_bytes: usize,
    detail_text_bytes: usize,
    registry_complete: bool,
}
fn check(cancel: &CancelFlag) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "class catalog cancelled");
    Ok(())
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
fn class_node(n: Node<'_>) -> bool {
    matches!(
        n.kind(),
        "class_definition"
            | "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration"
    )
}
fn method_node(n: Node<'_>) -> bool {
    matches!(
        n.kind(),
        "function_definition"
            | "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration"
            | "annotation_type_element_declaration"
    )
}
fn declaration(mut n: Node<'_>) -> Node<'_> {
    if n.kind() == "decorated_definition"
        && let Some(d) = n.child_by_field_name("definition")
    {
        n = d;
    }
    n
}
fn join(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.into()
    } else {
        format!("{a}.{b}")
    }
}
fn module_name(file: &SourceFile) -> String {
    let path = file
        .path
        .strip_suffix(".py")
        .unwrap_or(&file.path)
        .replace('/', ".");
    path.strip_suffix(".__init__").unwrap_or(&path).to_owned()
}
fn dotted(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|p| {
            !p.is_empty()
                && p.chars().enumerate().all(|(i, c)| {
                    c == '_' || c == '$' || c.is_alphabetic() || (i > 0 && c.is_numeric())
                })
        })
}
impl Catalog {
    pub fn build(files: &[SourceFile], nodes: &[Symbol], cancel: &CancelFlag) -> Result<Self> {
        check(cancel)?;
        let mut b = Builder {
            catalog: Self::default(),
            scopes: vec![],
            pending: vec![],
            symbols: BTreeMap::new(),
            cancel,
            records: 0,
            text_bytes: 0,
            detail_text_bytes: 0,
            registry_complete: true,
        };
        // A hard input cap also bounds auxiliary lookup tables, independent of AST limits.
        if nodes.len() > 1_000_000 || files.len() > 100_000 {
            b.registry_limit("Class catalog input limit exceeded (100000 files / 1000000 symbols)");
            return Ok(b.catalog);
        }
        for symbol in nodes {
            check(cancel)?;
            if matches!(
                symbol.kind,
                SymbolKind::Class | SymbolKind::Method | SymbolKind::Function
            ) {
                b.symbols
                    .entry((&symbol.path, symbol.range.start_byte, symbol.range.end_byte))
                    .or_default()
                    .push(symbol);
            }
        }
        let mut total = 0usize;
        for file in files {
            check(cancel)?;
            if !matches!(file.language.as_str(), "java" | "python") {
                continue;
            }
            total = total.saturating_add(file.text.len());
            if total > TOTAL_BYTES {
                b.registry_limit("Class catalog source limit exceeded (256 MiB)");
                break;
            }
            if file.text.len() > FILE_BYTES {
                b.registry_limit(&format!(
                    "{}: class source exceeds 2 MiB; skipped",
                    file.path
                ));
                continue;
            }
            if b.catalog.classes.len() >= CLASSES {
                b.registry_limit("Class catalog declaration limit reached (20000)");
                break;
            }
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&if file.language == "java" {
                tree_sitter_java::LANGUAGE.into()
            } else {
                tree_sitter_python::LANGUAGE.into()
            })?;
            let mut cancelled = |_: &tree_sitter::ParseState| cancel.load(Ordering::Relaxed);
            let tree = parser.parse_with_options(
                &mut |offset, _| &file.text.as_bytes()[offset..],
                None,
                Some(tree_sitter::ParseOptions::new().progress_callback(&mut cancelled)),
            );
            check(cancel)?;
            let tree = tree.context("class catalog parser failed")?;
            if tree.root_node().has_error() {
                b.registry_limit(&format!(
                    "{}: recovered syntax; class catalog may be incomplete",
                    file.path
                ));
            }
            let scope = b.scopes.len();
            b.scopes.push(Scope::default());
            let before = b.catalog.classes.len();
            let mut ex = Extractor {
                b: &mut b,
                file,
                module: if file.language == "python" {
                    module_name(file)
                } else {
                    String::new()
                },
                visits: 0,
                classes: 0,
                refs: 0,
                exhausted: false,
                detail_visits: 0,
            };
            ex.module(tree.root_node(), scope)?;
            if ex.exhausted {
                for c in &mut ex.b.catalog.classes[before..] {
                    c.truncated = true;
                }
            }
        }
        b.resolve()?;
        b.catalog.classes.sort_by(|a, b| {
            (&a.symbol.path, a.symbol.range.start_byte, &a.symbol.id).cmp(&(
                &b.symbol.path,
                b.symbol.range.start_byte,
                &b.symbol.id,
            ))
        });
        b.catalog.relations.sort_by(|a, b| {
            (&a.path, a.range.start_byte, &a.id).cmp(&(&b.path, b.range.start_byte, &b.id))
        });
        b.catalog.relations.dedup_by(|a, b| a.id == b.id);
        if !b.catalog.classes.is_empty() {
            b.catalog.warnings.push("Links are scoped syntax candidates, not compiler resolution. Direct Java/Python declarations only; no function-body, conditional/anonymous class, generated member, wildcard-import, type-alias or dependency discovery.".into());
        }
        if !b.registry_complete {
            b.catalog.warnings.push(
                "Incomplete class declaration registry: candidate linking is disabled to avoid false unique matches."
                    .into(),
            );
        }
        check(cancel)?;
        Ok(b.catalog)
    }
}
impl Builder<'_> {
    fn registry_limit(&mut self, message: &str) {
        self.registry_complete = false;
        self.limit(message);
    }
    fn reserve_detail_text(&mut self, bytes: usize) -> bool {
        if self.detail_text_bytes.saturating_add(bytes) > OUTPUT_TEXT {
            self.detail_text_bytes = OUTPUT_TEXT;
            self.limit("Class detail text limit reached (64 MiB); declaration discovery continues");
            false
        } else {
            self.detail_text_bytes += bytes;
            true
        }
    }
    fn limit(&mut self, message: &str) {
        self.catalog.truncated = true;
        if self.catalog.warnings.len() < 100 && !self.catalog.warnings.iter().any(|s| s == message)
        {
            self.catalog
                .warnings
                .push(message.chars().take(TEXT).collect());
        }
    }
    fn symbol(&self, file: &SourceFile, n: Node<'_>, kind: SymbolKind) -> Option<Symbol> {
        let found = self
            .symbols
            .get(&(file.path.as_str(), n.start_byte(), n.end_byte()))?;
        let mut matches = found
            .iter()
            .filter(|s| s.kind == kind && s.range == range(n));
        let s = matches.next()?;
        if matches.next().is_some() || s.id.len() > 8192 || s.name.len() > TEXT {
            None
        } else {
            Some((*s).clone())
        }
    }
    fn reserve_text(&mut self, bytes: usize) -> bool {
        self.text_bytes = self.text_bytes.saturating_add(bytes);
        if self.text_bytes > REGISTRY_TEXT {
            self.registry_limit("Class declaration registry text limit reached (32 MiB)");
            false
        } else {
            true
        }
    }
    fn bind(&mut self, scope: usize, name: String, value: Option<String>) {
        if name.len() > TEXT
            || value.as_ref().is_some_and(|s| s.len() > TEXT)
            || !self.reserve_text(name.len() + value.as_ref().map_or(0, String::len))
        {
            self.scopes[scope].wildcard = true;
            self.registry_limit("Class binding text limit reached");
            return;
        }
        self.scopes[scope]
            .bindings
            .entry(name)
            .or_default()
            .push(value);
    }
    fn resolve(&mut self) -> Result<()> {
        let mut by_name: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
        for c in &self.catalog.classes {
            by_name
                .entry((c.language.clone(), c.qualified_name.clone()))
                .or_default()
                .push(c.symbol.id.clone());
        }
        let mut candidate_bytes = 0usize;
        let mut candidates_clipped = false;
        for p in &mut self.pending {
            check(self.cancel)?;
            if self.registry_complete {
                let name = &p.relation.type_name;
                let (head, tail) = name
                    .split_once('.')
                    .map(|(a, b)| (a, format!(".{b}")))
                    .unwrap_or((name.as_str(), String::new()));
                let mut scope = Some(p.scope);
                let mut names = vec![];
                let mut bound = p.blocked.contains(head);
                let mut unsafe_binding = bound;
                while !bound {
                    let Some(i) = scope else { break };
                    let s = &self.scopes[i];
                    if s.blocked.contains(head) || s.wildcard {
                        bound = true;
                        unsafe_binding = true;
                    } else if let Some(bindings) = s.bindings.get(head) {
                        bound = true;
                        for binding in bindings {
                            match binding {
                                Some(v) => names.push(format!("{v}{tail}")),
                                None => unsafe_binding = true,
                            }
                        }
                        // A rebinding is not a unique syntax candidate even if values happen to agree.
                        if bindings.len() > 1 {
                            unsafe_binding = true;
                        }
                    }
                    scope = s.parent;
                }
                if !bound && p.language == "java" {
                    names.push(join(&p.module, name));
                    if name.contains('.') {
                        names.push(name.clone());
                    }
                }
                let mut ids = BTreeSet::new();
                for name in names {
                    if let Some(found) = by_name.get(&(p.language.clone(), name)) {
                        for id in found {
                            ids.insert(id.clone());
                            if ids.len() > 32 {
                                break;
                            }
                        }
                    }
                    if ids.len() > 32 {
                        break;
                    }
                }
                if ids.len() > 32 {
                    candidates_clipped = true;
                }
                let ids = ids.into_iter().take(32).collect::<Vec<_>>();
                let bytes = ids.iter().map(String::len).sum::<usize>() * 2;
                if candidate_bytes.saturating_add(bytes) > CANDIDATE_TEXT {
                    candidates_clipped = true;
                    // Retain safe earlier matches. Remaining references stay terminal hints.
                    break;
                }
                candidate_bytes += bytes;
                p.relation.candidate_ids = ids;
                if p.relation.candidate_ids.len() == 1 && !unsafe_binding {
                    p.relation.target = p.relation.candidate_ids.first().cloned();
                    p.relation.match_kind = "syntaxCandidate".into();
                } else if !p.relation.candidate_ids.is_empty() {
                    p.relation.match_kind = "ambiguous".into();
                }
            }
        }
        if candidates_clipped {
            self.limit("Class candidate limit reached (32/reference or 32 MiB text); some candidate details omitted");
        }
        if !self.registry_complete {
            for p in &mut self.pending {
                check(self.cancel)?;
                p.relation.candidate_ids.clear();
                p.relation.target = None;
                p.relation.match_kind = "unmatched".into();
            }
        }
        self.catalog.relations = std::mem::take(&mut self.pending)
            .into_iter()
            .map(|p| p.relation)
            .collect();
        Ok(())
    }
}
struct Extractor<'a, 'b> {
    b: &'a mut Builder<'b>,
    file: &'a SourceFile,
    module: String,
    visits: usize,
    classes: usize,
    refs: usize,
    exhausted: bool,
    detail_visits: usize,
}
impl Extractor<'_, '_> {
    fn text(&self, n: Node<'_>) -> &str {
        &self.file.text[n.byte_range()]
    }
    fn tick(&mut self, depth: usize) -> Result<bool> {
        check(self.b.cancel)?;
        if self.exhausted {
            return Ok(false);
        }
        self.visits += 1;
        if depth >= DEPTH || self.visits > VISITS || self.b.text_bytes > REGISTRY_TEXT {
            self.exhausted = true;
            self.b.registry_limit(&format!(
                "{}: class declaration work limit reached (64 depth / 100000 visits)",
                self.file.path
            ));
            return Ok(false);
        }
        Ok(true)
    }
    fn details(&mut self, index: usize) -> Result<bool> {
        check(self.b.cancel)?;
        if self.b.records >= RECORDS
            || self.b.detail_text_bytes >= OUTPUT_TEXT
            || self.detail_visits >= VISITS
        {
            self.b.catalog.classes[index].truncated = true;
            self.b.limit("Class detail limit reached (250000 records / 64 MiB text / 100000 visits per file); declaration discovery continues");
            return Ok(false);
        }
        Ok(true)
    }
    fn detail_tick(&mut self, index: usize, depth: usize) -> Result<bool> {
        if !self.details(index)? {
            return Ok(false);
        }
        self.detail_visits += 1;
        if depth >= DEPTH {
            self.b.catalog.classes[index].truncated = true;
            self.b.limit(
                "Class type detail depth limit reached (64); declaration discovery continues",
            );
            return Ok(false);
        }
        Ok(true)
    }
    fn detail_children<'t>(
        &mut self,
        n: Node<'t>,
        index: usize,
        depth: usize,
    ) -> Result<Vec<Node<'t>>> {
        let mut out = vec![];
        let mut cursor = n.walk();
        for c in n.named_children(&mut cursor) {
            if !self.detail_tick(index, depth)? {
                break;
            }
            out.push(c);
        }
        Ok(out)
    }
    fn children<'t>(&mut self, n: Node<'t>, depth: usize) -> Result<Vec<Node<'t>>> {
        let mut out = vec![];
        let mut cursor = n.walk();
        for c in n.named_children(&mut cursor) {
            if !self.tick(depth)? {
                break;
            }
            out.push(c);
        }
        Ok(out)
    }
    fn module(&mut self, n: Node<'_>, scope: usize) -> Result<()> {
        let children = self.children(n, 0)?;
        if self.file.language == "java" {
            for c in &children {
                if c.kind() == "package_declaration" {
                    for d in self.children(*c, 1)? {
                        if matches!(d.kind(), "identifier" | "scoped_identifier") {
                            self.module = self.text(d).to_owned();
                        }
                    }
                }
            }
        }
        self.bind_body(&children, scope, &self.module.clone(), 0)?;
        for c in children {
            let c = declaration(c);
            if class_node(c) {
                self.class(c, scope, &self.module.clone(), 0)?;
            }
        }
        Ok(())
    }
    fn bind_body(
        &mut self,
        children: &[Node<'_>],
        scope: usize,
        prefix: &str,
        depth: usize,
    ) -> Result<()> {
        for &raw in children {
            if !self.tick(depth)? {
                break;
            }
            let n = declaration(raw);
            if class_node(n) {
                if let Some(name) = n.child_by_field_name("name") {
                    let name = self.text(name).to_owned();
                    self.b.bind(scope, name.clone(), Some(join(prefix, &name)));
                }
            } else if self.file.language == "java" && n.kind() == "import_declaration" {
                let text = self
                    .text(n)
                    .trim()
                    .trim_end_matches(';')
                    .trim_start_matches("import")
                    .trim();
                if let Some(imported) = text.strip_prefix("static ").map(str::to_owned) {
                    if imported.ends_with(".*") {
                        self.b.scopes[scope].wildcard = true;
                    } else {
                        self.b.bind(
                            scope,
                            imported.rsplit('.').next().unwrap_or("").into(),
                            None,
                        );
                    }
                    continue;
                }
                if text.ends_with(".*") {
                    continue;
                }
                let text = text.to_owned();
                if dotted(&text) {
                    self.b
                        .bind(scope, text.rsplit('.').next().unwrap().into(), Some(text));
                }
            } else if self.file.language == "python" {
                match n.kind() {
                    "import_statement" | "import_from_statement" => {
                        self.import(n, scope, depth + 1)?
                    }
                    "function_definition" => {
                        if let Some(name) = n.child_by_field_name("name") {
                            self.b.bind(scope, self.text(name).into(), None);
                        }
                    }
                    "expression_statement" => {
                        for c in self.children(n, depth + 1)? {
                            if matches!(c.kind(), "assignment" | "augmented_assignment")
                                && let Some(left) = c.child_by_field_name("left")
                            {
                                self.shadow_pattern(left, scope, depth + 2)?;
                            }
                        }
                    }
                    "type_alias_statement" => {
                        if let Some(left) = n.child_by_field_name("left") {
                            self.shadow_pattern(left, scope, depth + 1)?;
                        }
                    }
                    // Conditional binding/import execution is not modeled. Disable matching in
                    // this scope rather than guess which branch shadows a class or module.
                    "if_statement" | "try_statement" | "for_statement" | "while_statement"
                    | "with_statement" | "match_statement" => {
                        self.b.scopes[scope].wildcard = true;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
    fn shadow_pattern(&mut self, n: Node<'_>, scope: usize, depth: usize) -> Result<()> {
        if !self.tick(depth)? {
            return Ok(());
        }
        if n.kind() == "identifier" {
            self.b.bind(scope, self.text(n).into(), None);
        } else if matches!(
            n.kind(),
            "pattern_list" | "tuple_pattern" | "list_pattern" | "type"
        ) {
            for c in self.children(n, depth + 1)? {
                self.shadow_pattern(c, scope, depth + 1)?;
            }
        }
        Ok(())
    }
    fn import(&mut self, n: Node<'_>, scope: usize, depth: usize) -> Result<()> {
        let module = n.child_by_field_name("module_name");
        let mut prefix = module.map(|m| self.text(m).to_owned()).unwrap_or_default();
        if prefix.starts_with('.') {
            let dots = prefix.bytes().take_while(|b| *b == b'.').count();
            let mut parts: Vec<_> = self.module.split('.').collect();
            if !self.file.path.ends_with("/__init__.py") && self.file.path != "__init__.py" {
                parts.pop();
            }
            if dots > parts.len() {
                prefix = String::new();
                self.b.scopes[scope].wildcard = true;
            } else {
                for _ in 1..dots {
                    parts.pop();
                }
                prefix = join(&parts.join("."), &prefix[dots..]);
                prefix = prefix.trim_end_matches('.').into();
            }
        }
        for c in self.children(n, depth)? {
            if Some(c) == module {
                continue;
            }
            if c.kind() == "wildcard_import" {
                self.b.scopes[scope].wildcard = true;
                continue;
            }
            let (name, alias) = if c.kind() == "aliased_import" {
                let Some(name) = c.child_by_field_name("name") else {
                    continue;
                };
                (
                    self.text(name).to_owned(),
                    c.child_by_field_name("alias")
                        .map(|a| self.text(a).to_owned()),
                )
            } else if c.kind() == "dotted_name" {
                (self.text(c).into(), None)
            } else {
                continue;
            };
            if module.is_some() {
                self.b.bind(
                    scope,
                    alias.unwrap_or_else(|| name.clone()),
                    Some(join(&prefix, &name)),
                );
            } else if let Some(alias) = alias {
                self.b.bind(scope, alias, Some(name));
            } else {
                let first = name.split('.').next().unwrap().to_owned();
                self.b.bind(scope, first.clone(), Some(first));
            }
        }
        Ok(())
    }
    fn parameter_children<'t>(
        &mut self,
        n: Node<'t>,
        depth: usize,
        detail: Option<usize>,
    ) -> Result<Option<Vec<Node<'t>>>> {
        let children = if let Some(index) = detail {
            self.detail_children(n, index, depth)?
        } else {
            self.children(n, depth)?
        };
        if children.len() != n.named_child_count() {
            return Ok(None);
        }
        Ok(Some(children))
    }
    fn type_parameters(
        &mut self,
        n: Node<'_>,
        depth: usize,
        detail: Option<usize>,
    ) -> Result<Option<BTreeSet<String>>> {
        let mut names = BTreeSet::new();
        if let Some(params) = n.child_by_field_name("type_parameters") {
            let Some(parameters) = self.parameter_children(params, depth, detail)? else {
                return Ok(None);
            };
            for p in parameters {
                if names.len() >= 256 {
                    if let Some(index) = detail {
                        self.b.catalog.classes[index].truncated = true;
                        self.b.limit("Method type parameter detail limit reached (256); method references omitted");
                    } else {
                        self.exhausted = true;
                        self.b
                            .registry_limit("Class type parameter limit reached (256/declaration)");
                    }
                    return Ok(None);
                }
                // Both grammars put the declared parameter first; never treat bounds as names.
                let mut node = p;
                let mut found = false;
                for _ in 0..8 {
                    if matches!(node.kind(), "type_identifier" | "identifier") {
                        names.insert(self.text(node).into());
                        found = true;
                        break;
                    }
                    let Some(children) = self.parameter_children(node, depth + 1, detail)? else {
                        return Ok(None);
                    };
                    let Some(first) = children
                        .into_iter()
                        .find(|c| !matches!(c.kind(), "annotation" | "marker_annotation"))
                    else {
                        break;
                    };
                    node = first;
                }
                if !found {
                    if let Some(index) = detail {
                        self.b.catalog.classes[index].truncated = true;
                        self.b.limit(
                            "Method type parameter syntax unsupported; method references omitted",
                        );
                    } else {
                        self.exhausted = true;
                        self.b.registry_limit("Class type parameter syntax unsupported; declaration registry incomplete");
                    }
                    return Ok(None);
                }
            }
        }
        Ok(Some(names))
    }
    fn class(&mut self, n: Node<'_>, parent: usize, prefix: &str, depth: usize) -> Result<()> {
        if !self.tick(depth)? {
            return Ok(());
        }
        if self.classes >= FILE_CLASSES || self.b.catalog.classes.len() >= CLASSES {
            self.exhausted = true;
            self.b.registry_limit(&format!(
                "{}: class declaration limit reached (1000/file, 20000/catalog)",
                self.file.path
            ));
            return Ok(());
        }
        let Some(symbol) = self.b.symbol(self.file, n, SymbolKind::Class) else {
            self.b.registry_limit(&format!(
                "{}: declaration has no unique measured class symbol; skipped",
                self.file.path
            ));
            return Ok(());
        };
        let qualified = join(prefix, &symbol.name);
        if !self
            .b
            .reserve_text(symbol.id.len() + symbol.name.len() + symbol.path.len() + qualified.len())
        {
            return Ok(());
        }
        if qualified.len() > TEXT {
            self.b
                .registry_limit("Class qualified name exceeds 2048 bytes");
            return Ok(());
        }
        let Some(mut blocked) = self.type_parameters(n, depth + 1, None)? else {
            return Ok(());
        };
        // A Python class body is not a closure over its enclosing class namespace.
        // Bases, in contrast, are expressions in the original defining namespace.
        let mut lexical_parent = parent;
        if self.file.language == "python" {
            while self.b.scopes[lexical_parent].class_namespace {
                // PEP 695 annotation-scope parameters do remain visible in nested
                // classes, even though ordinary outer class bindings do not.
                blocked.extend(self.b.scopes[lexical_parent].blocked.iter().cloned());
                let Some(next) = self.b.scopes[lexical_parent].parent else {
                    break;
                };
                lexical_parent = next;
            }
        }
        if blocked.len() > 256
            || !self
                .b
                .reserve_text(blocked.iter().map(String::len).sum::<usize>() * 2)
        {
            self.exhausted = true;
            self.b
                .registry_limit("Class inherited type parameter limit reached (256/scope)");
            return Ok(());
        }
        let base_scope = self.b.scopes.len();
        self.b.scopes.push(Scope {
            parent: Some(parent),
            blocked: blocked.clone(),
            ..Scope::default()
        });
        let scope = self.b.scopes.len();
        self.b.scopes.push(Scope {
            parent: Some(lexical_parent),
            class_namespace: true,
            blocked,
            ..Scope::default()
        });
        let base_scope = if self.file.language == "python" {
            base_scope
        } else {
            scope
        };
        let index = self.b.catalog.classes.len();
        self.classes += 1;
        self.b.catalog.classes.push(ClassDefinition {
            symbol,
            qualified_name: qualified.clone(),
            language: self.file.language.clone(),
            declaration_kind: n
                .kind()
                .trim_end_matches("_declaration")
                .trim_end_matches("_definition")
                .into(),
            fields: vec![],
            methods: vec![],
            truncated: false,
        });
        let body = n.child_by_field_name("body");
        let mut children = if let Some(body) = body {
            self.children(body, depth + 1)?
        } else {
            vec![]
        };
        // Java enum declarations use an extra body wrapper after the constants.
        let mut expanded = vec![];
        for child in children {
            if child.kind() == "enum_body_declarations" {
                expanded.extend(self.children(child, depth + 1)?);
            } else {
                expanded.push(child);
            }
        }
        children = expanded;
        self.bind_body(&children, scope, &qualified, depth + 1)?;
        for (field, kind) in [
            ("superclass", "extends"),
            ("interfaces", "implements"),
            ("superclasses", "extends"),
        ] {
            if let Some(ty) = n.child_by_field_name(field) {
                self.types(ty, index, base_scope, &BTreeSet::new(), kind, depth + 1)?;
            }
        }
        // Java interfaces have an unnamed extends_interfaces child.
        for c in self.children(n, depth + 1)? {
            if c.kind() == "extends_interfaces" {
                self.types(c, index, scope, &BTreeSet::new(), "extends", depth + 1)?;
            }
        }
        if n.kind() == "record_declaration"
            && let Some(params) = n.child_by_field_name("parameters")
        {
            for p in self.detail_children(params, index, depth + 1)? {
                self.field(p, index, scope, depth + 1)?;
            }
        }
        for raw in children {
            if self.exhausted {
                break;
            }
            let c = declaration(raw);
            if class_node(c) {
                self.class(c, scope, &qualified, depth + 1)?;
            } else if method_node(c) {
                self.method(c, index, scope, depth + 1)?;
            } else if matches!(
                c.kind(),
                "field_declaration" | "constant_declaration" | "enum_constant"
            ) {
                self.field(c, index, scope, depth + 1)?;
            } else if c.kind() == "expression_statement" && self.file.language == "python" {
                for assignment in self.children(c, depth + 1)? {
                    if assignment.kind() == "assignment" {
                        self.field(assignment, index, scope, depth + 1)?;
                    }
                }
            }
        }
        if self.exhausted {
            self.b.catalog.classes[index].truncated = true;
        }
        Ok(())
    }
    fn member(
        &mut self,
        n: Node<'_>,
        name: Node<'_>,
        ty: Option<Node<'_>>,
        index: usize,
        method: bool,
    ) -> Result<()> {
        if !self.details(index)? {
            return Ok(());
        }
        let c = &self.b.catalog.classes[index];
        if c.fields.len() + c.methods.len() >= MEMBERS {
            self.b.catalog.classes[index].truncated = true;
            self.b.limit(&format!(
                "{}: class member limit reached (256/class)",
                self.file.path
            ));
            return Ok(());
        }
        if self.text(name).len() > TEXT || ty.is_some_and(|t| t.end_byte() - t.start_byte() > TEXT)
        {
            self.b.catalog.classes[index].truncated = true;
            self.b.limit("Class member text exceeds 2048 bytes");
            return Ok(());
        }
        let member = ClassMember {
            name: self.text(name).into(),
            type_hint: ty.map(|t| self.text(t).into()),
            symbol_id: if method {
                self.b
                    .symbol(self.file, n, SymbolKind::Method)
                    .or_else(|| self.b.symbol(self.file, n, SymbolKind::Function))
                    .map(|s| s.id)
            } else {
                None
            },
            path: self.file.path.clone(),
            range: range(n),
        };
        if !self.b.reserve_detail_text(
            member.name.len()
                + member.type_hint.as_ref().map_or(0, String::len)
                + member.symbol_id.as_ref().map_or(0, String::len)
                + member.path.len(),
        ) {
            self.b.catalog.classes[index].truncated = true;
            return Ok(());
        }
        let c = &mut self.b.catalog.classes[index];
        if method {
            c.methods.push(member);
        } else {
            c.fields.push(member);
        }
        self.b.records += 1;
        Ok(())
    }
    fn field(&mut self, n: Node<'_>, index: usize, scope: usize, depth: usize) -> Result<()> {
        if !self.details(index)? {
            return Ok(());
        }
        let ty = n.child_by_field_name("type");
        if let Some(ty) = ty {
            self.types(ty, index, scope, &BTreeSet::new(), "field", depth + 1)?;
        }
        if n.kind() == "assignment" {
            if let Some(left) = n.child_by_field_name("left")
                && left.kind() == "identifier"
            {
                self.member(n, left, ty, index, false)?;
            }
        } else if let Some(name) = n.child_by_field_name("name") {
            self.member(n, name, ty, index, false)?;
        } else {
            for c in self.detail_children(n, index, depth + 1)? {
                if c.kind() == "variable_declarator"
                    && let Some(name) = c.child_by_field_name("name")
                {
                    self.member(c, name, ty, index, false)?;
                }
            }
        }
        Ok(())
    }
    fn method(&mut self, n: Node<'_>, index: usize, scope: usize, depth: usize) -> Result<()> {
        if !self.details(index)? {
            return Ok(());
        }
        let ty = n
            .child_by_field_name("return_type")
            .or_else(|| n.child_by_field_name("type"));
        if let Some(name) = n.child_by_field_name("name") {
            self.member(n, name, ty, index, true)?;
        }
        let Some(blocked) = self.type_parameters(n, depth + 1, Some(index))? else {
            return Ok(());
        };
        if !self
            .b
            .reserve_detail_text(blocked.iter().map(String::len).sum())
        {
            self.b.catalog.classes[index].truncated = true;
            return Ok(());
        }
        let method_scope = self.b.scopes.len();
        self.b.scopes.push(Scope {
            parent: Some(scope),
            blocked,
            ..Scope::default()
        });
        let scope = method_scope;
        let blocked = BTreeSet::new();
        if let Some(ty) = ty {
            self.types(ty, index, scope, &blocked, "returns", depth + 1)?;
        }
        if let Some(params) = n.child_by_field_name("parameters") {
            for p in self.detail_children(params, index, depth + 1)? {
                if let Some(ty) = p.child_by_field_name("type") {
                    self.types(ty, index, scope, &blocked, "parameter", depth + 2)?;
                } else if p.kind() == "spread_parameter" {
                    for ty in self.detail_children(p, index, depth + 2)? {
                        if matches!(
                            ty.kind(),
                            "type_identifier"
                                | "scoped_type_identifier"
                                | "generic_type"
                                | "array_type"
                        ) {
                            self.types(ty, index, scope, &blocked, "parameter", depth + 2)?;
                        }
                    }
                }
            }
        }
        // Deliberately never inspect the function body, default values or decorators.
        Ok(())
    }
    fn imported_name(&self, name: &str, scope: usize) -> String {
        let (head, tail) = name
            .split_once('.')
            .map(|(a, b)| (a, format!(".{b}")))
            .unwrap_or((name, String::new()));
        let mut current = Some(scope);
        while let Some(i) = current {
            let s = &self.b.scopes[i];
            if s.blocked.contains(head) || s.wildcard {
                break;
            }
            if let Some(values) = s.bindings.get(head) {
                if values.len() == 1
                    && let Some(value) = &values[0]
                {
                    return format!("{value}{tail}");
                }
                break;
            }
            current = s.parent;
        }
        name.into()
    }
    fn types(
        &mut self,
        n: Node<'_>,
        index: usize,
        scope: usize,
        blocked: &BTreeSet<String>,
        kind: &str,
        depth: usize,
    ) -> Result<()> {
        if !self.detail_tick(index, depth)? {
            return Ok(());
        }
        let java = self.file.language == "java";
        let atom = if java {
            matches!(n.kind(), "type_identifier" | "scoped_type_identifier")
        } else {
            matches!(n.kind(), "identifier" | "attribute" | "dotted_name")
        };
        if atom {
            let text = self.text(n).to_owned();
            if dotted(&text) {
                self.reference(n, &text, index, scope, blocked, kind)?;
            }
            return Ok(());
        }
        if !java && n.kind() == "string" {
            // Only simple quoted forward names. No eval, escape decoding or literal metadata.
            let text = self.text(n);
            let inner = text
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| text.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')));
            if let Some(inner) = inner.filter(|s| dotted(s)).map(str::to_owned) {
                self.reference(n, &inner, index, scope, blocked, kind)?;
            }
            return Ok(());
        }
        // Generic arguments are references in field/signature annotations, but are
        // not additional base classes. The contract has no generic-argument edge kind.
        if matches!(kind, "extends" | "implements")
            && matches!(n.kind(), "generic_type" | "subscript")
        {
            if let Some(first) = self.detail_children(n, index, depth + 1)?.first().copied() {
                self.types(first, index, scope, blocked, kind, depth + 1)?;
            }
            return Ok(());
        }
        if !java
            && n.kind() == "binary_operator"
            && !n
                .child_by_field_name("operator")
                .is_some_and(|op| self.text(op) == "|")
        {
            return Ok(());
        }
        if !java && matches!(n.kind(), "generic_type" | "subscript") {
            let children = self.detail_children(n, index, depth + 1)?;
            let base = children.first().map(|n| self.text(*n)).unwrap_or("");
            // Literal payloads and Annotated metadata are values, not declared type references.
            let canonical = self.imported_name(base, scope);
            let special = canonical
                .rsplit('.')
                .next()
                .unwrap_or(&canonical)
                .to_owned();
            for (i, c) in children.into_iter().enumerate() {
                if i > 0 && special == "Literal" {
                    break;
                }
                if i > 0 && special == "Annotated" {
                    if matches!(c.kind(), "type_parameter" | "tuple") {
                        if let Some(first) =
                            self.detail_children(c, index, depth + 1)?.first().copied()
                        {
                            self.types(first, index, scope, blocked, kind, depth + 1)?;
                        }
                    } else if i == 1 {
                        self.types(c, index, scope, blocked, kind, depth + 1)?;
                    }
                    break;
                }
                self.types(c, index, scope, blocked, kind, depth + 1)?;
            }
            return Ok(());
        }
        let container = if java {
            matches!(
                n.kind(),
                "superclass"
                    | "super_interfaces"
                    | "extends_interfaces"
                    | "type_list"
                    | "generic_type"
                    | "type_arguments"
                    | "array_type"
                    | "annotated_type"
                    | "wildcard"
            )
        } else {
            matches!(
                n.kind(),
                "type"
                    | "type_parameter"
                    | "union_type"
                    | "binary_operator"
                    | "argument_list"
                    | "list"
                    | "tuple"
                    | "parenthesized_expression"
                    | "constrained_type"
            )
        };
        if container {
            for c in self.detail_children(n, index, depth + 1)? {
                self.types(c, index, scope, blocked, kind, depth + 1)?;
            }
        }
        Ok(())
    }
    fn reference(
        &mut self,
        n: Node<'_>,
        name: &str,
        index: usize,
        scope: usize,
        blocked: &BTreeSet<String>,
        kind: &str,
    ) -> Result<()> {
        if !self.details(index)? {
            return Ok(());
        }
        if self.refs >= FILE_REFS || name.len() > TEXT {
            self.b.catalog.classes[index].truncated = true;
            self.b.limit(&format!(
                "{}: class reference limit reached (8192/file, 2048 bytes/name)",
                self.file.path
            ));
            return Ok(());
        }
        let owner = self.b.catalog.classes[index].symbol.id.clone();
        let relation = ClassRelation {
            id: format!(
                "class-ref:{owner}:{}:{}:{kind}:{name}",
                n.start_byte(),
                n.end_byte()
            ),
            owner,
            target: None,
            type_name: name.into(),
            kind: kind.into(),
            path: self.file.path.clone(),
            range: range(n),
            candidate_ids: vec![],
            match_kind: "unmatched".into(),
        };
        if !self.b.reserve_detail_text(
            relation.id.len()
                + relation.owner.len()
                + relation.type_name.len()
                + relation.path.len()
                + self.module.len(),
        ) {
            self.b.catalog.classes[index].truncated = true;
            return Ok(());
        }
        self.b.pending.push(Pending {
            relation,
            scope,
            blocked: blocked.clone(),
            module: self.module.clone(),
            language: self.file.language.clone(),
        });
        self.refs += 1;
        self.b.records += 1;
        Ok(())
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use crate::model::Graph;
    use std::sync::{Arc, atomic::AtomicBool};

    #[test]
    fn exhausted_detail_budgets_still_discover_classes_and_duplicate_bindings() {
        for (records, detail_text_bytes, detail_visits) in [
            (RECORDS, 0, 0),
            (0, OUTPUT_TEXT, 0),
            (0, 0, VISITS),
            (0, 0, VISITS - 3),
        ] {
            let cancel = Arc::new(AtomicBool::new(false));
            let file=SourceFile {path:"A.java".into(),hash:"synthetic".into(),language:"java".into(),
                text:"package p; class Target {} class Early { <Target, U, V, W> Target method(Target p) { return p; } Target value; class Nested {} } class Target {}".into()};
            let mut graph = Graph::default();
            crate::indexer_java::extract(&mut graph, &file, &cancel).unwrap();
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_java::LANGUAGE.into())
                .unwrap();
            let tree = parser.parse(&file.text, None).unwrap();
            let symbols = graph
                .nodes
                .iter()
                .filter(|s| s.kind == SymbolKind::Class)
                .map(|s| {
                    (
                        (s.path.as_str(), s.range.start_byte, s.range.end_byte),
                        vec![s],
                    )
                })
                .collect();
            let mut b = Builder {
                catalog: Catalog::default(),
                scopes: vec![Scope::default()],
                pending: vec![],
                symbols,
                cancel: &cancel,
                records,
                text_bytes: 0,
                detail_text_bytes,
                registry_complete: true,
            };
            Extractor {
                b: &mut b,
                file: &file,
                module: String::new(),
                visits: 0,
                classes: 0,
                refs: 0,
                exhausted: false,
                detail_visits,
            }
            .module(tree.root_node(), 0)
            .unwrap();
            assert!(b.registry_complete);
            assert!(
                b.pending.is_empty(),
                "incomplete method blockers must not emit references"
            );
            assert!(b.catalog.truncated);
            assert_eq!(b.catalog.classes.len(), 4);
            assert!(
                b.catalog
                    .classes
                    .iter()
                    .any(|c| c.qualified_name == "p.Early.Nested")
            );
            // Represents a reference measured before the detail budget was exhausted.
            b.pending.push(Pending {
                relation: ClassRelation {
                    id: "measured-before-cap".into(),
                    owner: b.catalog.classes[1].symbol.id.clone(),
                    target: None,
                    type_name: "Target".into(),
                    kind: "field".into(),
                    path: file.path.clone(),
                    range: SourceRange::default(),
                    candidate_ids: vec![],
                    match_kind: "unmatched".into(),
                },
                scope: 0,
                blocked: BTreeSet::new(),
                module: "p".into(),
                language: "java".into(),
            });
            b.resolve().unwrap();
            let relation = &b.catalog.relations[0];
            assert_eq!(relation.match_kind, "ambiguous");
            assert_eq!(relation.candidate_ids.len(), 2);
            assert!(relation.target.is_none());
        }
    }
}
