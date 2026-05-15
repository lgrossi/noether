use std::path::Path;

use axum::http::HeaderMap;
use serde::Deserialize;
use tokio::fs;

use crate::error::NoetError;

#[derive(Clone, Debug, Deserialize)]
pub struct ProxyRoutes {
    #[serde(default)]
    pub routes: Vec<ProxyRoute>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProxyRoute {
    pub id: String,
    #[serde(default)]
    pub path_prefix: Option<String>,
    #[serde(default)]
    pub header_name: Option<String>,
    #[serde(default)]
    pub header_value: Option<String>,
    #[serde(deserialize_with = "deserialize_url")]
    pub upstream_base_url: url::Url,
}

#[derive(Clone, Debug)]
pub struct MatchedProxyRoute {
    pub id: String,
    pub upstream_base_url: url::Url,
    pub upstream_path: String,
}

impl ProxyRoutes {
    pub fn match_request(
        &self,
        path_and_query: &str,
        headers: &HeaderMap,
    ) -> Option<MatchedProxyRoute> {
        self.routes
            .iter()
            .find(|route| route.matches(path_and_query, headers))
            .map(|route| MatchedProxyRoute {
                id: route.id.clone(),
                upstream_base_url: route.upstream_base_url.clone(),
                upstream_path: route.upstream_path(path_and_query),
            })
    }
}

impl ProxyRoute {
    fn matches(&self, path_and_query: &str, headers: &HeaderMap) -> bool {
        let path_matches = self
            .path_prefix
            .as_deref()
            .is_none_or(|prefix| path_prefix_matches(prefix, path_part(path_and_query)));
        let header_matches = self.header_name.as_deref().is_none_or(|name| {
            headers.get(name).is_some_and(|actual| {
                self.header_value
                    .as_deref()
                    .is_none_or(|expected| actual.to_str().is_ok_and(|actual| actual == expected))
            })
        });

        path_matches && header_matches
    }

    fn upstream_path(&self, path_and_query: &str) -> String {
        self.path_prefix.as_deref().map_or_else(
            || path_and_query.to_owned(),
            |prefix| strip_path_prefix(prefix, path_and_query),
        )
    }
}

pub async fn load_proxy_routes(path: &Path) -> Result<ProxyRoutes, NoetError> {
    let bytes = fs::read(path).await?;
    let routes: ProxyRoutes = serde_yaml::from_slice(&bytes)?;
    validate_proxy_routes(&routes)?;
    Ok(routes)
}

pub fn validate_proxy_routes(routes: &ProxyRoutes) -> Result<(), NoetError> {
    let mut errors = Vec::new();

    for route in &routes.routes {
        if route.id.trim().is_empty() {
            errors.push("route id must not be empty".to_owned());
        }
        if route.path_prefix.is_none() && route.header_name.is_none() {
            errors.push(format!(
                "route {} must define path_prefix or header_name",
                route.id
            ));
        }
        if let Some(prefix) = &route.path_prefix {
            if !prefix.starts_with('/') {
                errors.push(format!("route {} path_prefix must start with /", route.id));
            }
            if prefix.contains('?') {
                errors.push(format!(
                    "route {} path_prefix must not include a query",
                    route.id
                ));
            }
        }
        if let Some(name) = &route.header_name {
            if name.trim().is_empty() {
                errors.push(format!("route {} header_name must not be empty", route.id));
            } else if axum::http::HeaderName::from_bytes(name.as_bytes()).is_err() {
                errors.push(format!("route {} header_name is invalid", route.id));
            }
        }
        if !matches!(route.upstream_base_url.scheme(), "http" | "https") {
            errors.push(format!(
                "route {} upstream_base_url must use http or https",
                route.id
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(NoetError::InvalidConfig(errors.join("; ")))
    }
}

fn deserialize_url<'de, D>(deserializer: D) -> Result<url::Url, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    url::Url::parse(&value).map_err(serde::de::Error::custom)
}

fn path_part(path_and_query: &str) -> &str {
    path_and_query
        .split_once('?')
        .map_or(path_and_query, |(path, _)| path)
}

fn query_part(path_and_query: &str) -> Option<&str> {
    path_and_query.split_once('?').map(|(_, query)| query)
}

fn path_prefix_matches(prefix: &str, path: &str) -> bool {
    let prefix = normalized_prefix(prefix);
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn strip_path_prefix(prefix: &str, path_and_query: &str) -> String {
    let prefix = normalized_prefix(prefix);
    let path = path_part(path_and_query);
    let stripped = path.strip_prefix(prefix).unwrap_or(path);
    let stripped = if stripped.is_empty() { "/" } else { stripped };

    match query_part(path_and_query) {
        Some(query) => format!("{stripped}?{query}"),
        None => stripped.to_owned(),
    }
}

fn normalized_prefix(prefix: &str) -> &str {
    if prefix == "/" {
        prefix
    } else {
        prefix.trim_end_matches('/')
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn route() -> ProxyRoute {
        ProxyRoute {
            id: "openai".to_owned(),
            path_prefix: Some("/providers/openai/".to_owned()),
            header_name: Some("x-noet-provider".to_owned()),
            header_value: Some("openai".to_owned()),
            upstream_base_url: url::Url::parse("https://api.openai.com/").expect("url"),
        }
    }

    #[test]
    fn path_prefix_match_strips_wrapper_prefix_and_keeps_query() {
        let routes = ProxyRoutes {
            routes: vec![route()],
        };
        let mut headers = HeaderMap::new();
        headers.insert("x-noet-provider", HeaderValue::from_static("openai"));

        let matched = routes
            .match_request("/providers/openai/v1/responses?stream=false", &headers)
            .expect("route match");

        assert_eq!(matched.id, "openai");
        assert_eq!(matched.upstream_path, "/v1/responses?stream=false");
    }

    #[test]
    fn route_config_parses_local_yaml_contract() {
        let routes: ProxyRoutes = serde_yaml::from_str(
            r#"
routes:
  - id: openai
    path_prefix: /providers/openai
    header_name: x-noet-provider
    header_value: openai
    upstream_base_url: https://api.openai.com/
"#,
        )
        .expect("routes parse");

        validate_proxy_routes(&routes).expect("routes valid");
        assert_eq!(routes.routes[0].id, "openai");
    }
}
