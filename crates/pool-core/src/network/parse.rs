use super::types::*;
use crate::Result;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub fn parse_subscription(bytes: &[u8]) -> Result<ParsedSubscription> {
    if bytes.is_empty() || bytes.len() > MAX_SUBSCRIPTION_BYTES {
        return Err("订阅内容应为 1 字节至 8 MiB".into());
    }
    let mut budget = Budget {
        values: 0,
        bytes: 0,
    };
    let document = Bounded {
        budget: &mut budget,
        depth: 0,
    }
    .deserialize(serde_yaml_ng::Deserializer::from_slice(bytes))
    .map_err(|_| "订阅不是有效的 Clash YAML/JSON 配置，或其展开内容超出限制")?;
    let proxies = document
        .get("proxies")
        .and_then(Value::as_array)
        .ok_or("订阅中没有可导入的 proxies 节点列表")?;
    if proxies.len() > MAX_NODES {
        return Err("一个订阅最多支持 1000 个节点".into());
    }
    let mut seen = BTreeSet::new();
    let mut nodes = Vec::new();
    let mut excluded = 0;
    for proxy in proxies {
        match parse_node(proxy) {
            Some(node) if seen.insert(node.id.clone()) => nodes.push(node),
            _ => excluded += 1,
        }
    }
    if nodes.is_empty() {
        return Err("订阅中没有受支持的安全节点；请导入包含具体节点的 Clash 订阅".into());
    }
    Ok(ParsedSubscription { nodes, excluded })
}

