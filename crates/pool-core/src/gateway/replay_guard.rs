use super::{continuity, Context};
use crate::{storage::replay_guard::NativeReplayClaim, Result};
use axum::http::HeaderMap;
use serde_json::Value;
use sha2::{Digest, Sha256};

fn fingerprint_seed(headers: &HeaderMap, body: &Value, operation: &str) -> Result<Sha256> {
    let mut identity = continuity::identities(headers, body)?;
    identity.sort_unstable();
    let mut hash = Sha256::new();
    hash.update(b"fusion-native-replay/v1\0");
    bytes(&mut hash, operation.as_bytes());
    // Hashed native thread/session and turn identifiers distinguish a real
    // later user turn. Without identity, keep the conservative body-only key.
    hash.update((identity.len() as u64).to_be_bytes());
    for key in identity { bytes(&mut hash, key.as_bytes()); }
    Ok(hash)
}

fn fingerprints(headers: &HeaderMap, body: &Value, operation: &str) -> Result<(String, Vec<String>)> {
    let seed = fingerprint_seed(headers, body, operation)?;
    let Some(fields) = body.as_object().filter(|_| operation == "responses") else {
        let mut hash = seed;
        canonical(&mut hash, body);
        return Ok((format!("{:x}", hash.finalize()), Vec::new()));
    };
    let on = Value::Bool(true);
    let off = Value::Bool(false);
    let empty = Value::String(String::new());
    let defaults = [("instructions", Some(&empty)), ("store", Some(&off)), ("stream", Some(&on))];
    let instructions = fields.get("instructions").unwrap_or(&empty);
    let mut common: Vec<_> = fields.iter().map(|(key, value)| (key.as_str(), value))
        .filter(|(key, _)| !defaults.iter().any(|(option, _)| option == key)).collect();
    common.sort_unstable_by_key(|(key, _)| *key);
    let prefix = common.partition_point(|(key, _)| *key < "instructions");
    // Cover v1 claims from before Responses transport options were normalized.
    // The large input precedes those options, so share its hash per field count.
    let mut headers_by_size = std::collections::BTreeMap::new();
    let mut digest = |options: [(&str, Option<&Value>); 3]| {
        let field_count = common.len() + options.iter().filter(|(_, value)| value.is_some()).count();
        let mut hash = headers_by_size.entry(field_count).or_insert_with(|| {
            let mut hash = seed.clone();
            hash.update([5]);
            hash.update((field_count as u64).to_be_bytes());
            for (key, value) in &common[..prefix] { field(&mut hash, key, value); }
            hash
        }).clone();
        let mut tail = common[prefix..].to_vec();
        tail.extend(options.into_iter().filter_map(|(key, value)| value.map(|value| (key, value))));
        tail.sort_unstable_by_key(|(key, _)| *key);
        for (key, value) in tail { field(&mut hash, key, value); }
        format!("{:x}", hash.finalize())
    };
    let normalized = digest([("instructions", Some(instructions)), defaults[1], defaults[2]]);
    let mut legacy = vec![digest(defaults.map(|(key, _)| (key, fields.get(key))))];
    let old_instructions = [None, Some(instructions)];
    let offset = usize::from(instructions != &empty);
    for &instruction in &old_instructions[offset..] {
        for store in [None, Some(&off), Some(&on)] {
            for stream in [None, Some(&off), Some(&on)] {
                legacy.push(digest([("instructions", instruction), ("store", store), ("stream", stream)]));
            }
        }
    }
    legacy.retain(|old| old != &normalized);
    legacy.sort_unstable();
    legacy.dedup();
    Ok((normalized, legacy))
}

fn bytes(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_be_bytes());
    hash.update(value);
}

fn field(hash: &mut Sha256, name: &str, value: &Value) {
    bytes(hash, name.as_bytes());
    canonical(hash, value);
}

