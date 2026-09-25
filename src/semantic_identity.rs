//! Canonical, domain-separated identities for measured syntax and occurrences.
use crate::model::v1::{self, Header, Key, Language, OccurrenceId, SyntaxId, UInt};
use anyhow::{Result, bail, ensure};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use unicode_normalization::UnicodeNormalization;

const SYNTAX: &[u8] = b"baleyg.syntax.v1\0";
const OCCURRENCE: &[u8] = b"baleyg.occurrence.v1\0";
const HEADER: &[u8] = b"baleyg.header.v1\0";
const GROUP: &[u8] = b"baleyg.sibling-group.v1\0";

/// A canonical digest retains the complete hash as well as the bytes used to obtain it.
#[derive(Debug, Clone)]
pub struct CanonicalDigest {
    pub input: Vec<u8>,
    pub sha256: String,
}

fn write_json(value: &Value, out: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => {
            ensure!(
                n.as_u64().is_some_and(|n| n <= v1::MAX_SAFE_INTEGER),
                "expected UInt"
            );
            out.extend_from_slice(n.to_string().as_bytes());
        }
        Value::String(s) => {
            out.push(b'"');
            for c in s.chars() {
                match c {
                    '"' => out.extend_from_slice(b"\\\""),
                    '\\' => out.extend_from_slice(b"\\\\"),
                    c if c <= '\u{1f}' => {
                        out.extend_from_slice(format!("\\u00{:02x}", c as u32).as_bytes())
                    }
                    c => {
                        let mut buf = [0; 4];
                        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    }
                }
            }
            out.push(b'"');
        }
        Value::Array(a) => {
            out.push(b'[');
            for (i, item) in a.iter().enumerate() {
                if i != 0 {
                    out.push(b',');
                }
                write_json(item, out)?;
            }
            out.push(b']');
        }
        Value::Object(map) => {
            out.push(b'{');
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.as_bytes().cmp(b.as_bytes()));
            for (i, (key, item)) in entries.into_iter().enumerate() {
                ensure!(key.is_ascii(), "canonical object keys must be ASCII");
                if i != 0 {
                    out.push(b',');
                }
                write_json(&Value::String(key.clone()), out)?;
                out.push(b':');
                write_json(item, out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

pub fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let value = serde_json::to_value(value)?;
    let mut out = Vec::new();
    write_json(&value, &mut out)?;
    Ok(out)
}

pub fn digest<T: Serialize>(domain: &[u8], value: &T) -> Result<CanonicalDigest> {
    let input = canonical_json(value)?;
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(&input);
    Ok(CanonicalDigest {
        input,
        sha256: hex::encode(hasher.finalize()),
    })
}

fn validate_key(key: &Key, language: Language) -> Result<()> {
    let named = key.name.is_some();
    ensure!(
        named == !matches!(key.kind, v1::Kind::Module | v1::Kind::AnonymousFunction),
        "key name disagrees with kind"
    );
    ensure!(
        key.signature.is_some()
            == (language == Language::Java
                && matches!(key.kind, v1::Kind::Method | v1::Kind::Constructor)),
        "invalid signature for language/kind"
    );
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StableInput<'a> {
    source_set: &'a v1::Text,
    path: &'a v1::Path,
    language: Language,
    ancestors: &'a [Key],
    declaration: &'a Key,
}

pub fn syntax_digest(
    source_set: &v1::Text,
    path: &v1::Path,
    language: Language,
    ancestors: &[Key],
    declaration: &Key,
) -> Result<CanonicalDigest> {
    for key in ancestors.iter().chain(std::iter::once(declaration)) {
        validate_key(key, language)?;
    }
    digest(
        SYNTAX,
        &StableInput {
            source_set,
            path,
            language,
            ancestors,
            declaration,
        },
    )
}

pub fn syntax_id(
    source_set: &v1::Text,
    path: &v1::Path,
    language: Language,
    ancestors: &[Key],
    declaration: &Key,
) -> Result<SyntaxId> {
    let hash = syntax_digest(source_set, path, language, ancestors, declaration)?.sha256;
    Ok(SyntaxId::new(format!("sid:v1:{}", &hash[..32])).expect("valid truncated digest"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OccurrenceKind {
    Call,
    Reference,
    Control,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OccurrenceInput<'a> {
    revision_id: &'a v1::Text,
    owner_syntax_id: &'a SyntaxId,
    kind: OccurrenceKind,
    ordinal: UInt,
}

pub fn occurrence_digest(
    revision_id: &v1::Text,
    owner_syntax_id: &SyntaxId,
    kind: OccurrenceKind,
    ordinal: UInt,
) -> Result<CanonicalDigest> {
    digest(
        OCCURRENCE,
        &OccurrenceInput {
            revision_id,
            owner_syntax_id,
            kind,
            ordinal,
        },
    )
}

pub fn occurrence_id(
    revision_id: &v1::Text,
    owner_syntax_id: &SyntaxId,
    kind: OccurrenceKind,
    ordinal: UInt,
) -> Result<OccurrenceId> {
    let hash = occurrence_digest(revision_id, owner_syntax_id, kind, ordinal)?.sha256;
    Ok(OccurrenceId::new(format!("occ:v1:{}", &hash[..32])).expect("valid truncated digest"))
}

pub fn header_digest(header: &Header) -> Result<CanonicalDigest> {
    ensure!(
        header.name.is_some()
            == !matches!(header.kind, v1::Kind::Module | v1::Kind::AnonymousFunction),
        "header name disagrees with kind"
    );
    digest(HEADER, header)
}

#[derive(Serialize)]
struct Headers<'a> {
    headers: &'a [String],
}
pub fn sibling_group_digest(headers: &[String]) -> Result<CanonicalDigest> {
    ensure!(!headers.is_empty(), "empty sibling group");
    for header in headers {
        ensure!(
            v1::Hash::new(header.clone()).is_some(),
            "invalid header digest"
        );
    }
    digest(GROUP, &Headers { headers })
}

/// Track emitted handles in one retained identity domain, rejecting conflicting inputs.
#[derive(Default)]
pub struct CollisionRegistry {
    syntax: HashMap<String, Vec<u8>>,
    occurrence: HashMap<String, Vec<u8>>,
}
impl CollisionRegistry {
    fn register(map: &mut HashMap<String, Vec<u8>>, id: &str, input: Vec<u8>) -> Result<()> {
        if let Some(old) = map.get(id) {
            ensure!(old == &input, "identity collision: {id}");
        } else {
            map.insert(id.to_owned(), input);
        }
        Ok(())
    }
    pub fn syntax(&mut self, id: &SyntaxId, input: Vec<u8>) -> Result<()> {
        Self::register(&mut self.syntax, id.as_str(), input)
    }
    pub fn occurrence(&mut self, id: &OccurrenceId, input: Vec<u8>) -> Result<()> {
        Self::register(&mut self.occurrence, id.as_str(), input)
    }
}

/// Group by the immediate container and exact key fields; return source-index -> ordinal.
pub fn sibling_ordinals(entries: &[(Vec<Key>, Key, u64, u64)]) -> Result<Vec<UInt>> {
    let mut groups: BTreeMap<Vec<u8>, Vec<(usize, u64, u64)>> = BTreeMap::new();
    for (i, (ancestors, key, start, end)) in entries.iter().enumerate() {
        ensure!(start <= end, "reversed sibling range");
        groups
            .entry(canonical_json(&(
                ancestors,
                key.kind,
                &key.name,
                &key.signature,
            ))?)
            .or_default()
            .push((i, *start, *end));
    }
    let mut result = vec![UInt::new(0).unwrap(); entries.len()];
    for members in groups.values_mut() {
        members.sort_by_key(|(_, start, end)| (*start, *end));
        for pair in members.windows(2) {
            ensure!(
                (pair[0].1, pair[0].2) != (pair[1].1, pair[1].2),
                "duplicate sibling range"
            );
        }
        for (ordinal, (index, _, _)) in members.iter().enumerate() {
            result[*index] =
                UInt::new(ordinal as u64).ok_or_else(|| anyhow::anyhow!("ordinal overflow"))?;
        }
    }
    Ok(result)
}

/// Each owner/kind has a separate zero-based source-order namespace.
pub fn occurrence_ordinals(entries: &[(SyntaxId, OccurrenceKind, u64, u64)]) -> Result<Vec<UInt>> {
    type OccurrenceGroups = BTreeMap<(String, u8), Vec<(usize, u64, u64)>>;
    let mut groups: OccurrenceGroups = BTreeMap::new();
    for (i, (owner, kind, start, end)) in entries.iter().enumerate() {
        ensure!(start <= end, "reversed occurrence range");
        let kind = match kind {
            OccurrenceKind::Call => 0,
            OccurrenceKind::Reference => 1,
            OccurrenceKind::Control => 2,
        };
        groups
            .entry((owner.as_str().to_owned(), kind))
            .or_default()
            .push((i, *start, *end));
    }
    let mut result = vec![UInt::new(0).unwrap(); entries.len()];
    for members in groups.values_mut() {
        members.sort_by_key(|(_, start, end)| (*start, *end));
        for pair in members.windows(2) {
            ensure!(
                (pair[0].1, pair[0].2) != (pair[1].1, pair[1].2),
                "duplicate occurrence range"
            );
        }
        for (ordinal, (index, _, _)) in members.iter().enumerate() {
            result[*index] =
                UInt::new(ordinal as u64).ok_or_else(|| anyhow::anyhow!("ordinal overflow"))?;
        }
    }
    Ok(result)
}

/// Decode a measured lexical identifier before applying language lookup normalization.
pub fn lookup_key(language: Language, measured: &str) -> Result<String> {
    ensure!(!measured.is_empty(), "empty identifier");
    let decoded = match language {
        Language::Rust => measured.strip_prefix("r#").unwrap_or(measured).to_owned(),
        Language::Python => measured.to_owned(),
        Language::Java | Language::Javascript => decode_unicode_escapes(measured, language)?,
    };
    ensure!(!decoded.is_empty(), "empty decoded identifier");
    Ok(match language {
        Language::Rust => decoded.nfc().collect(),
        Language::Python => decoded.nfkc().collect(),
        _ => decoded,
    })
}

fn decode_unicode_escapes(text: &str, language: Language) -> Result<String> {
    let mut chars = text.chars().peekable();
    let mut result = String::new();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        ensure!(chars.next() == Some('u'), "invalid identifier escape");
        let mut hex = String::new();
        if language == Language::Javascript && chars.peek() == Some(&'{') {
            chars.next();
            let mut closed = false;
            for c in chars.by_ref() {
                if c == '}' {
                    closed = true;
                    break;
                }
                ensure!(
                    c.is_ascii_hexdigit() && hex.len() < 6,
                    "invalid Unicode escape"
                );
                hex.push(c);
            }
            ensure!(
                closed && !hex.is_empty() && hex.len() <= 6,
                "invalid Unicode escape"
            );
        } else {
            while chars.peek() == Some(&'u') && language == Language::Java {
                chars.next();
            }
            for _ in 0..4 {
                let c = chars
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("short Unicode escape"))?;
                ensure!(c.is_ascii_hexdigit(), "invalid Unicode escape");
                hex.push(c);
            }
        }
        let code = u32::from_str_radix(&hex, 16)?;
        let Some(decoded) = char::from_u32(code) else {
            bail!("invalid Unicode scalar escape");
        };
        result.push(decoded);
    }
    Ok(result)
}

/// Capture the exact focused header and complete measured sibling group.
pub fn capture_anchor(
    syntax_id: SyntaxId,
    document: v1::DocumentKey,
    revision_id: v1::Text,
    header: &Header,
    siblings: &[Header],
) -> Result<v1::DurableAnchor> {
    ensure!(!siblings.is_empty(), "empty sibling group");
    let header_hash = header_digest(header)?.sha256;
    let hashes = siblings
        .iter()
        .map(|h| header_digest(h).map(|digest| digest.sha256))
        .collect::<Result<Vec<_>>>()?;
    let count = hashes.iter().filter(|hash| *hash == &header_hash).count();
    ensure!(count > 0, "focused header absent from sibling group");
    let group_hash = sibling_group_digest(&hashes)?.sha256;
    Ok(v1::DurableAnchor {
        syntax_id,
        document,
        captured_revision_id: revision_id,
        header_hash: v1::Hash::new(header_hash).expect("SHA-256 digest"),
        sibling_group_hash: v1::Hash::new(group_hash).expect("SHA-256 digest"),
        sibling_count: UInt::new(siblings.len() as u64)
            .ok_or_else(|| anyhow::anyhow!("sibling count overflow"))?,
        identical_header_count: UInt::new(count as u64)
            .ok_or_else(|| anyhow::anyhow!("sibling count overflow"))?,
    })
}

/// Audit a captured anchor against exact measured candidates in its document.
/// Callers must supply the independently established group continuity state.
pub fn evaluate_anchor(
    captured: &v1::DurableAnchor,
    document: &v1::DocumentKey,
    revision: &v1::Text,
    candidates: &[(SyntaxId, Header, Vec<Header>)],
    continuity: Option<&v1::GroupContinuity>,
) -> Result<v1::AnchorResult> {
    use v1::{AnchorReason as Reason, AnchorStatus as Status};
    ensure!(
        &captured.document == document,
        "candidate document differs from capture"
    );
    let orphan = |reason| v1::AnchorResult {
        status: Status::Orphaned,
        target_id: None,
        reason,
    };
    let Some((_, header, group)) = candidates
        .iter()
        .find(|(id, _, _)| id == &captured.syntax_id)
    else {
        return Ok(orphan(Reason::Missing));
    };
    let current_header = header_digest(header)?.sha256;
    if current_header != captured.header_hash.as_str() {
        return Ok(orphan(Reason::HeaderMismatch));
    }
    let hashes = group
        .iter()
        .map(|header| header_digest(header).map(|d| d.sha256))
        .collect::<Result<Vec<_>>>()?;
    let identical = hashes
        .iter()
        .filter(|hash| *hash == &current_header)
        .count() as u64;
    if captured.identical_header_count.get() > 1 || identical > 1 {
        let group_hash = sibling_group_digest(&hashes)?.sha256;
        if group_hash != captured.sibling_group_hash.as_str()
            || group.len() as u64 != captured.sibling_count.get()
            || identical != captured.identical_header_count.get()
        {
            return Ok(orphan(Reason::GroupChanged));
        }
        if revision != &captured.captured_revision_id {
            let Some(proof) = continuity else {
                return Ok(orphan(Reason::UnprovenContinuity));
            };
            ensure!(
                proof.from_revision_id == captured.captured_revision_id
                    && proof.to_revision_id == *revision,
                "continuity revision mismatch"
            );
            match proof.state {
                v1::ContinuityState::Changed => return Ok(orphan(Reason::GroupChanged)),
                v1::ContinuityState::Unknown => return Ok(orphan(Reason::UnprovenContinuity)),
                v1::ContinuityState::Unchanged => ensure!(
                    proof.evidence.is_some(),
                    "missing independent continuity evidence"
                ),
            }
        }
    }
    Ok(v1::AnchorResult {
        status: Status::Attached,
        target_id: Some(captured.syntax_id.clone()),
        reason: Reason::None,
    })
}
