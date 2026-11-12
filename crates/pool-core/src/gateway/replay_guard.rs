use super::{continuity, Context};
use crate::{storage::replay_guard::NativeReplayClaim, Result};
use axum::http::HeaderMap;
use serde_json::Value;
use sha2::{Digest, Sha256};

fn fingerprint(headers: &HeaderMap, body: &Value, operation: &str) -> Result<String> {
    let mut identity = continuity::identities(headers, body)?;
    identity.sort_unstable();
    let mut hash = Sha256::new();
    hash.update(b"fusion-native-replay/v1\0");
    bytes(&mut hash, operation.as_bytes());
    // Hashed native thread/session and turn identifiers distinguish a real
    // later user turn. Without identity, keep the conservative body-only key.
    hash.update((identity.len() as u64).to_be_bytes());
    for key in identity { bytes(&mut hash, key.as_bytes()); }
    canonical(&mut hash, body);
    Ok(format!("{:x}", hash.finalize()))
}

fn bytes(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_be_bytes());
    hash.update(value);
}

// Stream a type-tagged, length-delimited canonical representation into SHA256.
// Keep every body field: native retries were observed to change object key
// order only. Arrays, model/options, readable input, tools and encrypted items
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
            for (name, value) in fields { bytes(hash, name.as_bytes()); canonical(hash, value); }
        }
    }
}

/// For authenticated requests with explicit chat and turn identity.
/// The caller decides that scope from the original input, before any projection.
pub(super) fn claim_authenticated(ctx: &Context, headers: &HeaderMap, body: &Value, operation: &str) -> Result<NativeReplayClaim> {
    if !super::authorized(ctx, headers) {
        return Err("本地接入尚未通过鉴权，未保存或发送请求".into());
    }
    ctx.accounts.store.claim_native_replay(&fingerprint(headers, body, operation)?)
}

pub(super) fn release(ctx: &Context, claim: &NativeReplayClaim) -> Result<()> {
    ctx.accounts.store.release_native_replay(claim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
            ("stream", json!(false)), ("store", json!(true)),
            ("unknown_business_option", json!(true)),
        ] {
            let mut changed = left.clone(); changed[field] = value;
            assert_ne!(original, fingerprint(&headers, &changed, "responses").unwrap(), "{field}");
        }
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
