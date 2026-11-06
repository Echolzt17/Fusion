//! Labels follow OpenAI Codex a29c7b97e88367044ae84c28c85451ec3e72af9f,
//! protocol/src/auth.rs and tui/src/subscription.rs. See THIRD_PARTY_NOTICES.md.
use crate::{account::{claims, Tokens}, Account};
use serde::Serialize;
use serde_json::Value;

/// Only non-secret subscription metadata is persisted or sent to the UI.
/// An authorization claim is a snapshot, not a live billing confirmation.
#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Expiry {
    expires_at: i64,
    checked_at: Option<i64>,
}

fn timestamp(value: &Value) -> Option<i64> {
    let number = value.as_i64().or_else(|| value.as_str()?.parse().ok());
    let seconds = number.map(|n| if n >= 1_000_000_000_000 { n / 1000 } else { n })
        .or_else(|| chrono::DateTime::parse_from_rfc3339(value.as_str()?.trim()).ok().map(|d| d.timestamp()))?;
    (946_684_800..=253_402_300_799).contains(&seconds).then_some(seconds)
}

/// Protocol reference: Cockpit v1.3.75 codex_account_token_refresh.rs.
/// Require both account and plan to match the freshly queried quota. Never use
/// token exp, quota reset_at or a different workspace's subscription date.
pub(crate) fn expiry(tokens: &Tokens, account_id: &str, plan: &str) -> Option<Expiry> {
    if plan.is_empty() || plan.eq_ignore_ascii_case("free") { return None; }
    [&tokens.id_token, &tokens.access_token].into_iter().filter_map(|token| {
        let payload = claims(token).ok()?;
        let auth = &payload["https://api.openai.com/auth"];
        if auth["chatgpt_account_id"].as_str()? != account_id
            || !auth["chatgpt_plan_type"].as_str()?.eq_ignore_ascii_case(plan) { return None; }
        Some(Expiry {
            expires_at: timestamp(&auth["chatgpt_subscription_active_until"])?,
            checked_at: timestamp(&auth["chatgpt_subscription_last_checked"]),
        })
    }).max_by_key(|snapshot| snapshot.checked_at.unwrap_or(0))
}

pub fn code(account: &Account) -> &str {
    account
        .quota
        .as_ref()
        .and_then(|q| q["planType"].as_str())
        .unwrap_or("")
}
pub fn rank(account: &Account) -> Option<u8> {
    match code(account).to_ascii_lowercase().as_str() {
        "free" => Some(0),
        "go" => Some(1),
        "plus" => Some(2),
        "prolite" => Some(3),
        "pro" => Some(4),
        "promax" => Some(5),
        _ => None,
    }
}
pub fn label(account: &Account) -> &'static str {
    match code(account).to_ascii_lowercase().as_str() {
        "free" => "Free",
        "go" => "Go",
        "plus" => "Plus",
        "prolite" => "Pro 100",
        "pro" => "Pro 200",
        "promax" => "Pro 500",
        "team" | "self_serve_business_usage_based" => "Business",
        "self_serve_business_prolite" => "Business Premium",
        "business" | "ent26" | "enterprise" | "hc" | "enterprise_cbp_usage_based" => "Enterprise",
        "enterprise_cbp_automation" => "Enterprise (Automation)",
        "edu" | "education" => "Edu",
        "edu_plus" => "Edu Plus",
        "edu_pro" => "Edu Pro",
        "sci" => "Academic Researchers",
        "sci_plus" => "Academic Researchers Plus",
        _ => "套餐未知",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use serde_json::json;

    fn jwt(auth: Value) -> String {
        format!("fixture.{}.signature", URL_SAFE_NO_PAD.encode(json!({"exp":1893456000,"https://api.openai.com/auth":auth}).to_string()))
    }
    fn auth(until: Value) -> Value {
        json!({"chatgpt_account_id":"account-a","chatgpt_plan_type":"plus","chatgpt_subscription_active_until":until,"chatgpt_subscription_last_checked":"2026-11-03T04:00:00Z"})
    }
    #[test]
    fn expiry_normalizes_dates_and_never_uses_token_or_quota_expiration() {
        for input in [json!(1793750400), json!(1793750400000_i64), json!("1793750400"), json!("2026-11-04T00:00:00Z"), json!("2026-11-04T08:00:00+08:00")] {
            assert_eq!(timestamp(&input), Some(1793750400));
        }
        for input in [Value::Null, json!(false), json!(""), json!("2026-02-30T00:00:00Z"), json!("2026-11-04"), json!(-1), json!(1e100)] {
            assert_eq!(timestamp(&input), None);
        }
        let mut tokens = Tokens { access_token: "fixture".into(), id_token: jwt(auth(json!("2026-11-04T00:00:00Z"))), refresh_token: "fixture".into(), expires_at: 1893456000 };
        let result = expiry(&tokens, "account-a", "plus").unwrap();
        assert_eq!(result.expires_at, 1793750400);
        assert_eq!(result.checked_at, Some(1793678400));
        assert!(expiry(&tokens, "different-workspace", "plus").is_none());
        assert!(expiry(&tokens, "account-a", "pro").is_none());
        assert!(expiry(&tokens, "account-a", "free").is_none());
        assert!(expiry(&tokens, "account-a", "").is_none());
        tokens.id_token = jwt(auth(Value::Null));
        assert!(expiry(&tokens, "account-a", "plus").is_none());
        tokens.access_token = jwt(auth(json!(1793750400)));
        assert_eq!(expiry(&tokens, "account-a", "plus").unwrap().expires_at, 1793750400);
        let mut older = auth(json!(1793635200));
        older["chatgpt_subscription_last_checked"] = json!("2026-11-01T00:00:00Z");
        tokens.id_token = jwt(older);
        assert_eq!(expiry(&tokens, "account-a", "plus").unwrap().expires_at, 1793750400);
    }
}
