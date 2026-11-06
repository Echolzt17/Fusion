use super::*;

// Only these identity fields are inspected. Metadata, prompts and images are never persisted.
pub(crate) fn identities(h: &HeaderMap, v: &Value) -> Result<Vec<String>> {
    let mut metadata = Vec::new();
    for raw in [
        h.get("x-codex-turn-metadata").and_then(|s| s.to_str().ok()),
        v["client_metadata"]["x-codex-turn-metadata"].as_str(),
    ]
    .into_iter()
    .flatten()
    {
        if raw.len() > 64 * 1024 {
            return Err("会话元数据超过限制".into());
        }
        metadata.push(serde_json::from_str::<Value>(raw).map_err(|_| "会话元数据格式错误")?);
    }
    let field = |name: &str| -> Result<Option<String>> {
        let values: Vec<_> = metadata
            .iter()
            .filter_map(|m| m[name].as_str())
            .chain(v["client_metadata"][name].as_str())
            .filter(|s| !s.is_empty())
            .collect();
        if values.iter().any(|s| Some(s) != values.first()) {
            return Err("会话元数据标识冲突，已停止发送".into());
        }
        Ok(values.first().map(|s| (*s).to_owned()))
    };
    let mut keys = Vec::new();
    if let Some(thread) = field("thread_id")? {
        keys.push(binding_key("thread", &thread));
    } else if let Some(s) = field("session_id")? {
        keys.push(binding_key("session", &s));
    } else if let Some(s) = session(h) {
        keys.push(s);
    }
    let mut turn = field("turn_id")?;
    if let Some(image_turn) = h.get("x-codex-image-turn-id").and_then(|s| s.to_str().ok()) {
        if turn.as_ref().is_some_and(|t| t != image_turn) {
            return Err("图片轮次与会话轮次冲突".into());
        }
        turn = Some(image_turn.to_owned());
    }
    if let Some(t) = turn.filter(|s| !s.is_empty()) {
        keys.push(binding_key("turn", &t));
    }
    Ok(keys)
}

fn references(v: &Value, keys: &mut Vec<String>) {
    match v {
        Value::Array(a) => {
            for item in a {
                references(item, keys);
            }
        }
        Value::Object(o) => {
            for (name, kind) in [
                ("file_id", "image_file"),
                ("generation_id", "image_generation"),
            ] {
                if let Some(id) = o.get(name).and_then(Value::as_str) {
                    keys.push(binding_key(kind, id));
                }
            }
            if o.get("type").and_then(Value::as_str) == Some("image_generation_call") {
                if let Some(id) = o.get("id").and_then(Value::as_str) {
                    keys.push(binding_key("image_call", id));
                }
            }
            // Descend only into image content, never schemas or function arguments.
            for name in ["content", "images", "output"] {
                if let Some(item) = o.get(name) {
                    references(item, keys);
                }
            }
        }
        _ => {}
    }
}
pub(crate) fn required(h: &HeaderMap, v: &Value) -> Vec<String> {
    let mut keys = opaque_keys(&v["input"]);
    if let Some(id) = v["previous_response_id"].as_str() {
        keys.push(binding_key("response", id));
    }
    references(&v["images"], &mut keys);
    references(&v["input"], &mut keys);
    // A native image request must resolve to the preceding model turn, not an arbitrary account.
    if let Some(t) = h.get("x-codex-image-turn-id").and_then(|s| s.to_str().ok()) {
        keys.push(binding_key("turn", t));
    }
    keys
}
pub(super) fn pin(ctx: &Context, h: &HeaderMap, v: &Value) -> Result<Option<String>> {
    let ids = identities(h, v)?;
    let required = required(h, v);
    let mut pin = None;
    for (key, must_exist) in ids
        .iter()
        .map(|k| (k, false))
        .chain(required.iter().map(|k| (k, true)))
    {
        let account = ctx.accounts.store.binding(key)?;
        if must_exist && account.is_none() {
            return Err(
                "找不到原上下文或图片引用的账号绑定；请恢复原会话或重新提供本地原图".into(),
            );
        }
        if let Some(account) = account {
            if pin.as_ref().is_some_and(|p| p != &account) {
                return Err("上下文与图片账号绑定冲突，已停止发送".into());
            }
            pin = Some(account);
        }
    }
    Ok(pin)
}
pub(crate) fn output_keys(value: &Value) -> Vec<String> {
    let mut keys = opaque_keys(&value["output"]);
    for item in value["output"].as_array().into_iter().flatten() {
        if item["type"] == "image_generation_call" {
            if let Some(id) = item["id"].as_str() {
                keys.push(binding_key("image_call", id));
            }
        }
    }
    for item in value["data"].as_array().into_iter().flatten() {
        if let Some(id) = item["generation_id"].as_str() {
            keys.push(binding_key("image_generation", id));
        }
    }
    keys
}
