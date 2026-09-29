//! Small helpers for working with JSON the way the Python version did.

use serde_json::Value;

/// Python's truthiness.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// A JSON value as a string: strings as they are, numbers written out, null as "".
pub fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// {k: obj[k]} for the keys whose value isn't None, "" or [] (the Python pick()).
pub fn pick(obj: &Value, keys: &[&str]) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for k in keys {
        if let Some(v) = obj.get(*k) {
            let empty = v.is_null() || v.as_str() == Some("") || v.as_array().map(|a| a.is_empty()).unwrap_or(false);
            if !empty {
                out.insert((*k).to_string(), v.clone());
            }
        }
    }
    out
}

/// JSON with one-space indents and real characters, like json.dumps(data, indent=1, ensure_ascii=False).
pub fn pretty(v: &Value) -> String {
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    serde::Serialize::serialize(v, &mut ser).ok();
    String::from_utf8(buf).unwrap_or_default()
}

/// Random URL-safe token (like secrets.token_urlsafe(n)).
pub fn token_urlsafe(n: usize) -> String {
    use base64::Engine;
    let bytes: Vec<u8> = (0..n).map(|_| rand::random::<u8>()).collect();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
