use std::collections::BTreeMap;

use axum::Json;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;

use crate::contract::{AuthorizeRequest, TraceEvent};

pub(crate) const NOETHER_API_KEY_HEADER: &str = "x-noet-api-key";

#[derive(Clone, Debug)]
pub struct RequestContext {
    pub request_id: String,
    pub actor: String,
    pub actor_source: &'static str,
}

pub(crate) fn request_has_noether_api_key(headers: &HeaderMap, api_key: &str) -> bool {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(bearer_token)
        .is_some_and(|token| token == api_key)
        || headers
            .get(NOETHER_API_KEY_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .is_some_and(|token| token == api_key)
}

pub(crate) fn request_context_from_headers(
    headers: &HeaderMap,
    request_id: &str,
    auth_configured: bool,
    actor_header: Option<&str>,
) -> Result<RequestContext, (StatusCode, Json<serde_json::Value>)> {
    let (actor, actor_source) = if let Some(actor_header) = actor_header {
        let Some(actor) = headers
            .get(actor_header)
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_owned())
        else {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "missing trusted actor header",
                    "message": format!("Noether requires trusted actor header `{actor_header}` because NOET_ACTOR_HEADER is configured. Configure the IAP/reverse proxy to strip client-supplied `{actor_header}` and inject the authenticated user value before forwarding to Noether."),
                    "actor_header": actor_header,
                })),
            ));
        };
        (actor, "trusted_header")
    } else if auth_configured {
        ("api_key".to_owned(), "bearer")
    } else {
        ("anonymous".to_owned(), "none")
    };
    Ok(RequestContext {
        request_id: request_id.to_owned(),
        actor,
        actor_source,
    })
}

pub(crate) fn request_id_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-noet-request-id")
        .or_else(|| headers.get("x-request-id"))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) fn insert_request_id_header(response: &mut Response, request_id: &str) {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert("x-noet-request-id", value);
    }
}

fn bearer_token(value: &str) -> Option<&str> {
    value
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

pub(crate) fn is_noether_bearer_authorization(value: &HeaderValue, api_key: &str) -> bool {
    value
        .to_str()
        .ok()
        .and_then(bearer_token)
        .is_some_and(|token| token == api_key)
}

pub(crate) fn normalize_api_key(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub(crate) fn normalize_actor_header(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
}

pub(crate) fn apply_request_context_to_authorize_request(
    request: &mut AuthorizeRequest,
    context: &RequestContext,
) {
    add_request_context_metadata(&mut request.metadata, context);
    if context.actor_source == "trusted_header" {
        let trusted_subject = trusted_actor_subject(&context.actor);
        if let Some(subject) = request.subject.replace(trusted_subject.clone()) {
            request
                .metadata
                .entry("client_claimed_subject".to_owned())
                .or_insert_with(|| serde_json::json!(subject));
        }
        let client_user_entities = request
            .entities
            .iter()
            .filter(|entity| entity.to_ascii_lowercase().starts_with("user:"))
            .cloned()
            .collect::<Vec<_>>();
        if !client_user_entities.is_empty() {
            request
                .metadata
                .entry("client_claimed_user_entities".to_owned())
                .or_insert_with(|| serde_json::json!(client_user_entities));
        }
        request
            .entities
            .retain(|entity| !entity.to_ascii_lowercase().starts_with("user:"));
        if !request
            .entities
            .iter()
            .any(|entity| entity == &trusted_subject)
        {
            request.entities.push(trusted_subject);
        }
    }
}

fn trusted_actor_subject(actor: &str) -> String {
    let actor = actor.trim();
    if actor.to_ascii_lowercase().starts_with("user:") {
        return actor.to_owned();
    }
    if let Some((_issuer, value)) = actor.split_once(':')
        && actor.contains('@')
        && !value.trim().is_empty()
    {
        return format!("user:{}", value.trim());
    }
    format!("user:{actor}")
}

pub(crate) fn add_request_context_metadata(
    metadata: &mut BTreeMap<String, serde_json::Value>,
    context: &RequestContext,
) {
    metadata
        .entry("request_id".to_owned())
        .or_insert_with(|| serde_json::json!(context.request_id));
    set_authoritative_actor_metadata(metadata, context);
}

fn set_authoritative_actor_metadata(
    metadata: &mut BTreeMap<String, serde_json::Value>,
    context: &RequestContext,
) {
    if let Some(client_actor) = metadata.remove("actor") {
        metadata
            .entry("client_claimed_actor".to_owned())
            .or_insert(client_actor);
    }
    metadata.insert(
        "actor".to_owned(),
        serde_json::json!({
            "id": context.actor,
            "source": context.actor_source,
        }),
    );
}

pub(crate) fn add_request_context_to_event(event: &mut TraceEvent, context: &RequestContext) {
    let payload = if let Some(payload) = event.payload.as_object_mut() {
        payload
    } else {
        let original_payload = std::mem::replace(&mut event.payload, serde_json::json!({}));
        event.payload = serde_json::json!({ "original_payload": original_payload });
        event.payload.as_object_mut().expect("object payload")
    };
    payload
        .entry("request_id".to_owned())
        .or_insert_with(|| serde_json::json!(context.request_id));
    if let Some(client_actor) = payload.remove("actor") {
        payload
            .entry("client_claimed_actor".to_owned())
            .or_insert(client_actor);
    }
    payload.insert(
        "actor".to_owned(),
        serde_json::json!({
            "id": context.actor,
            "source": context.actor_source,
        }),
    );
}