// Stream a type-tagged, length-delimited canonical representation into SHA256.
// Keep all business fields, independent of object key order.
// Arrays, model/options, readable input, tools and encrypted items
// retain their complete semantics without allocating a second request body.
fn canonical(hash: &mut Sha256, value: &Value) {
    match value {
        Value::Null => hash.update([0]),
        Value::Bool(value) => hash.update([1, u8::from(*value)]),
        Value::Number(value) => { hash.update([2]); bytes(hash, value.to_string().as_bytes()); }
        Value::String(value) => { hash.update([3]); bytes(hash, value.as_bytes()); }
        Value::Array(values) => {
            hash.update([4]); hash.update((values.len() as u64).to_be_bytes());
            for value in values { canonical(hash, value); }
        }
        Value::Object(values) => {
            hash.update([5]); hash.update((values.len() as u64).to_be_bytes());
            let mut fields: Vec<_> = values.iter().collect();
            fields.sort_unstable_by_key(|(key, _)| *key);
            for (name, value) in fields { field(hash, name, value); }
        }
    }
}

/// For authenticated requests with explicit chat and turn identity.
/// The caller decides that scope from the original input, before any projection.
pub(super) fn claim_authenticated(ctx: &Context, headers: &HeaderMap, body: &Value, operation: &str) -> Result<NativeReplayClaim> {
    if !super::authorized(ctx, headers) {
        return Err("本地接入尚未通过鉴权，未保存或发送请求".into());
    }
    let (digest, legacy) = fingerprints(headers, body, operation)?;
    ctx.accounts.store.claim_native_replay(&digest, &legacy)
}

