use std::path::{Path, PathBuf};

use tokio::fs;

use crate::config::{NoetherConfig, validate_noether_config};
use crate::error::NoetError;

pub const DEFAULT_LOCAL_BIND: &str = "127.0.0.1:4051";
const LOCAL_RUNTIME_DIR: &str = ".noether";
const DEFAULT_LOCAL_POLICY: &str = r#"version: 0
routing:
  mode: explicit_then_fallback
  fallback_order: [project, user, team, group, org, global]
budgets:
  - id: personal-local
    models:
      allow:
        - openai-codex:*
        - openai:*
        - anthropic:claude-sonnet-*
        - anthropic:claude-haiku-*
    limits:
      spend:
        - id: monthly-cap
          by: global
          window: 30d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 1000
          warn_at_fraction: 0.8
          action: block
        - id: daily-cap
          by: global
          window: 1d
          mode: tumbling
          anchor:
            kind: first_seen
          max_usd: 100
          action: block
"#;
const DEFAULT_LOCAL_CONFIG: &str = r#"advisory:
  warning_cadence: 4h
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalRuntimeLayout {
    pub root: PathBuf,
    pub config_path: PathBuf,
    pub policy_path: PathBuf,
    pub db_path: PathBuf,
    pub fixture_dir: PathBuf,
    pub simulation_dir: PathBuf,
    pub sidecar_dir: PathBuf,
    pub owner_path: PathBuf,
}

impl LocalRuntimeLayout {
    pub fn for_root(root: &Path) -> Self {
        let runtime_root = root.join(LOCAL_RUNTIME_DIR);
        Self {
            config_path: runtime_root.join("config.yaml"),
            policy_path: runtime_root.join("policy.yaml"),
            db_path: runtime_root.join("noether.sqlite"),
            fixture_dir: runtime_root.join("fixtures"),
            simulation_dir: runtime_root.join("simulations"),
            sidecar_dir: runtime_root.join("pi-sidecar"),
            owner_path: runtime_root.join("pi-sidecar").join("owner.json"),
            root: runtime_root,
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct LocalSidecarOwner {
    pub state: String,
    pub pid: u32,
    pub cwd: PathBuf,
    pub bind: String,
    pub url: String,
    pub started_at: String,
}

pub async fn ensure_local_runtime_layout(root: &Path) -> Result<LocalRuntimeLayout, NoetError> {
    let layout = LocalRuntimeLayout::for_root(root);
    fs::create_dir_all(&layout.root).await?;
    fs::create_dir_all(&layout.fixture_dir).await?;
    fs::create_dir_all(&layout.simulation_dir).await?;
    fs::create_dir_all(&layout.sidecar_dir).await?;
    if !fs::try_exists(&layout.policy_path).await? {
        fs::write(&layout.policy_path, DEFAULT_LOCAL_POLICY).await?;
    }
    if !fs::try_exists(&layout.config_path).await? {
        fs::write(&layout.config_path, DEFAULT_LOCAL_CONFIG).await?;
    }
    Ok(layout)
}

pub async fn load_local_config(path: &Path) -> Result<NoetherConfig, NoetError> {
    let bytes = fs::read(path).await?;
    let config: NoetherConfig = serde_yaml::from_slice(&bytes)?;
    validate_noether_config(&config)?;
    Ok(config)
}

pub async fn write_local_sidecar_owner(
    layout: &LocalRuntimeLayout,
    bind: &str,
) -> Result<LocalSidecarOwner, NoetError> {
    let owner = LocalSidecarOwner {
        state: "running".to_owned(),
        pid: std::process::id(),
        cwd: std::env::current_dir()?,
        bind: bind.to_owned(),
        url: format!("http://{bind}"),
        started_at: chrono::Utc::now().to_rfc3339(),
    };
    fs::write(&layout.owner_path, serde_json::to_vec(&owner)?).await?;
    Ok(owner)
}

pub async fn read_local_sidecar_owner(root: &Path) -> Result<Option<LocalSidecarOwner>, NoetError> {
    let layout = LocalRuntimeLayout::for_root(root);
    match fs::read(&layout.owner_path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(NoetError::from),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn ensure_local_runtime_layout_creates_standard_noether_home() {
        let tempdir = tempdir().expect("tempdir");

        let layout = ensure_local_runtime_layout(tempdir.path())
            .await
            .expect("create local runtime layout");

        assert_eq!(layout.root, tempdir.path().join(".noether"));
        assert!(layout.fixture_dir.exists());
        assert!(layout.simulation_dir.exists());
        assert!(layout.sidecar_dir.exists());
        assert!(layout.config_path.exists());
        assert!(layout.policy_path.exists());
        let config = load_local_config(&layout.config_path)
            .await
            .expect("default local config parses");
        assert_eq!(config.advisory.warning_cadence.as_deref(), Some("4h"));
        let policy = crate::policy::load_policy(&layout.policy_path)
            .await
            .expect("default local policy parses");
        assert_eq!(policy.budgets[0].id, "personal-local");
    }
}
