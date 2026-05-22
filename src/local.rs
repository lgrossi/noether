use std::path::{Path, PathBuf};

use tokio::fs;

use crate::error::NoetError;

pub const DEFAULT_LOCAL_BIND: &str = "127.0.0.1:4051";
const LOCAL_RUNTIME_DIR: &str = ".noether";
const DEFAULT_LOCAL_POLICY: &str = r#"version: 0
routing:
  mode: explicit_then_fallback
  specificity: [project, user, team, group, org, global]
budgets:
  - id: personal-local
    match: {}
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalRuntimeLayout {
    pub root: PathBuf,
    pub policy_path: PathBuf,
    pub db_path: PathBuf,
    pub fixture_dir: PathBuf,
    pub simulation_dir: PathBuf,
}

impl LocalRuntimeLayout {
    pub fn for_root(root: &Path) -> Self {
        let runtime_root = root.join(LOCAL_RUNTIME_DIR);
        Self {
            policy_path: runtime_root.join("policy.yaml"),
            db_path: runtime_root.join("noether.sqlite"),
            fixture_dir: runtime_root.join("fixtures"),
            simulation_dir: runtime_root.join("simulations"),
            root: runtime_root,
        }
    }
}

pub async fn ensure_local_runtime_layout(root: &Path) -> Result<LocalRuntimeLayout, NoetError> {
    let layout = LocalRuntimeLayout::for_root(root);
    fs::create_dir_all(&layout.root).await?;
    fs::create_dir_all(&layout.fixture_dir).await?;
    fs::create_dir_all(&layout.simulation_dir).await?;
    if !fs::try_exists(&layout.policy_path).await? {
        fs::write(&layout.policy_path, DEFAULT_LOCAL_POLICY).await?;
    }
    Ok(layout)
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
        assert!(layout.policy_path.exists());
        let policy = crate::policy::load_policy(&layout.policy_path)
            .await
            .expect("default local policy parses");
        assert_eq!(policy.budgets[0].id, "personal-local");
    }
}