// YAML aliases can expand a small input into a huge tree. Bound expansion while
// deserializing, before allocating an unrestricted serde_json::Value tree.
struct Budget {
    values: usize,
    bytes: usize,
}
struct Bounded<'a> {
    budget: &'a mut Budget,
    depth: usize,
}
impl Budget {
    fn charge<E: de::Error>(&mut self, bytes: usize) -> std::result::Result<(), E> {
        self.values = self.values.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes);
        if self.values > 250_000 || self.bytes > 16 * 1024 * 1024 {
            Err(E::custom("subscription expansion limit"))
        } else {
            Ok(())
        }
    }
}
impl<'de> DeserializeSeed<'de> for Bounded<'_> {
    type Value = Value;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Value, D::Error> {
        if self.depth > 32 {
            return Err(de::Error::custom("subscription nesting limit"));
        }
        self.budget.charge::<D::Error>(0)?;
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Bounded<'_> {
    type Value = Value;
    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("bounded subscription data")
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_none<E: de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> std::result::Result<Value, E> {
        Ok(value.into())
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> std::result::Result<Value, E> {
        Ok(value.into())
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> std::result::Result<Value, E> {
        Ok(value.into())
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> std::result::Result<Value, E> {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("invalid number"))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Value, E> {
        self.budget.charge::<E>(value.len())?;
        Ok(value.into())
    }
    fn visit_string<E: de::Error>(self, value: String) -> std::result::Result<Value, E> {
        self.budget.charge::<E>(value.len())?;
        Ok(value.into())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> std::result::Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = access.next_element_seed(Bounded {
            budget: self.budget,
            depth: self.depth + 1,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> std::result::Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            self.budget.charge::<A::Error>(key.len())?;
            if values.contains_key(&key) {
                return Err(de::Error::custom("duplicate subscription key"));
            }
            let value = access.next_value_seed(Bounded {
                budget: self.budget,
                depth: self.depth + 1,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

fn parse_node(proxy: &Value) -> Option<StoredNode> {
    let object = proxy.as_object()?;
    let protocol = object.get("type")?.as_str()?.to_ascii_lowercase();
    let name = clean_name(object.get("name")?.as_str()?)?;
    let server = object.get("server")?.as_str()?;
    if server.is_empty()
        || server.len() > 253
        || server.chars().any(|c| c.is_control() || c.is_whitespace())
        || server.contains(['/', '\\', '@', '#', '?'])
    {
        return None;
    }
    let port = object
        .get("port")
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))?;
    if !(1..=65535).contains(&port) {
        return None;
    }
    if object.get("dialer-proxy").is_some()
        || object.get("proxy").is_some()
        || object.get("skip-cert-verify") == Some(&Value::Bool(true))
    {
        return None;
    }
    let required: &[&str] = match protocol.as_str() {
        "ss" => &["cipher", "password"],
        "vmess" | "vless" => &["uuid"],
        "trojan" | "hysteria2" | "hy2" => &["password"],
        "tuic" => &["uuid", "password"],
        "http" | "socks5" => &[],
        _ => return None,
    };
    if required.iter().any(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .is_none_or(|v| v.is_empty() || v.len() > 16 * 1024)
    }) {
        return None;
    }
    let protocol = if protocol == "hy2" {
        "hysteria2".into()
    } else {
        protocol
    };
    let common = [
        "type",
        "server",
        "port",
        "username",
        "password",
        "udp",
        "tls",
        "sni",
        "servername",
        "alpn",
        "fingerprint",
        "client-fingerprint",
        "network",
        "ip-version",
        "tfo",
        "mptcp",
    ];
    let specific: &[&str] = match protocol.as_str() {
        "ss" => &["cipher", "udp-over-tcp", "udp-over-tcp-version"],
        "vmess" => &[
            "uuid",
            "alterId",
            "cipher",
            "packet-encoding",
            "global-padding",
            "authenticated-length",
        ],
        "vless" => &["uuid", "flow", "packet-encoding", "reality-opts"],
        "hysteria2" => &[
            "obfs",
            "obfs-password",
            "up",
            "down",
            "ports",
            "hop-interval",
        ],
        "tuic" => &[
            "uuid",
            "congestion-controller",
            "udp-relay-mode",
            "reduce-rtt",
            "request-timeout",
            "heartbeat-interval",
            "disable-sni",
            "max-udp-relay-packet-size",
        ],
        _ => &[],
    };
    // Plugin executables, local key/certificate paths, provider URLs and arbitrary
    // nested options never cross into the generated core configuration.
    let mut config = Map::new();
    for key in common.iter().chain(specific.iter()) {
        if let Some(value) = object.get(*key) {
            if value.is_string()
                && value
                    .as_str()
                    .is_some_and(|s| s.len() > 16 * 1024 || s.chars().any(|c| c == '\0'))
            {
                return None;
            }
            if value.is_array() {
                if *key != "alpn"
                    || value.as_array()?.len() > 16
                    || value.as_array()?.iter().any(|v| !v.is_string())
                {
                    return None;
                }
            } else if value.is_object() {
                if *key != "reality-opts" {
                    return None;
                }
                let opts = value.as_object()?;
                if !opts.contains_key("public-key") {
                    return None;
                }
                let mut safe = Map::new();
                for k in ["public-key", "short-id"] {
                    if let Some(v) = opts.get(k) {
                        let s = v.as_str()?;
                        if s.len() > 1024 {
                            return None;
                        }
                        safe.insert(k.into(), v.clone());
                    }
                }
                config.insert((*key).into(), Value::Object(safe));
                continue;
            } else if value.is_null() {
                continue;
            }
            config.insert((*key).into(), value.clone());
        }
    }
    config.insert("type".into(), protocol.clone().into());
    config.insert("port".into(), port.into());
    config.insert("skip-cert-verify".into(), false.into());
    if let Some(network) = config.get("network").and_then(Value::as_str) {
        match network {
            "tcp" => {}
            "ws" => {
                config.insert(
                    "ws-opts".into(),
                    transport_options(
                        object.get("ws-opts"),
                        &[
                            "path",
                            "headers",
                            "max-early-data",
                            "early-data-header-name",
                        ],
                    )?,
                );
            }
            "grpc" => {
                config.insert(
                    "grpc-opts".into(),
                    transport_options(object.get("grpc-opts"), &["grpc-service-name"])?,
                );
            }
            "http" => {
                config.insert(
                    "http-opts".into(),
                    transport_options(object.get("http-opts"), &["path", "method", "headers"])?,
                );
            }
            "h2" => {
                config.insert(
                    "h2-opts".into(),
                    transport_options(object.get("h2-opts"), &["path", "host"])?,
                );
            }
            _ => return None,
        }
    }
    // Refuse SS plugins instead of quietly stripping one and producing a broken node.
    if protocol == "ss" && object.get("plugin").is_some() {
        return None;
    }
    let encoded = serde_json::to_vec(&config).ok()?;
    let id = format!("{:x}", Sha256::digest(&encoded));
    Some(StoredNode {
        id,
        name: name.clone(),
        region: infer_region(&name),
        protocol,
        config: Value::Object(config),
    })
}

fn transport_options(value: Option<&Value>, allowed: &[&str]) -> Option<Value> {
    let mut safe = Map::new();
    if let Some(value) = value {
        let opts = value.as_object()?;
        for key in allowed {
            if let Some(v) = opts.get(*key) {
                if !bounded_value(v, 0) {
                    return None;
                }
                safe.insert((*key).into(), v.clone());
            }
        }
    }
    Some(Value::Object(safe))
}
fn bounded_value(value: &Value, depth: usize) -> bool {
    if depth > 2 {
        return false;
    }
    match value {
        Value::String(s) => s.len() <= 4096 && !s.chars().any(char::is_control),
        Value::Array(v) => v.len() <= 16 && v.iter().all(|v| bounded_value(v, depth + 1)),
        Value::Object(v) => {
            v.len() <= 16
                && v.iter()
                    .all(|(k, v)| k.len() <= 128 && bounded_value(v, depth + 1))
        }
        Value::Number(_) | Value::Bool(_) => true,
        _ => false,
    }
}
fn clean_name(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 512 || name.chars().any(char::is_control) {
        None
    } else {
        Some(name.into())
    }
}
pub fn valid_region(region: &str) -> bool {
    region == "UNKNOWN" || (region.len() == 2 && region.bytes().all(|b| b.is_ascii_uppercase()))
}
pub fn infer_region(name: &str) -> String {
    const REGIONS: &[(&str, &[&str])] = &[
        ("HK", &["🇭🇰", "香港", "hong kong", "hongkong"]),
        ("TW", &["🇹🇼", "台湾", "台灣", "taiwan"]),
        (
            "JP",
            &["🇯🇵", "日本", "东京", "東京", "大阪", "japan", "tokyo"],
        ),
        ("SG", &["🇸🇬", "新加坡", "狮城", "singapore"]),
        (
            "US",
            &[
                "🇺🇸",
                "美国",
                "美國",
                "洛杉矶",
                "洛杉磯",
                "united states",
                "los angeles",
                "america",
            ],
        ),
        (
            "GB",
            &[
                "🇬🇧",
                "英国",
                "英國",
                "伦敦",
                "倫敦",
                "united kingdom",
                "london",
            ],
        ),
        (
            "KR",
            &["🇰🇷", "韩国", "韓國", "首尔", "首爾", "korea", "seoul"],
        ),
        ("DE", &["🇩🇪", "德国", "德國", "germany", "frankfurt"]),
        ("FR", &["🇫🇷", "法国", "法國", "france", "paris"]),
        ("CA", &["🇨🇦", "加拿大", "canada"]),
        ("AU", &["🇦🇺", "澳大利亚", "澳洲", "australia"]),
        ("NL", &["🇳🇱", "荷兰", "荷蘭", "netherlands"]),
        ("IN", &["🇮🇳", "印度", "india"]),
        ("MY", &["🇲🇾", "马来西亚", "馬來西亞", "malaysia"]),
        ("TH", &["🇹🇭", "泰国", "泰國", "thailand"]),
        ("VN", &["🇻🇳", "越南", "vietnam"]),
        ("PH", &["🇵🇭", "菲律宾", "菲律賓", "philippines"]),
        ("ID", &["🇮🇩", "印度尼西亚", "印尼", "indonesia"]),
        ("RU", &["🇷🇺", "俄罗斯", "俄羅斯", "russia"]),
        ("TR", &["🇹🇷", "土耳其", "turkey", "türkiye"]),
        ("BR", &["🇧🇷", "巴西", "brazil"]),
        ("ZA", &["🇿🇦", "南非", "south africa"]),
        ("CH", &["🇨🇭", "瑞士", "switzerland"]),
        ("SE", &["🇸🇪", "瑞典", "sweden"]),
        ("FI", &["🇫🇮", "芬兰", "芬蘭", "finland"]),
        ("NO", &["🇳🇴", "挪威", "norway"]),
        ("IT", &["🇮🇹", "意大利", "italy"]),
        ("ES", &["🇪🇸", "西班牙", "spain"]),
        ("AE", &["🇦🇪", "阿联酋", "迪拜", "dubai"]),
        ("MO", &["🇲🇴", "澳门", "澳門", "macau", "macao"]),
    ];
    let lower = name.to_lowercase();
    // Long country names win over substrings such as 印度 in 印度尼西亚.
    let mut best = None;
    for (code, patterns) in REGIONS {
        for pattern in *patterns {
            if lower.contains(pattern)
                && best
                    .as_ref()
                    .is_none_or(|(_, length)| pattern.len() > *length)
            {
                best = Some((*code, pattern.len()));
            }
        }
    }
    if let Some((code, _)) = best {
        return code.into();
    }
    for token in name.split(|c: char| !c.is_ascii_alphanumeric()) {
        let token = token.to_ascii_uppercase();
        let code = if token == "UK" { "GB" } else { token.as_str() };
        if REGIONS.iter().any(|(country, _)| *country == code) {
            return code.into();
        }
    }
    "UNKNOWN".into()
}
