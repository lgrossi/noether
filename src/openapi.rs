use axum::http::header::CONTENT_TYPE;
use axum::response::{Html, IntoResponse};

use crate::error::NoetError;

pub const OPENAPI_YAML: &str = include_str!("../openapi/noether.yaml");

pub fn openapi_json_value() -> Result<serde_json::Value, NoetError> {
    let yaml: serde_yaml::Value = serde_yaml::from_str(OPENAPI_YAML)?;
    serde_json::to_value(yaml).map_err(NoetError::from)
}

pub fn openapi_json_response() -> Result<impl IntoResponse, NoetError> {
    let json = serde_json::to_string_pretty(&openapi_json_value()?)?;
    Ok(([(CONTENT_TYPE, "application/json; charset=utf-8")], json))
}

pub fn api_docs_html() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Noether Sidecar API</title>
    <style>
      body { font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; margin: 0; background: #0d1117; color: #e6edf3; }
      main { max-width: 920px; margin: 0 auto; padding: 48px 24px; }
      a { color: #79c0ff; }
      code, pre { background: #161b22; border: 1px solid #30363d; border-radius: 8px; }
      code { padding: 2px 5px; }
      pre { padding: 16px; overflow: auto; }
      .boundary { border: 1px solid #3fb950; background: rgba(63,185,80,.08); padding: 16px; border-radius: 10px; }
      li { margin: 8px 0; }
    </style>
  </head>
  <body>
    <main>
      <h1>Noether Sidecar API</h1>
      <p class="boundary"><strong>Boundary:</strong> Noether is a decision sidecar. Integrations call Noether for authorization, finalization, and events. Integrations own provider transport; Noether does not call model providers as part of this API.</p>
      <p>Machine-readable spec: <a href="/openapi.json"><code>/openapi.json</code></a></p>
      <h2>Core lifecycle</h2>
      <pre>integration -> POST /v1/authorize
integration -> provider call
integration -> POST /v1/reservations/{id}/finalize
integration -> POST /v1/events</pre>
      <h2>Core endpoints</h2>
      <ul>
        <li><code>POST /v1/authorize</code> - authorize planned AI work.</li>
        <li><code>POST /v1/reservations/{id}/finalize</code> - report actual provider outcome.</li>
        <li><code>POST /v1/events</code> - record harness/tool/session events.</li>
        <li><code>GET /health</code> - read sidecar runtime posture.</li>
        <li><code>GET /metrics</code> - read pilot counters and gauges.</li>
      </ul>
    </main>
  </body>
</html>"#,
    )
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use crate::contract::{AuthorizeRequest, FinalizeReservation, TraceEvent};

    use super::*;

    #[test]
    fn openapi_spec_parses_and_declares_sidecar_boundary() {
        let spec = openapi_json_value().expect("openapi parses");

        assert_eq!(spec["openapi"], "3.1.0");
        let description = spec["info"]["description"].as_str().expect("description");
        assert!(description.contains("Noether is a decision sidecar"));
        assert!(description.contains("Noether does not call model providers"));
        assert!(spec["paths"]["/v1/authorize"]["post"].is_object());
        assert!(spec["paths"]["/v1/reservations/{id}/finalize"]["post"].is_object());
        assert!(spec["paths"]["/v1/events"]["post"].is_object());
        assert!(spec["paths"]["/health"]["get"].is_object());
        assert!(spec["paths"]["/metrics"]["get"].is_object());
    }

    #[test]
    fn openapi_request_examples_deserialize_into_contract_types() {
        let spec = openapi_json_value().expect("openapi parses");

        let authorize = example_value(
            &spec,
            &[
                "paths",
                "/v1/authorize",
                "post",
                "requestBody",
                "content",
                "application/json",
                "examples",
                "projectRun",
                "value",
            ],
        );
        serde_json::from_value::<AuthorizeRequest>(authorize).expect("authorize example");

        let finalize = example_value(
            &spec,
            &[
                "paths",
                "/v1/reservations/{id}/finalize",
                "post",
                "requestBody",
                "content",
                "application/json",
                "examples",
                "providerUsage",
                "value",
            ],
        );
        serde_json::from_value::<FinalizeReservation>(finalize).expect("finalize example");

        let event = example_value(
            &spec,
            &[
                "paths",
                "/v1/events",
                "post",
                "requestBody",
                "content",
                "application/json",
                "examples",
                "toolObserved",
                "value",
            ],
        );
        serde_json::from_value::<TraceEvent>(event).expect("event example");
    }

    fn example_value(spec: &Value, path: &[&str]) -> Value {
        let mut value = spec;
        for key in path {
            value = &value[*key];
        }
        assert!(!value.is_null(), "missing OpenAPI example at {path:?}");
        value.clone()
    }
}
