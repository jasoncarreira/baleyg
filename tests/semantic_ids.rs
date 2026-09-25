use baleyg::model::v1::{self, Header, Key, Language, Path, SyntaxId, Text, UInt};
use baleyg::semantic_identity::{self as ids, OccurrenceKind};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Vectors {
    version: u64,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Case {
    case_id: String,
    language: Language,
    scenario: String,
    descriptor: Descriptor,
    previous_case_id: Option<String>,
    continuity: Option<v1::GroupContinuity>,
    digests: Vec<DigestRow>,
    expected: Expected,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Descriptor {
    source_set: Text,
    path: Path,
    language: Language,
    ancestors: Vec<Key>,
    declaration: Key,
    header: Header,
    revision_id: Text,
    sibling_headers: Vec<Header>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DigestRow {
    label: String,
    algorithm: String,
    domain_hex: String,
    input_hex: String,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Expected {
    stable_id: SyntaxId,
    anchor: v1::DurableAnchor,
    previous_anchor_result: Option<v1::AnchorResult>,
}

fn assert_digest(row: &DigestRow, domain: &[u8], computed: &ids::CanonicalDigest) {
    assert_eq!(row.algorithm, "sha256", "{}", row.label);
    assert_eq!(row.domain_hex, hex::encode(domain), "{}", row.label);
    assert_eq!(row.input_hex, hex::encode(&computed.input), "{}", row.label);
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(&computed.input);
    assert_eq!(row.sha256, hex::encode(hash.finalize()), "{}", row.label);
    assert_eq!(row.sha256, computed.sha256, "{}", row.label);
}

// The vector suite is the first identity test. No synthetic test is a substitute for it.
#[test]
fn normative_vectors_then_synthetic() {
    all_64_normative_vectors();
    synthetic_lookup_spellings_and_occurrence_namespaces();
    synthetic_ordinals_and_canonical_control_escapes();
}

fn all_64_normative_vectors() {
    let vectors: Vectors = serde_json::from_str(include_str!(
        "../docs/semantic-evidence/id-test-vectors/stable-ids.json"
    ))
    .unwrap();
    assert_eq!(vectors.version, 1);
    assert_eq!(vectors.cases.len(), 64);
    let mut seen = HashMap::<String, usize>::new();
    for (index, case) in vectors.cases.iter().enumerate() {
        assert!(seen.insert(case.case_id.clone(), index).is_none());
        assert!(!case.scenario.is_empty());
        assert_eq!(case.language, case.descriptor.language, "{}", case.case_id);
        let d = &case.descriptor;
        let syntax = ids::syntax_digest(
            &d.source_set,
            &d.path,
            d.language,
            &d.ancestors,
            &d.declaration,
        )
        .unwrap();
        let stable = ids::syntax_id(
            &d.source_set,
            &d.path,
            d.language,
            &d.ancestors,
            &d.declaration,
        )
        .unwrap();
        let mut digests = case
            .digests
            .iter()
            .map(|row| (row.label.as_str(), row))
            .collect::<HashMap<_, _>>();
        assert_eq!(digests.len(), case.digests.len());
        assert_digest(
            digests.remove("syntax").unwrap(),
            b"baleyg.syntax.v1\0",
            &syntax,
        );
        assert_eq!(stable.as_str(), format!("sid:v1:{}", &syntax.sha256[..32]));
        assert_eq!(stable, case.expected.stable_id, "{}", case.case_id);
        assert_eq!(
            d.header,
            d.sibling_headers[d.declaration.ordinal.get() as usize],
            "{}",
            case.case_id
        );
        let mut distinct = HashMap::new();
        let hashes = d
            .sibling_headers
            .iter()
            .map(|h| {
                let hd = ids::header_digest(h).unwrap();
                distinct.entry(hd.sha256.clone()).or_insert(hd);
                ids::header_digest(h).unwrap().sha256
            })
            .collect::<Vec<_>>();
        let mut ordered_unique = Vec::new();
        for hash in &hashes {
            if !ordered_unique.contains(hash) {
                ordered_unique.push(hash.clone());
            }
        }
        for (i, hash) in ordered_unique.iter().enumerate() {
            assert_digest(
                digests.remove(format!("header-{i}").as_str()).unwrap(),
                b"baleyg.header.v1\0",
                &distinct[hash],
            );
        }
        let group = ids::sibling_group_digest(&hashes).unwrap();
        assert_digest(
            digests.remove("sibling-group").unwrap(),
            b"baleyg.sibling-group.v1\0",
            &group,
        );
        assert!(
            digests.is_empty(),
            "unverified digest row in {}",
            case.case_id
        );
        let anchor = &case.expected.anchor;
        assert_eq!(
            ids::capture_anchor(
                stable.clone(),
                anchor.document.clone(),
                d.revision_id.clone(),
                &d.header,
                &d.sibling_headers
            )
            .unwrap(),
            *anchor
        );
        assert_eq!(anchor.syntax_id, stable);
        assert_eq!(anchor.document.source_set_id, d.source_set);
        assert_eq!(anchor.document.path, d.path);
        assert_eq!(anchor.document.language, d.language);
        assert_eq!(anchor.captured_revision_id, d.revision_id);
        assert_eq!(
            anchor.header_hash.as_str(),
            hashes[d.declaration.ordinal.get() as usize]
        );
        assert_eq!(anchor.sibling_group_hash.as_str(), group.sha256);
        assert_eq!(anchor.sibling_count.get(), hashes.len() as u64);
        assert_eq!(
            anchor.identical_header_count.get(),
            hashes
                .iter()
                .filter(|x| **x == *anchor.header_hash.as_str())
                .count() as u64
        );
        if let Some(previous) = &case.previous_case_id {
            let prior = &vectors.cases[*seen
                .get(previous)
                .expect("previous vector must precede case")];
            let continuity = case
                .continuity
                .as_ref()
                .expect("linked case needs continuity");
            assert_eq!(continuity.from_revision_id, prior.descriptor.revision_id);
            assert_eq!(continuity.to_revision_id, d.revision_id);
            let mut candidates = Vec::new();
            for (ordinal, header) in d.sibling_headers.iter().enumerate() {
                let mut key = d.declaration.clone();
                key.ordinal = UInt::new(ordinal as u64).unwrap();
                let id =
                    ids::syntax_id(&d.source_set, &d.path, d.language, &d.ancestors, &key).unwrap();
                candidates.push((id, header.clone(), d.sibling_headers.clone()));
            }
            let result = ids::evaluate_anchor(
                &prior.expected.anchor,
                &prior.expected.anchor.document,
                &d.revision_id,
                &candidates,
                Some(continuity),
            )
            .unwrap();
            assert_eq!(
                Some(result),
                case.expected.previous_anchor_result,
                "{}",
                case.case_id
            );
        } else {
            assert!(case.continuity.is_none() && case.expected.previous_anchor_result.is_none());
        }
    }
}

fn synthetic_lookup_spellings_and_occurrence_namespaces() {
    let source = Text::new("core").unwrap();
    let path = Path::new("src/a.rs").unwrap();
    let nfc = "é";
    let nfd = "e\u{301}";
    for language in [
        Language::Java,
        Language::Rust,
        Language::Python,
        Language::Javascript,
    ] {
        let mut key: Key = serde_json::from_value(
            json!({"kind":"function","name":nfc,"signature":null,"ordinal":0}),
        )
        .unwrap();
        let first = ids::syntax_id(&source, &path, language, &[], &key).unwrap();
        key.name = Text::new(nfd);
        let second = ids::syntax_id(&source, &path, language, &[], &key).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            ids::lookup_key(language, nfc).unwrap() == ids::lookup_key(language, nfd).unwrap(),
            matches!(language, Language::Rust | Language::Python)
        );
    }
    assert_eq!(ids::lookup_key(Language::Python, "Ａ").unwrap(), "A");
    assert_eq!(ids::lookup_key(Language::Rust, "r#type").unwrap(), "type");
    assert_eq!(ids::lookup_key(Language::Java, "\\u0061").unwrap(), "a");
    assert_eq!(
        ids::lookup_key(Language::Javascript, "\\u{61}").unwrap(),
        "a"
    );
    let owner = SyntaxId::new("sid:v1:00000000000000000000000000000000").unwrap();
    let r1 = Text::new("r1").unwrap();
    let r2 = Text::new("r2").unwrap();
    let zero = UInt::new(0).unwrap();
    let c = ids::occurrence_id(&r1, &owner, OccurrenceKind::Call, zero).unwrap();
    assert_ne!(
        c,
        ids::occurrence_id(&r2, &owner, OccurrenceKind::Call, zero).unwrap()
    );
    assert_ne!(
        c,
        ids::occurrence_id(&r1, &owner, OccurrenceKind::Control, zero).unwrap()
    );
    assert_ne!(
        c,
        ids::occurrence_id(&r1, &owner, OccurrenceKind::Reference, zero).unwrap()
    );
    assert!(ids::lookup_key(Language::Javascript, "\\u{110000}").is_err());
}

fn synthetic_ordinals_and_canonical_control_escapes() {
    let k: Key =
        serde_json::from_value(json!({"kind":"function","name":"x","signature":null,"ordinal":0}))
            .unwrap();
    let entries = vec![
        (vec![], k.clone(), 30, 40),
        (vec![], k.clone(), 10, 20),
        (vec![], k.clone(), 20, 30),
    ];
    assert_eq!(
        ids::sibling_ordinals(&entries)
            .unwrap()
            .iter()
            .map(|n| n.get())
            .collect::<Vec<_>>(),
        [2, 0, 1]
    );
    assert!(ids::sibling_ordinals(&[(vec![], k.clone(), 1, 2), (vec![], k, 1, 2)]).is_err());
    let owner = SyntaxId::new("sid:v1:00000000000000000000000000000000").unwrap();
    let entries = vec![
        (owner.clone(), OccurrenceKind::Call, 4, 5),
        (owner.clone(), OccurrenceKind::Reference, 4, 5),
        (owner.clone(), OccurrenceKind::Call, 1, 2),
    ];
    assert_eq!(
        ids::occurrence_ordinals(&entries)
            .unwrap()
            .iter()
            .map(|n| n.get())
            .collect::<Vec<_>>(),
        [1, 0, 0]
    );
    assert!(
        ids::occurrence_ordinals(&[
            (owner.clone(), OccurrenceKind::Call, 1, 2),
            (owner, OccurrenceKind::Call, 1, 2)
        ])
        .is_err()
    );
    assert_eq!(
        String::from_utf8(ids::canonical_json(&json!({"z":"\n\t\"\\\u{2028}","a":null})).unwrap())
            .unwrap(),
        "{\"a\":null,\"z\":\"\\u000a\\u0009\\\"\\\\\u{2028}\"}"
    );
}
