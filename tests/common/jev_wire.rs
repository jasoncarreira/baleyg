//! Test-only independent decoder for the lossless provider wire format.
use serde_json::{Map, Value};
fn identity(value: &Value, dictionary: &[Value]) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::Array(values) => {
            Value::Array(values.iter().map(|v| identity(v, dictionary)).collect())
        }
        _ => dictionary[value.as_u64().expect("identity index") as usize].clone(),
    }
}
pub fn decode_packet(body: &Value) -> Value {
    let state = &body["state"];
    assert_eq!(state["encoding"], "baleyg-evidence-tables-v1");
    let dictionary = state["identities"].as_array().unwrap();
    let mut packet = state["packet"].clone();
    for path in state["identityPaths"].as_array().unwrap() {
        let cell = packet.pointer_mut(path.as_str().unwrap()).unwrap();
        *cell = identity(cell, dictionary);
    }
    for name in ["nodes", "calls", "regions"] {
        let table = &packet["context"][name];
        let columns = table["columns"].as_array().unwrap();
        let identity_columns = table["identityColumns"].as_array().unwrap();
        let rows = table["rows"].as_array().unwrap();
        let decoded = rows
            .iter()
            .map(|row| {
                let row = row.as_array().unwrap();
                assert_eq!(row.len(), columns.len());
                let object: Map<String, Value> = columns
                    .iter()
                    .zip(row)
                    .map(|(column, value)| {
                        let value = if identity_columns.contains(column) {
                            identity(value, dictionary)
                        } else if column == "range" {
                            let names = state["rangeColumns"].as_array().unwrap();
                            let values = value.as_array().unwrap();
                            assert_eq!(names.len(), values.len());
                            Value::Object(
                                names
                                    .iter()
                                    .zip(values)
                                    .map(|(key, v)| (key.as_str().unwrap().to_owned(), v.clone()))
                                    .collect(),
                            )
                        } else {
                            value.clone()
                        };
                        (column.as_str().unwrap().to_owned(), value)
                    })
                    .collect();
                Value::Object(object)
            })
            .collect();
        packet["context"][name] = Value::Array(decoded);
    }
    packet
}
