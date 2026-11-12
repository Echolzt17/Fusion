use super::*;
use std::collections::HashMap;

/// A new local user turn may carry its complete, readable history to another
/// subscription. Server-side references, compaction and unfinished tools cannot.
/// This projection exists only for the lifetime of the incoming request.
pub(super) fn project(v: &Value) -> Option<Value> {
    project_checked(v).ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BlockReason {
    ServerReference,
    MissingHistory,
    NotNewUserTurn,
    EncryptedHistory,
    UnsupportedControl,
    UnsupportedContent,
    IncompleteTools,
    UnsupportedItem,
}

pub(super) fn project_checked(v: &Value) -> std::result::Result<Value, BlockReason> {
    if ["previous_response_id", "conversation"].iter().any(|k| !v[*k].is_null()) {
        return Err(BlockReason::ServerReference);
    }
    // Responses accepts a string as a complete user input. Normalize it before
    // deciding whether the request may use a soft chat preference, just as the
    // gateway does for the upstream wire body.
    let normalized = v["input"].as_str().map(|text| vec![
        json!({"role":"user","content":[{"type":"input_text","text":text}]})
    ]);
    let input = normalized.as_ref().or_else(|| v["input"].as_array())
        .ok_or(BlockReason::MissingHistory)?;
    let latest = input.iter().rev().find(|item| !matches!(item["type"].as_str(),
        Some("additional_tools" | "configuration_update")))
        .ok_or(BlockReason::MissingHistory)?;
    if latest["role"] != "user" {
        return Err(BlockReason::NotNewUserTurn);
    }
    let mut calls = HashMap::new();
    let mut output = Vec::with_capacity(input.len());
    for item in input {
        let mut item = item.clone();
        let kind = item["type"].as_str().unwrap_or("message");
        if kind != "reasoning" && !item["encrypted_content"].is_null()
            || !item["encrypted_function_args"].is_null() {
            return Err(BlockReason::EncryptedHistory);
        }
        match kind {
            "reasoning" => continue,
            "configuration_update" => {
                // Native durable reasoning-effort control is interpreted at
                // this position in history. It is not a user turn or an opaque
                // model response. Preserve exactly the schema verified on the
                // installed engine; unknown control variants stay hard-pinned.
                let control = item.as_object().ok_or(BlockReason::UnsupportedControl)?;
                let reasoning = item["reasoning"].as_object().ok_or(BlockReason::UnsupportedControl)?;
                if control.len() != 2 || reasoning.len() != 1
                    || !reasoning["effort"].as_str().is_some_and(|effort| !effort.is_empty()) {
                    return Err(BlockReason::UnsupportedControl);
                }
                output.push(item);
                continue;
            }
            // Responses Lite carries native tool declarations in input rather
            // than top-level tools. These are schemas, not account references;
            // never interpret property names inside a schema as response ids.
            "additional_tools" => {
                if !item["tools"].is_array() || item["role"] != "developer" {
                    return Err(BlockReason::UnsupportedControl);
                }
                output.push(item);
                continue;
            }
            "message" => {
                if !matches!(item["role"].as_str(), Some("user" | "assistant" | "system" | "developer"))
                    || !portable_content(&item["content"]) {
                    return Err(BlockReason::UnsupportedContent);
                }
            }
            "agent_message" => {
                let content = item["content"].as_array().ok_or(BlockReason::UnsupportedContent)?;
                if content.iter().any(|part| part["type"] == "encrypted_content"
                    || !part["encrypted_content"].is_null()) {
                    return Err(BlockReason::EncryptedHistory);
                }
                // The native protocol distinguishes plaintext agent messages
                // from encrypted ones. Keep the explicit sender, recipient and
                // every text part; never flatten an encrypted agent handoff.
                if !["author", "recipient"].iter().all(|key|
                    item[*key].as_str().is_some_and(|value| !value.is_empty()))
                    || content.is_empty()
                    || !content.iter().all(|part| part["type"] == "input_text" && part["text"].is_string()) {
                    return Err(BlockReason::UnsupportedContent);
                }
            }
            "function_call" | "custom_tool_call" => {
                let id = item["call_id"].as_str().filter(|s| !s.is_empty())
                    .ok_or(BlockReason::IncompleteTools)?.to_owned();
                if calls.insert(id, kind.to_owned()).is_some()
                    || item["name"].as_str().is_none()
                    || if kind == "function_call" { item["arguments"].as_str().is_none() }
                       else { item["input"].as_str().is_none() } {
                    return Err(BlockReason::IncompleteTools);
                }
            }
            "function_call_output" | "custom_tool_call_output" => {
                let expected = if kind == "function_call_output" { "function_call" } else { "custom_tool_call" };
                let named_external = kind == "function_call_output" && item["call_id"].is_null()
                    && ["name", "namespace"].iter().all(|key|
                        item[*key].as_str().is_some_and(|value| !value.is_empty()));
                // The native ResponseItem schema also carries completed,
                // explicitly named external tool results without call_id.
                // Keep both routing fields and the full readable result;
                // unnamed orphan outputs still require a matching call.
                if !named_external {
                    let id = item["call_id"].as_str().ok_or(BlockReason::IncompleteTools)?;
                    if calls.remove(id).as_deref() != Some(expected) {
                        return Err(BlockReason::IncompleteTools);
                    }
                }
                if !portable_content(&item["output"]) {
                    return Err(BlockReason::UnsupportedContent);
                }
            }
            _ => return Err(BlockReason::UnsupportedItem),
        }
        let object = item.as_object_mut().ok_or(BlockReason::UnsupportedItem)?;
        // Item ids are upstream references; call_id is the explicit local pair.
        object.remove("id");
        object.remove("encrypted_content");
        output.push(item);
    }
    if !calls.is_empty() { return Err(BlockReason::IncompleteTools); }
    let mut projected = v.clone();
    projected["input"] = Value::Array(output);
    Ok(projected)
}

fn portable_content(value: &Value) -> bool {
    if value.is_string() { return true; }
    let Some(parts) = value.as_array() else { return false; };
    parts.iter().all(|part| match part["type"].as_str() {
        Some("input_text" | "output_text" | "text") => part["text"].is_string(),
        Some("input_image") => part["file_id"].is_null()
            && part["image_url"].as_str().is_some_and(|s| s.starts_with("data:image/")),
        _ => false,
    })
}

/// After a verified chat rollover, the desktop still holds old reasoning items
/// in its local transcript. Remove only those foreign reasoning items; current
/// account reasoning and all tool calls/results retain their native structure.
pub(super) fn sanitize_rolled_history(ctx: &Context, h: &mut HeaderMap, v: &mut Value) -> Result<()> {
    let (keys, preferred) = chat_preference(ctx, h, v)?;
    let Some(account) = preferred else { return Ok(()); };
    for key in continuity::identities(h, v)?.iter().filter(|key| key.starts_with("turn:")) {
        if ctx.accounts.store.binding(key)?.is_some_and(|owner| owner != account) {
            // A late tool result from the previous account's turn is never
            // reinterpreted using the newer chat preference.
            return Ok(());
        }
    }
    let rolled = keys.iter().any(|key| ctx.accounts.store.get(&format!("portable-chat/v1/{key}"))
        .is_ok_and(|stored| stored.as_deref() == Some(account.as_str())));
    if !rolled { return Ok(()); }
    h.remove("x-codex-turn-state");
    let Some(input) = v["input"].as_array_mut() else { return Ok(()); };
    let mut keep = Vec::with_capacity(input.len());
    for mut item in std::mem::take(input) {
        if item["type"] == "reasoning" {
            if let Some(encrypted) = item["encrypted_content"].as_str() {
                let owner = ctx.accounts.store.binding(&binding_key("opaque", encrypted))?;
                if owner.as_deref().is_some_and(|owner| owner != account) { continue; }
            }
        }
        if matches!(item["type"].as_str().unwrap_or("message"),
            "message" | "agent_message" | "function_call" | "function_call_output" | "custom_tool_call" | "custom_tool_call_output") {
            if let Some(object) = item.as_object_mut() { object.remove("id"); }
        }
        keep.push(item);
    }
    *input = keep;
    Ok(())
}

pub(super) fn chat_preference(ctx: &Context, h: &HeaderMap, v: &Value) -> Result<(Vec<String>, Option<String>)> {
    let keys: Vec<_> = continuity::identities(h, v)?.into_iter()
        .filter(|key| key.starts_with("thread:") || key.starts_with("session:")).collect();
    let mut preferred = None;
    for key in &keys {
        if let Some(account) = ctx.accounts.store.binding(key)? {
            if preferred.as_ref().is_some_and(|p| p != &account) {
                return Err("本地聊天账号状态冲突，未发送请求".into());
            }
            preferred = Some(account);
        }
    }
    Ok((keys, preferred))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_user_input_is_projected_before_routing_without_losing_options() {
        for text in ["", "public user task"] {
            let source = json!({"input":text,"model":"public-model","instructions":"public rules",
                "reasoning":{"effort":"high"},"stream":false});
            let projected = project_checked(&source).unwrap();
            let mut expected = source.clone();
            expected["input"] = json!([{"role":"user","content":[{"type":"input_text","text":text}]}]);
            assert_eq!(projected, expected);
            assert_eq!(source["input"], text, "projection does not rewrite the incoming value");
        }
        assert_eq!(project_checked(&json!({"input":"task","previous_response_id":"old-response"})),
            Err(BlockReason::ServerReference));
    }

    #[test]
    fn encrypted_function_arguments_are_never_projected_to_another_account() {
        let mut source = json!({"input":[
            {"type":"function_call","call_id":"read","name":"read","arguments":"{}"},
            {"type":"function_call_output","call_id":"read","output":"public result"},
            {"role":"user","content":"next task"}
        ]});
        assert!(project_checked(&source).is_ok());
        source["input"][0]["encrypted_function_args"] = Value::Null;
        assert!(project_checked(&source).is_ok());
        for encrypted in [json!([]), json!(["PUBLIC_FAKE_ENCRYPTED_ARGUMENTS"])] {
            source["input"][0]["encrypted_function_args"] = encrypted;
            assert_eq!(project_checked(&source), Err(BlockReason::EncryptedHistory));
            assert!(project(&source).is_none());
        }
    }

    #[test]
    fn plaintext_agent_messages_preserve_native_routing_and_encrypted_messages_block_rollover() {
        let message = json!({"type":"agent_message","id":"public-agent-message","author":"public-agent-a",
            "recipient":"public-agent-b","content":[
                {"type":"input_text","text":"public result"},
                {"type":"input_text","text":"public followup"}],
            "internal_chat_message_metadata_passthrough":{"turn_id":"public-agent-turn"}});
        let mut source = json!({"input":[message,{"role":"user","content":"next user task"}]});
        let projected = project_checked(&source).unwrap();
        let mut expected = message.clone();
        expected.as_object_mut().unwrap().remove("id");
        assert_eq!(projected["input"][0], expected);
        assert_eq!(source["input"][0]["id"], "public-agent-message");
        source["input"][0]["content"].as_array_mut().unwrap().push(
            json!({"type":"encrypted_content","encrypted_content":"PUBLIC_FAKE_AGENT_CIPHERTEXT"}));
        assert_eq!(project_checked(&source), Err(BlockReason::EncryptedHistory));
        source["input"][0]["content"] = json!([{"type":"unknown_agent_content","text":"public"}]);
        assert_eq!(project_checked(&source), Err(BlockReason::UnsupportedContent));
        source["input"][0]["content"] = message["content"].clone();
        source["input"][0]["recipient"] = Value::Null;
        assert_eq!(project_checked(&source), Err(BlockReason::UnsupportedContent));
    }

    #[test]
    fn named_external_tool_outputs_without_call_id_keep_their_complete_native_structure() {
        let output = json!({"type":"function_call_output","id":"public-external-output",
            "name":"public_fixture_read","namespace":"functions","output":[
                {"type":"input_text","text":"public external result"}]});
        let source = json!({"input":[output,{"role":"user","content":"next user task"}]});
        let projected = project_checked(&source).unwrap();
        let mut expected = output.clone(); expected.as_object_mut().unwrap().remove("id");
        assert_eq!(projected["input"][0], expected);
        for (field, value) in [
            ("name", Value::Null), ("namespace", json!("")), ("call_id", json!("")),
            ("call_id", json!("unmatched-call")),
        ] {
            let mut invalid = source.clone(); invalid["input"][0][field] = value;
            assert_eq!(project_checked(&invalid), Err(BlockReason::IncompleteTools));
        }
        let mut encrypted = source.clone();
        encrypted["input"][0]["encrypted_content"] = json!("PUBLIC_FAKE_ENCRYPTED_OUTPUT");
        assert_eq!(project_checked(&encrypted), Err(BlockReason::EncryptedHistory));
        let mut incomplete = source.clone();
        incomplete["input"].as_array_mut().unwrap().insert(0,
            json!({"type":"function_call","call_id":"pending","name":"run","arguments":"{}"}));
        assert_eq!(project_checked(&incomplete), Err(BlockReason::IncompleteTools));
    }

    #[test]
    fn blocked_projection_reports_structure_without_guessing_completed_tool_support() {
        for (source, reason) in [
            (json!({"input":[]}), BlockReason::MissingHistory),
            (json!({"input":[{"role":"user","content":"task"},
                {"type":"reasoning","encrypted_content":"PUBLIC_FAKE_CURRENT_REASONING"}]}), BlockReason::NotNewUserTurn),
            (json!({"input":[{"role":"user","content":[{"type":"input_file","file_id":"public-file"}]}]}),
                BlockReason::UnsupportedContent),
            (json!({"input":[{"type":"function_call","call_id":"run","name":"run","arguments":"{}"},
                {"role":"user","content":"next task"}]}), BlockReason::IncompleteTools),
            (json!({"input":[{"role":"user","content":"task"},
                {"type":"configuration_update","reasoning":{"effort":"high","unknown":"control"}}]}),
                BlockReason::UnsupportedControl),
        ] {
            assert_eq!(project_checked(&source), Err(reason));
        }
        for item in [
            json!({"type":"web_search_call","id":"public-search","status":"completed",
                "action":{"type":"search","query":"public query"}}),
            json!({"type":"local_shell_call","call_id":"public-shell","status":"completed",
                "action":{"type":"exec","command":["public-fixture"]}}),
            json!({"type":"tool_search_output","call_id":"public-tools","status":"completed",
                "execution":"client","tools":[]}),
        ] {
            assert_eq!(project_checked(&json!({"input":[item,{"role":"user","content":"next task"}]})),
                Err(BlockReason::UnsupportedItem));
        }
    }

    #[test]
    fn native_configuration_update_is_preserved_without_becoming_a_user_turn() {
        let control = json!({"type":"configuration_update","reasoning":{"effort":"xhigh"}});
        let source = json!({"input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"task"}]},control]});
        assert_eq!(project(&source).unwrap(), source);
        let continuation = json!({"input":[
            {"role":"user","content":"task"},
            {"type":"function_call","call_id":"read","name":"read","arguments":"{}"},
            {"type":"function_call_output","call_id":"read","output":"already read"},control
        ]});
        assert!(project(&continuation).is_none(), "a tool continuation is still the same hard-bound turn");
        assert!(project(&json!({"input":[{"role":"user","content":"task"},
            {"type":"configuration_update","reasoning":{"effort":"high","unknown_state":"opaque"}}]})).is_none());
    }
    #[test]
    fn projection_preserves_native_local_compaction_user_summary_and_followup() {
        // Actual 0.167.0-alpha.3.2 local-compaction shape, observed with an
        // isolated home, public fake login and loopback-only Responses server.
        // The model's assistant summary becomes a readable user message; its
        // compaction.summary marker is metadata, not an opaque compaction item.
        let summary = concat!(
            "Another language model started to solve this problem and produced a summary of its thinking process. ",
            "You also have access to the state of the tools that were used by that language model. ",
            "Use this to build on the work that has already been done and avoid duplicating work. ",
            "Here is the summary produced by the other language model, use the information in this summary to assist with your own analysis:\n",
            "PUBLIC LOCAL COMPACTION SUMMARY"
        );
        let source = json!({"model":"public-fixture-model","input":[
            {"type":"message","role":"developer","content":[{"type":"input_text","text":"Public fixture instructions"}]},
            {"type":"message","role":"developer","content":[{"type":"input_text","text":"Public fixture tools and permissions"}]},
            {"type":"message","role":"user","content":[{"type":"input_text","text":"Public fixture environment"}]},
            {"type":"message","role":"user","content":[{"type":"input_text","text":"PUBLIC FIXTURE INITIAL TURN"}]},
            {"type":"message","id":"msg_public-local-summary","role":"user",
                "content":[{"type":"input_text","text":summary}],
                "internal_chat_message_metadata_passthrough":{
                    "turn_id":"0199b551-3c14-7431-91aa-195be1881d11",
                    "content_item_kinds":["compaction.summary"]}},
            {"type":"message","role":"user","content":[{"type":"input_text","text":"PUBLIC FIXTURE FOLLOWUP"}]}
        ]});
        let projected = project(&source).expect("native readable summary must remain portable");
        let input = projected["input"].as_array().unwrap();
        assert_eq!(input.len(), 6);
        assert_eq!(input[4]["role"], "user");
        assert_eq!(input[4]["content"][0]["text"], summary);
        assert_eq!(input[4]["internal_chat_message_metadata_passthrough"]["content_item_kinds"], json!(["compaction.summary"]));
        assert_eq!(input[5]["content"][0]["text"], "PUBLIC FIXTURE FOLLOWUP");
        assert!(input.iter().all(|item| item["type"] == "message" && item["encrypted_content"].is_null()));
        assert!(input[4]["id"].is_null());
        assert_eq!(source["input"][4]["id"], "msg_public-local-summary");
    }
    #[test]
    fn projection_keeps_readable_history_and_completed_tools_without_foreign_state() {
        let source = json!({"input":[
            {"type":"additional_tools","id":"local-schema","role":"developer","tools":[{"type":"function","name":"read","parameters":{"type":"object","properties":{"file_id":{"type":"string"}}}}]},
            {"role":"user","content":"first"},
            {"type":"reasoning","encrypted_content":"PRIVATE_ENCRYPTED"},
            {"type":"function_call","id":"upstream-id","call_id":"local-call","name":"read","arguments":"{}"},
            {"type":"function_call_output","call_id":"local-call","output":"already read"},
            {"role":"assistant","id":"old-message","phase":"final_answer","content":[{"type":"output_text","text":"done"}]},
            {"role":"user","content":"continue"}
        ]});
        let projected = project(&source).unwrap();
        assert_eq!(projected["input"].as_array().unwrap().len(), 6);
        assert_eq!(projected["input"][0], source["input"][0]);
        assert_eq!(projected["input"][2]["call_id"], "local-call");
        assert_eq!(projected["input"][3]["output"], "already read");
        assert_eq!(projected["input"][4]["phase"], "final_answer");
        let wire = projected.to_string();
        assert!(!wire.contains("PRIVATE_ENCRYPTED") && !wire.contains("upstream-id") && !wire.contains("old-message"));
        assert_eq!(source["input"][2]["encrypted_content"], "PRIVATE_ENCRYPTED");
    }
    #[test]
    fn projection_refuses_missing_history_account_references_and_unfinished_tools() {
        for input in [
            json!([{"type":"compaction","encrypted_content":"opaque"},{"role":"user","content":"next"}]),
            json!([{"type":"function_call","call_id":"x","name":"run","arguments":"{}"},{"role":"user","content":"next"}]),
            json!([{"type":"function_call_output","call_id":"x","output":"result"},{"role":"user","content":"next"}]),
            json!([{"role":"user","content":[{"type":"input_file","file_id":"account-file"}]}]),
            json!([{"role":"user","content":[{"type":"input_image","image_url":"https://private.invalid/image"}]}]),
        ] { assert!(project(&json!({"input":input})).is_none()); }
        assert!(project(&json!({"input":[{"role":"user","content":"next"}],"previous_response_id":"server-response"})).is_none());
    }
}
