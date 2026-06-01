use std::collections::BTreeMap;

use axum::http::HeaderMap;
use reqwest::header::HeaderMap as ReqwestHeaderMap;
use serde_json::Value;

pub const REDACTED: &str = "<redacted>";

pub fn redact_json_value(value: &Value) -> Value {
    redact_json_value_with_key(None, value)
}

fn redact_json_value_with_key(key: Option<&str>, value: &Value) -> Value {
    if key.is_some_and(is_prompt_content_key) && matches!(value, Value::String(_)) {
        return Value::String(REDACTED.to_owned());
    }
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, child)| {
                    if is_secret_key(key) {
                        (key.clone(), Value::String(REDACTED.to_owned()))
                    } else {
                        (key.clone(), redact_json_value_with_key(Some(key), child))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_json_value_with_key(key, item))
                .collect(),
        ),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value.clone(),
    }
}

pub fn redact_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .map(|(name, value)| {
            let name = name.as_str().to_ascii_lowercase();
            let value = if is_secret_key(&name) {
                REDACTED.to_owned()
            } else {
                value.to_str().unwrap_or("<non-utf8>").to_owned()
            };
            (name, value)
        })
        .collect()
}

pub fn redact_reqwest_headers(headers: &ReqwestHeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .map(|(name, value)| {
            let name = name.as_str().to_ascii_lowercase();
            let value = if is_secret_key(&name) {
                REDACTED.to_owned()
            } else {
                value.to_str().unwrap_or("<non-utf8>").to_owned()
            };
            (name, value)
        })
        .collect()
}

pub fn redaction_findings(value: &Value) -> Vec<String> {
    let mut findings = Vec::new();
    collect_findings(value, "$", &mut findings);
    findings
}

pub fn is_secret_key(name: &str) -> bool {
    let normalized: String = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();

    matches!(
        normalized.as_str(),
        "apikey"
            | "xapikey"
            | "api"
            | "authorization"
            | "proxyauthorization"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "password"
            | "passwd"
            | "secret"
            | "clientsecret"
            | "cookie"
            | "setcookie"
            | "credential"
            | "credentials"
            | "privatekey"
            | "sessiontoken"
    ) || normalized.contains("apikey")
        || normalized.contains("token")
        || normalized.contains("secret")
        || normalized.contains("password")
        || normalized.contains("credential")
        || normalized.contains("authorization")
        || normalized.contains("cookie")
}

fn is_prompt_content_key(name: &str) -> bool {
    let normalized: String = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        normalized.as_str(),
        "prompt" | "content" | "message" | "messages" | "input" | "output" | "completion"
    ) || normalized.contains("prompt")
}

fn collect_findings(value: &Value, path: &str, findings: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = format!("{path}.{key}");
                if is_secret_key(key) && child != REDACTED {
                    findings.push(child_path.clone());
                }
                collect_findings(child, &child_path, findings);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_findings(child, &format!("{path}[{index}]"), findings);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;
    use axum::http::header;
    use serde_json::json;

    use super::*;

    #[test]
    fn redacts_secret_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer sk-test"),
        );
        headers.insert("x-trace-id", HeaderValue::from_static("trace"));

        let redacted = redact_headers(&headers);

        assert_eq!(redacted.get("authorization"), Some(&REDACTED.to_owned()));
        assert_eq!(redacted.get("x-trace-id"), Some(&"trace".to_owned()));
    }

    #[test]
    fn recursively_redacts_json_credentials_in_objects_and_arrays() {
        let value = json!({
            "apiKey": "sk-test",
            "nested": {
                "access_token": "access",
                "items": [
                    { "refresh-token": "refresh", "prompt": "keep prompt" },
                    { "cookie": "session=abc" }
                ]
            },
            "messages": [{ "content": "body retention is explicit" }]
        });

        let redacted = redact_json_value(&value);

        assert_eq!(redacted["apiKey"], REDACTED);
        assert_eq!(redacted["nested"]["access_token"], REDACTED);
        assert_eq!(redacted["nested"]["items"][0]["refresh-token"], REDACTED);
        assert_eq!(redacted["nested"]["items"][0]["prompt"], REDACTED);
        assert_eq!(redacted["messages"][0]["content"], REDACTED);
        assert!(redaction_findings(&redacted).is_empty());
    }
}
