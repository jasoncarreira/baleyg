use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

// Independent serializer: this test deliberately does not call production native_ids.
fn encode(v: &Value) -> Vec<u8> {
    match v {
        Value::Null => b"null".to_vec(),
        Value::Bool(true) => b"true".to_vec(),
        Value::Bool(false) => b"false".to_vec(),
        Value::Number(n) => n.to_string().into_bytes(),
        Value::String(s) => {
            let mut out = vec![b'"'];
            for c in s.chars() {
                match c {
                    '"' => out.extend(b"\\\""),
                    '\\' => out.extend(b"\\\\"),
                    '\u{0}'..='\u{1f}' => out.extend(format!("\\u{:04x}", c as u32).bytes()),
                    _ => {
                        let mut b = [0; 4];
                        out.extend(c.encode_utf8(&mut b).bytes());
                    }
                }
            }
            out.push(b'"');
            out
        }
        Value::Array(items) => {
            let mut out = vec![b'['];
            for (i, item) in items.iter().enumerate() {
                if i != 0 {
                    out.push(b',');
                }
                out.extend(encode(item));
            }
            out.push(b']');
            out
        }
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            let mut out = vec![b'{'];
            for (i, (key, value)) in entries.iter().enumerate() {
                if i != 0 {
                    out.push(b',');
                }
                out.extend(encode(&Value::String((*key).clone())));
                out.push(b':');
                out.extend(encode(value));
            }
            out.push(b'}');
            out
        }
    }
}

#[test]
fn every_normative_stable_id_vector() {
    let root: Value = serde_json::from_str(include_str!(
        "../docs/semantic-evidence/id-test-vectors/stable-ids.json"
    ))
    .unwrap();
    let mut languages = BTreeMap::new();
    let cases = root["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 64);
    for case in cases {
        let language = case["language"].as_str().unwrap();
        *languages.entry(language).or_insert(0usize) += 1;
        let descriptor = &case["descriptor"];
        let input = json!({
            "sourceSet":descriptor["sourceSet"], "path":descriptor["path"],
            "language":descriptor["language"], "ancestors":descriptor["ancestors"],
            "declaration":descriptor["declaration"]
        });
        let bytes = encode(&input);
        let syntax = case["digests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|digest| digest["label"] == "syntax")
            .unwrap();
        assert_eq!(
            hex::encode(&bytes),
            syntax["inputHex"].as_str().unwrap(),
            "{} canonical bytes",
            case["caseId"]
        );
        let full = Sha256::digest([b"baleyg.syntax.v1\0".as_slice(), bytes.as_slice()].concat());
        let expected = format!("sid:v1:{}", hex::encode(&full[..16]));
        assert_eq!(
            expected,
            case["expected"]["stableId"].as_str().unwrap(),
            "{} literal stable ID",
            case["caseId"]
        );
        assert_eq!(
            hex::encode(full),
            syntax["sha256"].as_str().unwrap(),
            "{} full digest",
            case["caseId"]
        );
    }
    assert_eq!(
        languages,
        BTreeMap::from([
            ("java", 16),
            ("rust", 16),
            ("python", 16),
            ("javascript", 16)
        ])
    );
}
