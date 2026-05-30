use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use crate::contract::DecisionMode;
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::error::NoetError;
use crate::policy::parse_limit_window;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NoetherConfig {
    #[serde(default, skip_serializing_if = "is_default_server_config")]
    pub server: ServerConfig,
    #[serde(default, skip_serializing_if = "is_default_policy_config")]
    pub policy: PolicyConfig,
    #[serde(default, skip_serializing_if = "is_default_storage_config")]
    pub storage: StorageConfig,
    #[serde(default, skip_serializing_if = "is_default_updates_config")]
    pub updates: UpdatesConfig,
    #[serde(default, skip_serializing_if = "is_default_advisory_config")]
    pub advisory: AdvisoryConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind: Option<SocketAddr>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_mode: Option<DecisionMode>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sqlite_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_url: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatesConfig {
    #[serde(default, skip_serializing_if = "is_default_auto_update_mode")]
    pub auto: AutoUpdateMode,
    #[serde(default, skip_serializing_if = "is_false")]
    pub check_on_start: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoUpdateMode {
    #[default]
    Off,
    Patch,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdvisoryConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning_cadence: Option<String>,
    #[serde(default, skip_serializing_if = "is_default_enablement_tips_config")]
    pub enablement_tips: EnablementTipsConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnablementTipsConfig {
    #[serde(default, skip_serializing_if = "is_default_tip_catalog_mode")]
    pub mode: TipCatalogMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tips: Vec<EnablementTipConfig>,
}

impl Default for EnablementTipsConfig {
    fn default() -> Self {
        Self {
            mode: TipCatalogMode::Extend,
            tips: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TipCatalogMode {
    #[default]
    Extend,
    Replace,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnablementTipConfig {
    pub key: String,
    pub body: String,
}

pub async fn load_noether_config(path: &Path) -> Result<NoetherConfig, NoetError> {
    let bytes = fs::read(path).await?;
    parse_noether_config_bytes(&bytes)
}

pub fn parse_noether_config_bytes(bytes: &[u8]) -> Result<NoetherConfig, NoetError> {
    let config: NoetherConfig = serde_yaml::from_slice(bytes)?;
    validate_noether_config(&config)?;
    Ok(config)
}

pub fn validate_noether_config(config: &NoetherConfig) -> Result<(), NoetError> {
    let mut errors = Vec::new();
    if let Some(cadence) = config.advisory.warning_cadence.as_deref()
        && parse_limit_window(cadence).is_none()
    {
        errors.push(format!(
            "advisory.warning_cadence must use <number><s|m|h|d>, got {cadence}"
        ));
    }
    for tip in &config.advisory.enablement_tips.tips {
        if tip.key.trim().is_empty() {
            errors.push("advisory.enablement_tips.tips.key must not be empty".to_owned());
        }
        if tip.body.trim().is_empty() {
            errors.push("advisory.enablement_tips.tips.body must not be empty".to_owned());
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(NoetError::InvalidConfig(errors.join("; ")))
    }
}

fn is_default_advisory_config(config: &AdvisoryConfig) -> bool {
    config == &AdvisoryConfig::default()
}

fn is_default_server_config(config: &ServerConfig) -> bool {
    config == &ServerConfig::default()
}

fn is_default_policy_config(config: &PolicyConfig) -> bool {
    config == &PolicyConfig::default()
}

fn is_default_storage_config(config: &StorageConfig) -> bool {
    config == &StorageConfig::default()
}

fn is_default_updates_config(config: &UpdatesConfig) -> bool {
    config == &UpdatesConfig::default()
}

fn is_default_auto_update_mode(mode: &AutoUpdateMode) -> bool {
    *mode == AutoUpdateMode::Off
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_default_enablement_tips_config(config: &EnablementTipsConfig) -> bool {
    config == &EnablementTipsConfig::default()
}

fn is_default_tip_catalog_mode(mode: &TipCatalogMode) -> bool {
    *mode == TipCatalogMode::Extend
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_noether_advisory_config() {
        let config = parse_noether_config_bytes(
            br#"
server:
  bind: 127.0.0.1:4051
policy:
  path: policy.yaml
  decision_mode: enforce
storage:
  sqlite_path: noet.sqlite
updates:
  auto: patch
  check_on_start: true
advisory:
  warning_cadence: 30m
  enablement_tips:
    mode: replace
    tips:
      - key: custom.workflow
        body: Try this workflow-specific AI affordance.
"#,
        )
        .expect("config parses");

        assert_eq!(config.server.bind.unwrap().to_string(), "127.0.0.1:4051");
        assert_eq!(
            config.policy.path.as_deref(),
            Some(Path::new("policy.yaml"))
        );
        assert_eq!(config.policy.decision_mode, Some(DecisionMode::Enforce));
        assert_eq!(
            config.storage.sqlite_path.as_deref(),
            Some(Path::new("noet.sqlite"))
        );
        assert_eq!(config.updates.auto, AutoUpdateMode::Patch);
        assert!(config.updates.check_on_start);
        assert_eq!(config.advisory.warning_cadence.as_deref(), Some("30m"));
        assert_eq!(
            config.advisory.enablement_tips.mode,
            TipCatalogMode::Replace
        );
        assert_eq!(
            config.advisory.enablement_tips.tips[0].key,
            "custom.workflow"
        );
    }

    #[test]
    fn rejects_invalid_noether_advisory_config() {
        let error = parse_noether_config_bytes(
            br#"
advisory:
  warning_cadence: sometimes
  enablement_tips:
    tips:
      - key: ""
        body: ""
"#,
        )
        .expect_err("advisory config should be invalid");
        let message = error.to_string();
        assert!(message.contains("advisory.warning_cadence must use <number><s|m|h|d>"));
        assert!(message.contains("advisory.enablement_tips.tips.key must not be empty"));
        assert!(message.contains("advisory.enablement_tips.tips.body must not be empty"));
    }
}
