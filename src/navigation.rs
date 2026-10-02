//! Revision-bound navigation from cached, measured declarations and scoped type evidence.
//! This is line navigation, not token go-to-definition or global name resolution.
use crate::model::IndexPin;
use crate::{
    classes::ClassMember,
    model::{Symbol, SymbolKind},
};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, Params, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeSet;

pub const MAX_RESPONSE_BYTES: usize = 512 * 1024;
// Evidence records and source parsing have separate input budgets. Source never leaves this reader.
const INPUT_BYTES: usize = 256 * 1024;
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
        Ok(self.records("SELECT CASE WHEN length(CAST(payload AS BLOB))<=?2 THEN payload END FROM graph_nodes n JOIN revision_documents d ON d.revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND d.graph_projection_id=n.projection_id WHERE n.id=?1",
            params![id, RECORD_BYTES])?.pop())
    }
    fn add(&mut self, symbol: Symbol, reason: &'static str, certainty: &str) -> Result<()> {
        let action = match symbol.kind {
            SymbolKind::Function | SymbolKind::Method => "sequence",
            SymbolKind::Class => {
                if !self.db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM classes c JOIN revision_documents d ON d.revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND d.class_projection_id=c.projection_id WHERE c.id=?1)",
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
    fn source(&mut self, s: &SourceSelector) -> Result<()> {
        // Count lines inside SQLite: do not copy even a large cached source into Rust.
        // Byte spans always come from measured symbols, never Unicode character offsets.
        let lines: Option<i64> = self.db.query_row(
            "SELECT 1+length(v.source_bytes)-length(CAST(replace(CAST(v.source_bytes AS TEXT),char(10),'') AS BLOB)) FROM document_versions v JOIN revision_documents d ON d.revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND d.document_version_id=v.id WHERE d.path=?1",
            [&s.path], |r| r.get(0)).optional()?;
        ensure!(
            lines.is_some_and(|lines| s.line as i64 <= lines),
            InvalidRequest("The path or line is not present in the cached revision.")
        );
        let declarations: Vec<Symbol> = self.records(
            "SELECT CASE WHEN length(CAST(payload AS BLOB))<=?3 THEN payload END FROM graph_nodes n JOIN revision_documents d ON d.revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND d.graph_projection_id=n.projection_id
             WHERE n.path=?1 AND json_extract(payload,'$.kind') IN ('class','method','function')
             AND json_extract(payload,'$.range.startLine')=?2 ORDER BY id LIMIT 129",
            params![s.path, s.line as i64, RECORD_BYTES],
        )?;
        for symbol in declarations {
            self.add(symbol, "declaration", "measured")?;
        }
        for kinds in ["class", "callable"] {
            let symbols: Vec<Symbol> = self.records(
                "SELECT CASE WHEN length(CAST(payload AS BLOB))<=?4 THEN payload END FROM graph_nodes n JOIN revision_documents d ON d.revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND d.graph_projection_id=n.projection_id
                 WHERE n.path=?1 AND ((?3='class' AND json_extract(payload,'$.kind')='class')
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
        // Calls and declared type names do not prove a navigable declaration.
        Ok(())
    }
    fn member(&mut self, s: &MemberSelector) -> Result<()> {
        // JSON arrays stay in SQLite. Validate exact recorded name + range before returning
        // even one member, including overloads and shared multi-field declaration ranges.
        if self.result.require_index {
            return Ok(());
        }
        let matches: i64 = self.db.query_row(
            "SELECT count(*) FROM (SELECT 1 FROM classes c JOIN revision_documents d ON d.revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND d.class_projection_id=c.projection_id, json_each(c.payload) a, json_each(a.value) m
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
             FROM classes c JOIN revision_documents d ON d.revision_id=(SELECT 'pin:v1:'||index_generation||':'||index_revision FROM index_metadata WHERE singleton=1) AND d.class_projection_id=c.projection_id, json_each(c.payload) a, json_each(a.value) m
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
        // The recorded member is a terminal declaration. Its type hint is not
        // a link to another class or method.
        Ok(())
    }
}
pub(crate) fn navigate(
    db: &Connection,
    request: &NavigationRequest,
    revision: IndexPin,
) -> Result<NavigationResult> {
    let metadata: Option<bool> = db
        .query_row(
            "SELECT r.class_truncated FROM native_revisions r JOIN index_metadata m ON m.index_revision=r.published_index_revision AND r.id='pin:v1:'||m.index_generation||':'||m.index_revision",
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