pub(super) fn release(ctx: &Context, claim: &NativeReplayClaim) -> Result<()> {
    ctx.accounts.store.release_native_replay(claim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fingerprint(headers: &HeaderMap, body: &Value, operation: &str) -> Result<String> {
        Ok(fingerprints(headers, body, operation)?.0)
    }

    #[test]
    fn native_replay_canonicalizes_order_without_discarding_business_fields() {
        let headers = HeaderMap::new();
        let left: Value = serde_json::from_str(r#"{"model":"m","input":[{"role":"user","content":"ask"}],"tools":[{"type":"function","name":"a","parameters":{"b":2,"a":1}}],"reasoning":{"effort":"high"},"stream":true,"store":false}"#).unwrap();
        let right: Value = serde_json::from_str(r#"{"store":false,"stream":true,"reasoning":{"effort":"high"},"tools":[{"parameters":{"a":1,"b":2},"name":"a","type":"function"}],"input":[{"content":"ask","role":"user"}],"model":"m"}"#).unwrap();
        let original = fingerprint(&headers, &left, "responses").unwrap();
        assert_eq!(original, fingerprint(&headers, &right, "responses").unwrap());
        assert_ne!(original, fingerprint(&headers, &left, "image_generate").unwrap());
        for (field, value) in [
            ("model", json!("other")), ("instructions", json!("different")),
            ("input", json!([{"role":"user","content":"changed"}])),
            ("tools", json!([])), ("reasoning", json!({"effort":"low"})),
            ("text", json!({"format":{"type":"json_object"}})),
            ("unknown_business_option", json!(true)),
        ] {
            let mut changed = left.clone(); changed[field] = value;
            assert_ne!(original, fingerprint(&headers, &changed, "responses").unwrap(), "{field}");
        }
        assert_eq!(original, fingerprint(&headers, &json!({
            "model":"m","input":[{"role":"user","content":"ask"}],
            "tools":[{"type":"function","name":"a","parameters":{"a":1,"b":2}}],
            "reasoning":{"effort":"high"},"stream":false,"store":true,"instructions":""
        }), "responses").unwrap());
    }

    #[test]
    fn native_replay_allows_later_turns_and_distinct_tool_stages() {
        let mut headers = HeaderMap::new();
        headers.insert("x-codex-turn-metadata", r#"{"thread_id":"public-thread","turn_id":"turn-a"}"#.parse().unwrap());
        let initial = json!({"model":"m","input":[{"role":"user","content":"task"}]});
        let first = fingerprint(&headers, &initial, "responses").unwrap();
        let next_tool = json!({"model":"m","input":[{"role":"user","content":"task"},
            {"type":"function_call","call_id":"call-a","name":"test","arguments":"{}"},
            {"type":"function_call_output","call_id":"call-a","output":"result"}]});
        assert_ne!(first, fingerprint(&headers, &next_tool, "responses").unwrap());
        headers.insert("x-codex-turn-metadata", r#"{"thread_id":"public-thread","turn_id":"turn-b"}"#.parse().unwrap());
        assert_ne!(first, fingerprint(&headers, &initial, "responses").unwrap());
        let no_identity = fingerprint(&HeaderMap::new(), &initial, "responses").unwrap();
        assert_ne!(first, no_identity);
        assert_eq!(no_identity, fingerprint(&HeaderMap::new(), &initial, "responses").unwrap());
    }

    #[test]
    fn equivalent_responses_respect_preexisting_replay_records() {
        let (_dir, accounts) = crate::tests::fixture();
        let ctx = super::super::tests::ctx(accounts.clone(), "http://127.0.0.1:1".into());
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer test-key".parse().unwrap());
        headers.insert("x-codex-turn-metadata", r#"{"thread_id":"public-thread","turn_id":"old-turn"}"#.parse().unwrap());
        let old = json!({"input":[{"role":"user","content":"public task"}],"stream":false});
        let mut hash = fingerprint_seed(&headers, &old, "responses").unwrap();
        canonical(&mut hash, &old);
        accounts.store.claim_native_replay(&format!("{:x}", hash.finalize()), &[]).unwrap();
        let changed = json!({"input":old["input"],"stream":true,"store":true,"instructions":""});
        assert!(claim_authenticated(&ctx, &headers, &changed, "responses").is_err());
    }

    #[test]
    fn native_replay_persists_hashes_and_numbers_without_body_or_identity() {
        let (_dir, accounts) = crate::tests::fixture();
        let ctx = super::super::tests::ctx(accounts.clone(), "http://127.0.0.1:1".into());
        let mut headers = HeaderMap::new();
        headers.insert("session_id", "PRIVATE_SESSION_CANARY".parse().unwrap());
        let body = json!({"model":"m","input":"PRIVATE_BODY_CANARY"});
        assert!(claim_authenticated(&ctx, &headers, &body, "responses").is_err());
        headers.insert("authorization", "Bearer test-key".parse().unwrap());
        let first = claim_authenticated(&ctx, &headers, &body, "responses").unwrap();
        assert!(claim_authenticated(&ctx, &headers, &body, "responses").is_err());
        let value = accounts.store.get(&format!("native-replay/v1/{}", fingerprint(&headers, &body, "responses").unwrap())).unwrap().unwrap();
        assert!(!value.contains("PRIVATE_") && !value.contains("input") && !value.contains("model"));
        let entry: Value = serde_json::from_str(&value).unwrap();
        assert_eq!(entry.as_object().unwrap().len(), 3);
        assert_eq!(entry["version"], 1);
        assert!(entry["claimedAt"].is_i64());
        assert_eq!(entry["ownerHash"].as_str().unwrap().len(), 64);
        release(&ctx, &first).unwrap();
        assert!(claim_authenticated(&ctx, &headers, &body, "responses").is_ok());
    }

    #[test]
    fn native_replay_authenticated_legacy_helper_requires_local_authorization() {
        let (_dir, accounts) = crate::tests::fixture();
        let ctx = super::super::tests::ctx(accounts, "http://127.0.0.1:1".into());
        let mut headers = HeaderMap::new();
        let body = json!({"input":[{"role":"user","content":"public task"}]});
        assert!(claim_authenticated(&ctx, &headers, &body, "responses").is_err());
        headers.insert("authorization", "Bearer test-key".parse().unwrap());
        assert!(claim_authenticated(&ctx, &headers, &body, "responses").is_ok());
        assert!(claim_authenticated(&ctx, &headers, &body, "responses").is_err());
    }
}
