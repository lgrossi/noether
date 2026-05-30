use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::error::NoetError;
use crate::policy::parse_limit_window;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NoetherConfig {
    #[serde(default, skip_serializing_if = "is_default_advisory_config")]
    pub advisory: AdvisoryConfig,
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
