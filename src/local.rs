use std::fs as std_fs;
use std::path::{Path, PathBuf};

use tokio::fs;

use crate::config::{NoetherConfig, validate_noether_config};
use crate::error::NoetError;

pub const DEFAULT_LOCAL_BIND: &str = "127.0.0.1:4051";
const LOCAL_RUNTIME_DIR: &str = ".noet";
const LEGACY_LOCAL_RUNTIME_DIR: &str = ".noether";
pub const DEFAULT_LOCAL_POLICY: &str = r#"version: 0
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
pub const DEFAULT_LOCAL_CONFIG: &str = r#"server:
  bind: 127.0.0.1:4051
policy:
  path: policy.yaml
  decision_mode: enforce
storage:
  sqlite_path: noet.sqlite
  fixture_dir: fixtures
  simulation_dir: simulations
updates:
  auto: patch
  check_on_start: false
advisory:
  warning_cadence: 4h
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalRuntimeLayout {
    pub root: PathBuf,
    pub config_path: PathBuf,
    pub policy_path: PathBuf,
    pub db_path: PathBuf,
    pub log_path: PathBuf,
    pub fixture_dir: PathBuf,
    pub simulation_dir: PathBuf,
    pub sidecar_dir: PathBuf,
    pub owner_path: PathBuf,
}

impl LocalRuntimeLayout {
    pub fn for_root(root: &Path) -> Self {
        Self::for_runtime_root(root.join(LOCAL_RUNTIME_DIR), "noet.sqlite")
    }

    pub fn legacy_for_root(root: &Path) -> Self {
        Self::for_runtime_root(root.join(LEGACY_LOCAL_RUNTIME_DIR), "noether.sqlite")
    }

