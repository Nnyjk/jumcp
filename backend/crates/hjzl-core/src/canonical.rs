//! 规范化 JSON + 哈希：与 Python `json.dumps(sort_keys, separators=(',',':'), ensure_ascii=False)` 逐字节一致。
//! 为什么手写排序：serde_json preserve_order 会保留插入序，必须显式按 key 排序才稳定。

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

fn escape_str(s: &str, out: &mut String) {
    // 与 Python json.dumps(ensure_ascii=False) 对照：仅转义 " \ 和控制字符，其余 UTF-8 原样。
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_value(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        // Python json 整数无小数点；serde_json Number 保持原字面（int 不带小数）。
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => escape_str(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(e, out);
            }
            out.push(']');
        }
        Value::Object(m) => write_object(m, out),
    }
}

fn write_object(m: &Map<String, Value>, out: &mut String) {
    let mut keys: Vec<&String> = m.keys().collect();
    keys.sort();
    out.push('{');
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        escape_str(k, out);
        out.push(':');
        write_value(&m[*k], out);
    }
    out.push('}');
}

/// 规范化串：键排序 + 紧凑分隔 + UTF-8 原样。绝不返回 Err（入参已是 Value）。
pub fn canonical_json(v: &Value) -> String {
    let mut s = String::new();
    write_value(v, &mut s);
    s
}

pub fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// audit 哈希：sha256(canonical(envelope_without_hash) + prev_hash)。
pub fn hash_concat(envelope_without_hash: &Value, prev_hash: &str) -> String {
    sha256_hex(&format!(
        "{}{}",
        canonical_json(envelope_without_hash),
        prev_hash
    ))
}

/// registry definition_hash = sha256(canonical(definition))。
pub fn definition_hash(definition: &Value) -> String {
    sha256_hex(&canonical_json(definition))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn python_vector_envelope() {
        let v = json!({
            "event_id":"e1","ts":"2026-11-05T00:00:00+00:00","tenant":"t1",
            "actor":{"type":"agent","id":"a-1","role":"analyst"},"session_id":"s-1",
            "event_type":"session_init","payload":{"groups":["db.read.core"]},"prev_hash":""
        });
        assert_eq!(
            canonical_json(&v),
            r#"{"actor":{"id":"a-1","role":"analyst","type":"agent"},"event_id":"e1","event_type":"session_init","payload":{"groups":["db.read.core"]},"prev_hash":"","session_id":"s-1","tenant":"t1","ts":"2026-11-05T00:00:00+00:00"}"#
        );
        assert_eq!(
            hash_concat(&v, ""),
            "23ee83350786f57df8e4ca2d7208ff383a7d1f509e3c88cdade4b7bbc2c76626"
        );
    }

    #[test]
    fn python_vector_definition() {
        let v = json!({"description":"x","params":[]});
        assert_eq!(canonical_json(&v), r#"{"description":"x","params":[]}"#);
        assert_eq!(
            definition_hash(&v),
            "009950a2cde95868b80be3d3046f86ae40d247ace2375351a1f98265b75cb900"
        );
    }

    #[test]
    fn python_vector_unicode_key_order() {
        let v = json!({"b":1,"a":"订单"});
        assert_eq!(canonical_json(&v), r#"{"a":"订单","b":1}"#);
    }
}
