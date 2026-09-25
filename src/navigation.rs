//! Revision-bound navigation from cached, measured declarations and scoped type evidence.
//! This is line navigation, not token go-to-definition or global name resolution.
use crate::model::IndexPin;
use crate::{
    classes::{ClassMember, ClassRelation},
    model::{Symbol, SymbolKind},
};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, Params, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeSet;

pub const MAX_RESPONSE_BYTES: usize = 512 * 1024;
// Evidence records and source parsing have separate input budgets. Source never leaves this reader.
const INPUT_BYTES: usize = 256 * 1024;
const SOURCE_BYTES: usize = 2 * 1024 * 1024;
const RECORD_BYTES: i64 = 32 * 1024;
const QUERY_ROWS: usize = 128;
const TOTAL_ROWS: usize = 256;
const TARGETS: usize = 64;
const WARNINGS: usize = 32;

#[derive(Debug)]
pub struct InvalidRequest(pub &'static str);
impl std::fmt::Display for InvalidRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for InvalidRequest {}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum NavigationRequest {
    Source(SourceSelector),
    Member(MemberSelector),
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSelector {
    pub expected_revision: IndexPin,
    pub path: String,
    pub line: usize,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemberSelector {
    pub expected_revision: IndexPin,
    pub class_id: String,
    pub member_name: String,
    pub start_byte: usize,
    pub end_byte: usize,
}
impl NavigationRequest {
    pub fn expected_revision(&self) -> IndexPin {
        match self {
            Self::Source(s) => s.expected_revision,
            Self::Member(s) => s.expected_revision,
        }
    }
    pub fn validate(&self) -> Result<()> {
        let text = |s: &str, max| !s.is_empty() && s.len() <= max && !s.contains('\0');
        match self {
            Self::Source(s) => ensure!(
                text(&s.path, 8192)
                    && !s.path.contains(['\\', ':'])
                    && !s
                        .path
                        .split('/')
                        .any(|p| p.is_empty() || p == "." || p == "..")
                    && (1..=10_000_000).contains(&s.line),
                InvalidRequest(
                    "Choose a cached workspace-relative path and a 1-based source line."
                )
            ),
            Self::Member(s) => ensure!(
                text(&s.class_id, 8192)
                    && text(&s.member_name, 2048)
                    && s.start_byte < s.end_byte
                    && s.end_byte <= i64::MAX as usize,
                InvalidRequest("Choose a recorded class member by name and exact byte range.")
            ),
        }
        Ok(())
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigationTarget {
    pub symbol: Symbol,
    pub action: &'static str,
    pub reason: &'static str,
    pub match_kind: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigationResult {
    pub revision: IndexPin,
    pub targets: Vec<NavigationTarget>,
    pub warnings: Vec<String>,
    pub truncated: bool,
    pub require_index: bool,
}
struct Reader<'a> {
    db: &'a Connection,
    result: NavigationResult,
    input_bytes: usize,
    rows: usize,
    output_bytes: usize,
    seen: BTreeSet<(String, &'static str, String)>,
}
impl Reader<'_> {
    fn warn(&mut self, message: &str) {
        let message: String = message.chars().take(512).collect();
        if !self.result.warnings.contains(&message) {
            if self.result.warnings.len() < WARNINGS {
                self.result.warnings.push(message);
            } else {
                self.result.truncated = true;
            }
        }
    }
    fn clipped(&mut self) {
        self.result.truncated = true;
        self.warn("Navigation limits reached; some cached evidence or targets were omitted.");
    }
    // Each query narrows by path/owner/id/range in SQL. Oversized records are represented
    // by NULL, never loaded/deserialized. The caller must SELECT a bounded CASE expression.
    fn records<T: DeserializeOwned>(&mut self, sql: &str, values: impl Params) -> Result<Vec<T>> {
        let mut stmt = self.db.prepare(sql)?;
        let mut rows = stmt.query(values)?;
        let mut result = vec![];
        let mut count = 0;
        while let Some(row) = rows.next()? {
            count += 1;
            self.rows += 1;
            if count > QUERY_ROWS || self.rows > TOTAL_ROWS {
                self.clipped();
                break;
            }
            let payload: Option<String> = row.get(0)?;
            let Some(payload) = payload else {
                self.clipped();
                continue;
            };
            if self.input_bytes + payload.len() > INPUT_BYTES {
                self.clipped();
                break;
            }
            self.input_bytes += payload.len();
            result.push(serde_json::from_str(&payload)?);
        }
        Ok(result)
    }
    fn symbol(&mut self, id: &str) -> Result<Option<Symbol>> {
        Ok(self.records("SELECT CASE WHEN length(CAST(payload AS BLOB))<=?2 THEN payload END FROM nodes WHERE id=?1",
            params![id, RECORD_BYTES])?.pop())
    }
    fn add(&mut self, symbol: Symbol, reason: &'static str, certainty: &str) -> Result<()> {
        let action = match symbol.kind {
            SymbolKind::Function | SymbolKind::Method => "sequence",
            SymbolKind::Class => {
                if !self.db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM classes WHERE id=?1)",
                    [&symbol.id],
                    |r| r.get::<_, bool>(0),
                )? {
                    return Ok(());
                }
                "class"
            }
            _ => return Ok(()),
        };
        if reason == "enclosing"
            && self
                .result
                .targets
                .iter()
                .any(|t| t.symbol.id == symbol.id && t.reason == "declaration")
        {
            return Ok(());
        }
        if !self
            .seen
            .insert((symbol.id.clone(), reason, certainty.into()))
        {
            return Ok(());
        }
        let target = NavigationTarget {
            symbol,
            action,
            reason,
            match_kind: certainty.into(),
        };
        let bytes = serde_json::to_vec(&target)?.len() + 1;
        // Reserve room for warnings and the envelope, including worst-case JSON escaping.
        if self.result.targets.len() >= TARGETS
            || self.output_bytes + bytes > MAX_RESPONSE_BYTES - 100 * 1024
        {
            self.clipped();
            return Ok(());
        }
        self.output_bytes += bytes;
        self.result.targets.push(target);
        Ok(())
    }
    fn types(&mut self, relations: Vec<ClassRelation>) -> Result<()> {
        for relation in relations {
            let ids: BTreeSet<_> = relation
                .target
                .iter()
                .chain(relation.candidate_ids.iter())
                .collect();
            if ids.is_empty() {
                self.warn(&format!(
                    "No indexed target for declared type {} ({}).",
                    relation.type_name, relation.match_kind
                ));
                continue;
            }
            self.warn("Type targets are cached scoped syntax candidates, not compiler resolution.");
            if ids.len() > 32 {
                self.clipped();
            }
            for id in ids.into_iter().take(32) {
                if self.rows >= TOTAL_ROWS {
                    self.clipped();
                    return Ok(());
                }
                if let Some(symbol) = self.symbol(id)? {
                    // Relations may only navigate classes supported by the cached projection.
                    if symbol.kind == SymbolKind::Class {
                        self.add(symbol, "type", &relation.match_kind)?;
                    }
                }
            }
        }
        Ok(())
    }
    fn source(&mut self, s: &SourceSelector) -> Result<()> {
        // Count lines inside SQLite: do not copy even a large cached source into Rust.
        // Byte spans always come from measured symbols, never Unicode character offsets.
        let lines: Option<i64> = self.db.query_row(
            "SELECT 1+length(json_extract(payload,'$.text'))-length(replace(json_extract(payload,'$.text'),char(10),'')) FROM files WHERE path=?1",
            [&s.path], |r| r.get(0)).optional()?;
        ensure!(
            lines.is_some_and(|lines| s.line as i64 <= lines),
            InvalidRequest("The path or line is not present in the cached revision.")
        );
        let declarations: Vec<Symbol> = self.records(
            "SELECT CASE WHEN length(CAST(payload AS BLOB))<=?3 THEN payload END FROM nodes
             WHERE path=?1 AND json_extract(payload,'$.kind') IN ('class','method','function')
             AND json_extract(payload,'$.range.startLine')=?2 ORDER BY id LIMIT 129",
            params![s.path, s.line as i64, RECORD_BYTES],
        )?;
        for symbol in declarations {
            self.add(symbol, "declaration", "measured")?;
        }
        for kinds in ["class", "callable"] {
            let symbols: Vec<Symbol> = self.records(
                "SELECT CASE WHEN length(CAST(payload AS BLOB))<=?4 THEN payload END FROM nodes
                 WHERE path=?1 AND ((?3='class' AND json_extract(payload,'$.kind')='class')
                    OR (?3='callable' AND json_extract(payload,'$.kind') IN ('function','method')))
                 AND json_extract(payload,'$.range.startLine')<=?2
                 AND (json_extract(payload,'$.range.endLine')>?2 OR
                    (json_extract(payload,'$.range.endLine')=?2 AND json_extract(payload,'$.range.endColumn')>1))
                 ORDER BY json_extract(payload,'$.range.endByte')-json_extract(payload,'$.range.startByte'),id LIMIT 1",
                params![s.path, s.line as i64, kinds, RECORD_BYTES])?;
            for symbol in symbols {
                self.add(symbol, "enclosing", "measured")?;
            }
        }
        let relations = self.records(
            "SELECT CASE WHEN length(CAST(r.payload AS BLOB))<=?3 THEN r.payload END
             FROM classes c JOIN class_relations r ON r.owner=c.id WHERE c.path=?1
             AND json_extract(r.payload,'$.range.startLine')<=?2
             AND (json_extract(r.payload,'$.range.endLine')>?2 OR
                (json_extract(r.payload,'$.range.endLine')=?2 AND json_extract(r.payload,'$.range.endColumn')>1))
             ORDER BY r.id LIMIT 129", params![s.path, s.line as i64, RECORD_BYTES])?;
        self.types(relations)?;
        // Preserve measured internal targets. Syntax candidates below never resolve graph calls.
        let calls: Vec<crate::model::CallSite> = self.records(
            "SELECT CASE WHEN length(CAST(payload AS BLOB))<=?3 THEN payload END FROM calls
             WHERE path=?1 AND target IS NOT NULL AND json_extract(payload,'$.resolution')='internal'
             AND json_extract(payload,'$.range.startLine')<=?2
             AND (json_extract(payload,'$.range.endLine')>?2 OR
                (json_extract(payload,'$.range.endLine')=?2 AND json_extract(payload,'$.range.endColumn')>1))
             ORDER BY id LIMIT 129", params![s.path, s.line as i64, RECORD_BYTES])?;
        for call in calls {
            if let Some(id) = call.target
                && let Some(symbol) = self.symbol(&id)?
            {
                self.add(symbol, "call", "measured")?;
            }
        }
        self.java_same_class_calls(s)?;
        Ok(())
    }
    fn java_same_class_calls(&mut self, s: &SourceSelector) -> Result<()> {
        let java: bool = self.db.query_row(
            "SELECT json_extract(payload,'$.language')='java' FROM files WHERE path=?1",
            [&s.path],
            |r| r.get(0),
        )?;
        if !java {
            return Ok(());
        }
        let calls: Vec<crate::model::CallSite> = self.records(
            "SELECT CASE WHEN length(CAST(payload AS BLOB))<=?3 THEN payload END FROM calls
             WHERE path=?1 AND target IS NULL AND json_extract(payload,'$.resolution')='unresolved'
             AND json_extract(payload,'$.range.startLine')<=?2
             AND (json_extract(payload,'$.range.endLine')>?2 OR
                (json_extract(payload,'$.range.endLine')=?2 AND json_extract(payload,'$.range.endColumn')>1))
             ORDER BY id LIMIT 129", params![s.path, s.line as i64, RECORD_BYTES])?;
        if calls.is_empty() {
            return Ok(());
        }
        let Some(java) = self.cached_java(&s.path)? else {
            return Ok(());
        };
        let mut searched = BTreeSet::new();
        for call in calls {
            if self.rows >= TOTAL_ROWS {
                self.clipped();
                break;
            }
            let Some(caller) = self.symbol(&call.caller)? else {
                continue;
            };
            let Some(owner_id) = caller.parent.as_deref() else {
                continue;
            };
            let Some(owner) = self.symbol(owner_id)? else {
                continue;
            };
            let Some(name) = java.same_class_call(&call, &caller, &owner) else {
                continue;
            };
            if !searched.insert((owner.id.clone(), name.to_owned())) {
                continue;
            }
            // Class member arrays are capped projections. Measured nodes preserve overloads.
            // Never broaden this query to a file-wide or global bare-name search.
            let methods: Vec<Symbol> = self.records(
                "SELECT CASE WHEN length(CAST(payload AS BLOB))<=?4 THEN payload END FROM nodes
                 WHERE path=?1 AND name=?2 AND json_extract(payload,'$.parent')=?3
                 AND json_extract(payload,'$.kind')='method' ORDER BY id LIMIT 129",
                params![s.path, name, owner.id, RECORD_BYTES],
            )?;
            for method in methods {
                if method.path == owner.path
                    && method.parent.as_deref() == Some(&owner.id)
                    && method.name == name
                    && java.method_owner(&method, &owner).is_some()
                {
                    self.warn("Same-class method targets are syntax candidates, not compiler resolution; overload selection and dispatch are not resolved.");
                    self.add(method, "call", "sameClassCandidate")?;
                }
            }
        }
        if java.limited.get() {
            self.clipped();
        }
        Ok(())
    }
    /// Load and parse at most one cached file per navigation request, separately from
    /// evidence-record budgets. Source/member selectors are mutually exclusive.
    fn cached_java(&mut self, path: &str) -> Result<Option<CachedJava>> {
        let text: Option<Vec<u8>> = self
            .db
            .query_row(
                "SELECT CASE WHEN length(CAST(json_extract(payload,'$.text') AS BLOB))<=?2
                THEN CAST(json_extract(payload,'$.text') AS BLOB) END FROM files WHERE path=?1",
                params![path, SOURCE_BYTES as i64],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let Some(text) = text else {
            self.clipped();
            self.warn("Cached Java source exceeds the navigation text budget; syntax associations cannot be verified.");
            return Ok(None);
        };
        let started = std::time::Instant::now();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_java::LANGUAGE.into())?;
        let mut expired = |_: &tree_sitter::ParseState| {
            started.elapsed() >= std::time::Duration::from_millis(250)
        };
        let tree = parser.parse_with_options(
            &mut |offset, _| &text[offset..],
            None,
            Some(tree_sitter::ParseOptions::new().progress_callback(&mut expired)),
        );
        let Some(tree) = tree else {
            self.clipped();
            self.warn("Cached Java syntax association exceeded the parse time limit.");
            return Ok(None);
        };
        if started.elapsed() >= std::time::Duration::from_millis(250) {
            self.clipped();
            self.warn("Cached Java syntax association exceeded the parse time limit.");
            return Ok(None);
        }
        if tree.root_node().has_error() {
            self.warn(
                "No indexed target: cached Java syntax errors prevent proving syntax associations.",
            );
            return Ok(None);
        }
        Ok(Some(CachedJava {
            text,
            tree,
            limited: std::cell::Cell::new(false),
        }))
    }
    /// Java stores declarator spans separately from their preceding shared type.
    /// Prove association structurally in bounded cached source; never infer it from names,
    /// nearest lines, or a client type hint. This does not create/resolve any new evidence.
    fn java_field_type(
        &mut self,
        owner: &Symbol,
        member: &ClassMember,
    ) -> Result<Option<(usize, usize)>> {
        let Some(java) = self.cached_java(&member.path)? else {
            return Ok(None);
        };
        let (text, tree) = (&java.text, &java.tree);
        // The smallest matching node may be the name identifier, so climb to its declarator.
        let mut node = tree
            .root_node()
            .descendant_for_byte_range(member.range.start_byte, member.range.end_byte);
        for _ in 0..64 {
            let Some(current) = node else {
                break;
            };
            if current.kind() == "variable_declarator"
                && current.start_byte() == member.range.start_byte
                && current.end_byte() == member.range.end_byte
                && current
                    .child_by_field_name("name")
                    .is_some_and(|n| text.get(n.byte_range()) == Some(member.name.as_bytes()))
            {
                if let Some(parent) = current.parent()
                    && matches!(parent.kind(), "field_declaration" | "constant_declaration")
                    && !parent.has_error()
                    && let Some(ty) = parent.child_by_field_name("type")
                {
                    let mut ancestor = parent.parent();
                    for _ in 0..64 {
                        let Some(class) = ancestor else {
                            break;
                        };
                        if matches!(
                            class.kind(),
                            "class_declaration"
                                | "interface_declaration"
                                | "enum_declaration"
                                | "record_declaration"
                                | "annotation_type_declaration"
                        ) {
                            if class.start_byte() == owner.range.start_byte
                                && class.end_byte() == owner.range.end_byte
                                && class.child_by_field_name("name").is_some_and(|n| {
                                    text.get(n.byte_range()) == Some(owner.name.as_bytes())
                                })
                            {
                                return Ok(Some((ty.start_byte(), ty.end_byte())));
                            }
                            break;
                        }
                        ancestor = class.parent();
                    }
                }
                break;
            }
            node = current.parent();
        }
        self.warn(
            "No indexed target: the cached Java field/type association could not be verified.",
        );
        Ok(None)
    }
    fn member(&mut self, s: &MemberSelector) -> Result<()> {
        // JSON arrays stay in SQLite. Validate exact recorded name + range before returning
        // even one member, including overloads and shared multi-field declaration ranges.
        if self.result.require_index {
            return Ok(());
        }
        let matches: i64 = self.db.query_row(
            "SELECT count(*) FROM (SELECT 1 FROM classes c, json_each(c.payload) a, json_each(a.value) m
             WHERE c.id=?1 AND a.key IN ('fields','methods')
             AND json_extract(m.value,'$.name')=?2
             AND json_extract(m.value,'$.range.startByte')=?3 AND json_extract(m.value,'$.range.endByte')=?4 LIMIT 2)",
            params![s.class_id, s.member_name, s.start_byte as i64, s.end_byte as i64], |r| r.get(0))?;
        ensure!(
            matches == 1,
            InvalidRequest("The selector does not match one recorded class member.")
        );
        let members: Vec<ClassMember> = self.records(
            "SELECT CASE WHEN length(CAST(m.value AS BLOB))<=?5 THEN m.value END
             FROM classes c, json_each(c.payload) a, json_each(a.value) m
             WHERE c.id=?1 AND a.key IN ('fields','methods')
             AND json_extract(m.value,'$.name')=?2
             AND json_extract(m.value,'$.range.startByte')=?3 AND json_extract(m.value,'$.range.endByte')=?4 LIMIT 2",
            params![s.class_id, s.member_name, s.start_byte as i64, s.end_byte as i64, RECORD_BYTES])?;
        let Some(member) = members.first() else {
            self.warn("The recorded member exceeds the navigation evidence budget.");
            return Ok(());
        };
        if let Some(id) = &member.symbol_id
            && let Some(symbol) = self.symbol(id)?
            && matches!(symbol.kind, SymbolKind::Method | SymbolKind::Function)
            && symbol.name == member.name
            && symbol.path == member.path
            && symbol.range == member.range
            && symbol.parent.as_deref() == Some(&s.class_id)
        {
            self.add(symbol, "declaration", "measured")?;
        }
        let java_field = member.symbol_id.is_none()
            && self.db.query_row(
                "SELECT json_extract(payload,'$.language')='java' FROM classes WHERE id=?1",
                [&s.class_id],
                |r| r.get::<_, bool>(0),
            )?;
        let (start, end) = if java_field {
            let Some(owner) = self.symbol(&s.class_id)? else {
                return Ok(());
            };
            if owner.kind != SymbolKind::Class || owner.path != member.path {
                self.warn("No indexed target: cached member ownership could not be verified.");
                return Ok(());
            }
            let Some(bounds) = self.java_field_type(&owner, member)? else {
                return Ok(());
            };
            bounds
        } else {
            (s.start_byte, s.end_byte)
        };
        let relations = self.records(
            "SELECT CASE WHEN length(CAST(payload AS BLOB))<=?5 THEN payload END FROM class_relations
             WHERE owner=?1 AND json_extract(payload,'$.path')=?2
             AND json_extract(payload,'$.range.startByte')>=?3 AND json_extract(payload,'$.range.endByte')<=?4
             AND (json_extract(payload,'$.kind')='field' OR (?6=0 AND json_extract(payload,'$.kind') IN ('parameter','returns')))
             ORDER BY id LIMIT 129",
            params![s.class_id, member.path, start as i64, end as i64, RECORD_BYTES, java_field])?;
        if relations.is_empty() && member.type_hint.is_some() {
            self.warn("No indexed target for this member's declared type (builtin or no cached type evidence).");
        }
        self.types(relations)?;
        if self.result.targets.is_empty() && self.result.warnings.is_empty() {
            self.warn("No indexed target for this member or its declared types.");
        }
        Ok(())
    }
}
// Bounded structural checks. No identifier search or overload/receiver inference.
struct CachedJava {
    text: Vec<u8>,
    tree: tree_sitter::Tree,
    limited: std::cell::Cell<bool>,
}
impl CachedJava {
    fn measured(node: tree_sitter::Node<'_>, range: &crate::model::SourceRange) -> bool {
        node.start_byte() == range.start_byte
            && node.end_byte() == range.end_byte
            && node.start_position().row + 1 == range.start_line
            && node.start_position().column + 1 == range.start_column
            && node.end_position().row + 1 == range.end_line
            && node.end_position().column + 1 == range.end_column
    }
    fn named(&self, node: tree_sitter::Node<'_>, symbol: &Symbol) -> bool {
        Self::measured(node, &symbol.range)
            && node
                .child_by_field_name("name")
                .is_some_and(|n| self.text.get(n.byte_range()) == Some(symbol.name.as_bytes()))
    }
    fn exact_node(&self, range: &crate::model::SourceRange) -> Option<tree_sitter::Node<'_>> {
        if range.start_byte >= range.end_byte || range.end_byte > self.text.len() {
            return None;
        }
        let mut node = self
            .tree
            .root_node()
            .descendant_for_byte_range(range.start_byte, range.end_byte)?;
        for _ in 0..64 {
            if Self::measured(node, range) {
                return Some(node);
            }
            node = node.parent()?;
        }
        self.limited.set(true);
        None
    }
    fn class_kind(kind: &str) -> bool {
        matches!(
            kind,
            "class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "record_declaration"
                | "annotation_type_declaration"
        )
    }
    fn body_kind(kind: &str) -> bool {
        matches!(
            kind,
            "class_body"
                | "interface_body"
                | "enum_body"
                | "enum_body_declarations"
                | "annotation_type_body"
        )
    }
    fn method_owner(&self, method: &Symbol, owner: &Symbol) -> Option<tree_sitter::Node<'_>> {
        if method.kind != SymbolKind::Method
            || owner.kind != SymbolKind::Class
            || method.path != owner.path
            || method.parent.as_deref() != Some(&owner.id)
        {
            return None;
        }
        let node = self.exact_node(&method.range)?;
        if node.kind() != "method_declaration" || !self.named(node, method) {
            return None;
        }
        let mut class = node.parent()?;
        for _ in 0..4 {
            if !Self::body_kind(class.kind()) {
                break;
            }
            class = class.parent()?;
        }
        if !Self::class_kind(class.kind()) || !self.named(class, owner) {
            return None;
        }
        // Named member classes are valid owners, but local and anonymous scopes are not.
        // Do not climb across a method, lambda, initializer or object creation to an outer class.
        let mut ancestor = class.parent()?;
        for _ in 0..64 {
            if ancestor.kind() == "program" {
                return Some(node);
            }
            if !Self::class_kind(ancestor.kind()) && !Self::body_kind(ancestor.kind()) {
                return None;
            }
            ancestor = ancestor.parent()?;
        }
        self.limited.set(true);
        None
    }
    fn same_class_call<'a>(
        &'a self,
        call: &crate::model::CallSite,
        caller: &Symbol,
        owner: &Symbol,
    ) -> Option<&'a str> {
        if call.path != caller.path || call.caller != caller.id {
            return None;
        }
        let method = self.method_owner(caller, owner)?;
        let invocation = self.exact_node(&call.range)?;
        if invocation.kind() != "method_invocation" {
            return None;
        }
        let name = invocation.child_by_field_name("name")?;
        let name = std::str::from_utf8(self.text.get(name.byte_range())?).ok()?;
        let callee = match invocation.child_by_field_name("object") {
            None => name.to_owned(),
            Some(object)
                if object.kind() == "this"
                    && self.text.get(object.byte_range()) == Some(b"this".as_slice()) =>
            {
                format!("this.{name}")
            }
            _ => return None,
        };
        if callee != call.callee_text {
            return None;
        }
        // Reject super forms even when the grammar stores super separately from object.
        let mut cursor = invocation.walk();
        if invocation
            .children(&mut cursor)
            .any(|n| n.kind() == "super")
        {
            return None;
        }
        let mut ancestor = invocation.parent()?;
        for _ in 0..64 {
            if ancestor == method {
                return Some(name);
            }
            if matches!(
                ancestor.kind(),
                "method_declaration"
                    | "constructor_declaration"
                    | "compact_constructor_declaration"
                    | "lambda_expression"
            ) || Self::class_kind(ancestor.kind())
                || Self::body_kind(ancestor.kind())
            {
                return None;
            }
            ancestor = ancestor.parent()?;
        }
        self.limited.set(true);
        None
    }
}
pub(crate) fn navigate(
    db: &Connection,
    request: &NavigationRequest,
    revision: IndexPin,
) -> Result<NavigationResult> {
    let metadata: Option<bool> = db
        .query_row(
            "SELECT truncated FROM class_catalog WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let mut reader = Reader {
        db,
        result: NavigationResult {
            revision,
            targets: vec![],
            warnings: vec![],
            truncated: metadata.unwrap_or(false),
            require_index: metadata.is_none(),
        },
        input_bytes: 0,
        rows: 0,
        output_bytes: 0,
        seen: BTreeSet::new(),
    };
    if metadata.is_none() {
        reader.warn(crate::class_diagram::INDEX_NOTICE);
    }
    if metadata == Some(true) {
        reader.warn("The cached class projection is incomplete; some declared types or members may be absent.");
    }
    match request {
        NavigationRequest::Source(s) => reader.source(s)?,
        NavigationRequest::Member(s) => reader.member(s)?,
    }
    if reader.result.targets.is_empty() && reader.result.warnings.is_empty() {
        reader.warn("No indexed navigation target on this source line.");
    }
    Ok(reader.result)
}