    fn for_runtime_root(runtime_root: PathBuf, db_filename: &str) -> Self {
        Self {
            config_path: runtime_root.join("config.yaml"),
            policy_path: runtime_root.join("policy.yaml"),
            db_path: runtime_root.join(db_filename),
            log_path: runtime_root.join("noet.log"),
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
    migrate_legacy_runtime_if_needed(root, &layout).await?;
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

async fn migrate_legacy_runtime_if_needed(
    root: &Path,
    layout: &LocalRuntimeLayout,
) -> Result<(), NoetError> {
    let legacy = LocalRuntimeLayout::legacy_for_root(root);
    if !fs::try_exists(&legacy.root).await? {
        return Ok(());
    }

    fs::create_dir_all(&layout.root).await?;
    copy_if_present(&legacy.config_path, &layout.config_path).await?;
    copy_if_present(&legacy.policy_path, &layout.policy_path).await?;
    copy_if_present(&legacy.db_path, &layout.db_path).await?;
    copy_if_present(
        &path_with_suffix(&legacy.db_path, "-wal"),
        &path_with_suffix(&layout.db_path, "-wal"),
    )
    .await?;
    copy_if_present(
        &path_with_suffix(&legacy.db_path, "-shm"),
        &path_with_suffix(&layout.db_path, "-shm"),
    )
    .await?;
    copy_dir_if_present(&legacy.fixture_dir, &layout.fixture_dir)?;
    copy_dir_if_present(&legacy.simulation_dir, &layout.simulation_dir)?;
    Ok(())
}

async fn copy_if_present(source: &Path, destination: &Path) -> Result<(), NoetError> {
    if fs::try_exists(source).await? && !fs::try_exists(destination).await? {
        fs::copy(source, destination).await?;
    }
    Ok(())
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{}", path.display(), suffix))
}

fn copy_dir_if_present(source: &Path, destination: &Path) -> Result<(), NoetError> {
    if !source.exists() || destination.exists() {
        return Ok(());
    }
    std_fs::create_dir_all(destination)?;
    for entry in std_fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_if_present(&source_path, &destination_path)?;
        } else if !destination_path.exists() {
            std_fs::copy(&source_path, &destination_path)?;
        }
    }
    Ok(())
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

pub fn write_local_sidecar_owner_sync(
    layout: &LocalRuntimeLayout,
    bind: &str,
) -> Result<LocalSidecarOwner, NoetError> {
    std_fs::create_dir_all(&layout.sidecar_dir)?;
    let owner = LocalSidecarOwner {
        state: "running".to_owned(),
        pid: std::process::id(),
        cwd: std::env::current_dir()?,
        bind: bind.to_owned(),
        url: format!("http://{bind}"),
        started_at: chrono::Utc::now().to_rfc3339(),
    };
    std_fs::write(&layout.owner_path, serde_json::to_vec(&owner)?)?;
    Ok(owner)
}

pub async fn read_local_sidecar_owner(root: &Path) -> Result<Option<LocalSidecarOwner>, NoetError> {
    let layout = local_runtime_layout_for_read(root).await?;
    if let Some(owner) = read_local_sidecar_owner_at(&layout.owner_path).await? {
        return Ok(Some(owner));
    }
    let legacy = LocalRuntimeLayout::legacy_for_root(root);
    if legacy.owner_path != layout.owner_path {
        return read_local_sidecar_owner_at(&legacy.owner_path).await;
    }
    Ok(None)
}

pub async fn clear_local_sidecar_owner(root: &Path) -> Result<(), NoetError> {
    let layout = local_runtime_layout_for_read(root).await?;
    remove_owner_file(&layout.owner_path).await?;
    let legacy = LocalRuntimeLayout::legacy_for_root(root);
    if legacy.owner_path != layout.owner_path {
        remove_owner_file(&legacy.owner_path).await?;
    }
    Ok(())
}

async fn read_local_sidecar_owner_at(path: &Path) -> Result<Option<LocalSidecarOwner>, NoetError> {
    match fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(NoetError::from),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn remove_owner_file(path: &Path) -> Result<(), NoetError> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

async fn local_runtime_layout_for_read(root: &Path) -> Result<LocalRuntimeLayout, NoetError> {
    let layout = LocalRuntimeLayout::for_root(root);
    if fs::try_exists(&layout.root).await? {
        return Ok(layout);
    }
    let legacy = LocalRuntimeLayout::legacy_for_root(root);
    if fs::try_exists(&legacy.root).await? {
        return Ok(legacy);
    }
    Ok(layout)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn ensure_local_runtime_layout_creates_standard_noet_home() {
        let tempdir = tempdir().expect("tempdir");

        let layout = ensure_local_runtime_layout(tempdir.path())
            .await
            .expect("create local runtime layout");

        assert_eq!(layout.root, tempdir.path().join(".noet"));
        assert_eq!(layout.db_path, tempdir.path().join(".noet/noet.sqlite"));
        assert!(layout.fixture_dir.exists());
        assert!(layout.simulation_dir.exists());
        assert!(layout.sidecar_dir.exists());
        assert!(layout.config_path.exists());
        assert!(layout.policy_path.exists());
        let config = load_local_config(&layout.config_path)
            .await
            .expect("default local config parses");
        assert_eq!(config.server.bind.unwrap().to_string(), DEFAULT_LOCAL_BIND);
        assert_eq!(
            config.policy.path.as_deref(),
            Some(Path::new("policy.yaml"))
        );
        assert_eq!(
            config.storage.sqlite_path.as_deref(),
            Some(Path::new("noet.sqlite"))
        );
        assert_eq!(config.advisory.warning_cadence.as_deref(), Some("4h"));
        let policy = crate::policy::load_policy(&layout.policy_path)
            .await
            .expect("default local policy parses");
        assert_eq!(policy.budgets[0].id, "personal-local");
    }

    #[tokio::test]
    async fn read_local_sidecar_owner_accepts_legacy_noether_home() {
        let tempdir = tempdir().expect("tempdir");
        let legacy = LocalRuntimeLayout::legacy_for_root(tempdir.path());
        fs::create_dir_all(&legacy.sidecar_dir)
            .await
            .expect("create legacy sidecar dir");
        fs::write(
            &legacy.owner_path,
            br#"{"state":"running","pid":123,"cwd":"/tmp","bind":"127.0.0.1:4051","url":"http://127.0.0.1:4051","started_at":"2026-05-30T00:00:00Z"}"#,
        )
        .await
        .expect("write owner");

        let owner = read_local_sidecar_owner(tempdir.path())
            .await
            .expect("read owner")
            .expect("owner present");

        assert_eq!(owner.pid, 123);
    }

    #[tokio::test]
    async fn read_local_sidecar_owner_falls_back_to_legacy_owner_when_noet_exists() {
        let tempdir = tempdir().expect("tempdir");
        let layout = LocalRuntimeLayout::for_root(tempdir.path());
        fs::create_dir_all(&layout.sidecar_dir)
            .await
            .expect("create new sidecar dir");
        let legacy = LocalRuntimeLayout::legacy_for_root(tempdir.path());
        fs::create_dir_all(&legacy.sidecar_dir)
            .await
            .expect("create legacy sidecar dir");
        fs::write(
            &legacy.owner_path,
            br#"{"state":"running","pid":456,"cwd":"/tmp","bind":"127.0.0.1:4051","url":"http://127.0.0.1:4051","started_at":"2026-05-30T00:00:00Z"}"#,
        )
        .await
        .expect("write legacy owner");

        let owner = read_local_sidecar_owner(tempdir.path())
            .await
            .expect("read owner")
            .expect("owner present");

        assert_eq!(owner.pid, 456);
    }

    #[tokio::test]
    async fn ensure_local_runtime_layout_migrates_legacy_core_files() {
        let tempdir = tempdir().expect("tempdir");
        let legacy = LocalRuntimeLayout::legacy_for_root(tempdir.path());
        fs::create_dir_all(&legacy.root)
            .await
            .expect("create legacy root");
        fs::write(&legacy.config_path, b"advisory:\n  warning_cadence: 2h\n")
            .await
            .expect("write legacy config");
        fs::write(&legacy.policy_path, DEFAULT_LOCAL_POLICY)
            .await
            .expect("write legacy policy");
        fs::write(&legacy.db_path, b"legacy-db")
            .await
            .expect("write legacy db");
        fs::write(path_with_suffix(&legacy.db_path, "-wal"), b"legacy-wal")
            .await
            .expect("write legacy db wal");
        fs::write(path_with_suffix(&legacy.db_path, "-shm"), b"legacy-shm")
            .await
            .expect("write legacy db shm");
        fs::create_dir_all(&legacy.fixture_dir)
            .await
            .expect("create legacy fixture dir");
        fs::write(legacy.fixture_dir.join("fixture.json"), b"{}")
            .await
            .expect("write legacy fixture");
        fs::create_dir_all(&legacy.simulation_dir)
            .await
            .expect("create legacy simulation dir");
        fs::write(legacy.simulation_dir.join("simulation.json"), b"{}")
            .await
            .expect("write legacy simulation");
        let partial = LocalRuntimeLayout::for_root(tempdir.path());
        fs::create_dir_all(&partial.sidecar_dir)
            .await
            .expect("create partial noet sidecar dir");

        let layout = ensure_local_runtime_layout(tempdir.path())
            .await
            .expect("create local runtime layout");

        assert_eq!(
            fs::read_to_string(&layout.config_path)
                .await
                .expect("read migrated config"),
            "advisory:\n  warning_cadence: 2h\n"
        );
        assert_eq!(
            fs::read(&layout.db_path).await.expect("read migrated db"),
            b"legacy-db"
        );
        assert_eq!(
            fs::read(path_with_suffix(&layout.db_path, "-wal"))
                .await
                .expect("read migrated wal"),
            b"legacy-wal"
        );
        assert_eq!(
            fs::read(path_with_suffix(&layout.db_path, "-shm"))
                .await
                .expect("read migrated shm"),
            b"legacy-shm"
        );
        assert!(layout.fixture_dir.join("fixture.json").exists());
        assert!(layout.simulation_dir.join("simulation.json").exists());
    }
}
