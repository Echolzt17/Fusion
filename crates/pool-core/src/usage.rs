use serde::{Deserialize, Serialize};
use serde_json::Value;

// Model metadata only. Never retain arbitrary strings supplied in this field.
pub fn model_name(value: &Value) -> Option<String> {
    value.as_str().filter(|s| !s.is_empty() && s.len() <= 128
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b)))
        .map(str::to_owned)
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationUsage {
    pub conversation_key: String,
    pub requests: u64,
    pub measured_requests: u64,
    pub tokens: TokenUsage,
    pub first_observed_at: i64,
    pub last_started_at: i64,
    pub last_account: Option<String>,
    pub history_partial: bool,
}
impl ConversationUsage {
    pub fn record(&mut self, log: &crate::RequestLog) {
        let started = log.started_at.unwrap_or(log.at.saturating_mul(1000));
        if self.requests == 0 || started < self.first_observed_at { self.first_observed_at = started; }
        if self.requests == 0 || started >= self.last_started_at {
            self.last_started_at = started;
            self.last_account = log.account.clone();
        }
        self.requests = self.requests.saturating_add(1);
        if let Some(tokens) = &log.usage {
            self.measured_requests = self.measured_requests.saturating_add(1);
            self.tokens.add(tokens);
        }
    }
}

#[derive(Clone, Default, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}
impl TokenUsage {
    pub fn from_response(v: &Value) -> Option<Self> {
        let v = &v["usage"];
        let input_tokens = v["input_tokens"].as_u64()?;
        let output_tokens = v["output_tokens"].as_u64()?;
        Some(Self {
            input_tokens,
            output_tokens,
            total_tokens: v["total_tokens"]
                .as_u64()
                .unwrap_or(input_tokens.saturating_add(output_tokens)),
            cached_tokens: v["input_tokens_details"]["cached_tokens"].as_u64(),
            reasoning_tokens: v["output_tokens_details"]["reasoning_tokens"].as_u64(),
        })
    }
    pub fn add(&mut self, other: &Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
        for (to, from) in [
            (&mut self.cached_tokens, other.cached_tokens),
            (&mut self.reasoning_tokens, other.reasoning_tokens),
        ] {
            if let Some(n) = from {
                *to = Some(to.unwrap_or(0).saturating_add(n));
            }
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDay {
    pub day: i64,
    pub total_tokens: u64,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsage {
    pub account: String,
    pub requests: u64,
    pub measured_requests: u64,
    pub tokens: TokenUsage,
    pub days: Vec<UsageDay>,
}
impl AccountUsage {
    pub fn record(&mut self, at: i64, usage: Option<&TokenUsage>) {
        self.requests = self.requests.saturating_add(1);
        if let Some(usage) = usage {
            self.measured_requests = self.measured_requests.saturating_add(1);
            self.tokens.add(usage);
            let day = at.div_euclid(86400) * 86400;
            if let Some(d) = self.days.iter_mut().find(|d| d.day == day) {
                d.total_tokens = d.total_tokens.saturating_add(usage.total_tokens);
            } else {
                self.days.push(UsageDay {
                    day,
                    total_tokens: usage.total_tokens,
                });
            }
            self.days.sort_by_key(|d| d.day);
            if let Some(latest) = self.days.last().map(|d| d.day) {
                self.days.retain(|d| d.day >= latest - 6 * 86400);
            }
        }
    }
}
