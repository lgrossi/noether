use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::fs;

use crate::error::NoetError;

pub const DEFAULT_UPDATE_MANIFEST_URL: &str = "github:lgrossi/noether:preview";

#[derive(Clone, Debug, Deserialize)]
pub struct ReleaseManifest {
    pub version: String,
    pub channel: String,
    pub release_type: String,
    pub auto_update_eligible: bool,
    #[serde(default)]
    pub changes_contract: bool,
    #[serde(default)]
    pub changes_defaults: bool,
    #[serde(default)]
    pub changes_policy_semantics: bool,
    #[serde(default)]
    pub changes_enforcement_semantics: bool,
    #[serde(default)]
    pub changes_audit_semantics: bool,
    #[serde(default)]
    pub artifacts: Vec<ReleaseArtifact>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReleaseArtifact {
    pub target: String,
    pub kind: String,
    pub file: String,
    pub sha256: String,
    pub url: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub fn parse(input: &str) -> Result<Self, NoetError> {
        let value = input.trim().strip_prefix('v').unwrap_or(input.trim());
        let mut parts = value.split('.');
        let major = parse_part(parts.next(), input)?;
        let minor = parse_part(parts.next(), input)?;
        let patch = parse_part(parts.next(), input)?;
        if parts.next().is_some() {
            return Err(invalid_version(input));
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    pub fn current() -> Result<Self, NoetError> {
        Self::parse(env!("CARGO_PKG_VERSION"))
    }

    pub fn is_auto_update_to(self, latest: Self, manifest: &ReleaseManifest) -> bool {
        if latest <= self || !manifest.auto_update_eligible {
            return false;
        }
        if manifest.changes_contract
            || manifest.changes_defaults
            || manifest.changes_policy_semantics
            || manifest.changes_enforcement_semantics
            || manifest.changes_audit_semantics
        {
            return false;
        }
        if self.major == 0 {
            return latest.major == 0 && latest.minor == self.minor && latest.patch > self.patch;
        }
        latest.major == self.major && latest.minor == self.minor && latest.patch > self.patch
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn parse_part(part: Option<&str>, original: &str) -> Result<u64, NoetError> {
    part.filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_version(original))?
        .parse::<u64>()
        .map_err(|_| invalid_version(original))
}

fn invalid_version(input: &str) -> NoetError {
    NoetError::InvalidConfig(format!(
        "invalid release version `{input}`; expected vX.Y.Z"
    ))
}

#[derive(Clone, Debug)]
pub struct UpdatePlan {
    pub current: Version,
    pub latest: Version,
    pub manifest: ReleaseManifest,
    pub target: String,
    pub artifact: Option<ReleaseArtifact>,
    pub auto_update_allowed: bool,
}

pub async fn fetch_update_plan(manifest_url: &str) -> Result<UpdatePlan, NoetError> {
    let current = Version::current()?;
    let manifest = fetch_manifest(manifest_url).await?;
    let latest = Version::parse(&manifest.version)?;
    let target = target_name()?;
    let artifact = manifest
        .artifacts
        .iter()
        .find(|artifact| artifact.target == target && artifact.kind == "binary")
        .cloned();
    let auto_update_allowed = current.is_auto_update_to(latest, &manifest);
    Ok(UpdatePlan {
        current,
        latest,
        manifest,
        target,
        artifact,
        auto_update_allowed,
    })
}

pub async fn fetch_manifest(manifest_url: &str) -> Result<ReleaseManifest, NoetError> {
    if let Some(path) = manifest_url.strip_prefix("file://") {
        let bytes = fs::read(path).await?;
        return Ok(serde_json::from_slice(&bytes)?);
    }
    if let Some(selector) = manifest_url.strip_prefix("github:") {
        return fetch_github_release_manifest(selector).await;
    }
    let response = http_client()
        .get(manifest_url)
        .send()
        .await?
        .error_for_status()?;
    Ok(response.json::<ReleaseManifest>().await?)
}

async fn fetch_github_release_manifest(selector: &str) -> Result<ReleaseManifest, NoetError> {
    let (repo, channel) = selector.rsplit_once(':').ok_or_else(|| {
        NoetError::InvalidConfig(
            "github update selector must look like github:owner/repo:preview".to_owned(),
        )
    })?;
    let releases_url = format!("https://api.github.com/repos/{repo}/releases?per_page=30");
    let releases = http_client()
        .get(releases_url)
        .send()
        .await?
        .error_for_status()?
        .json::<Vec<GithubRelease>>()
        .await?;
    let release = releases
        .into_iter()
        .find(|release| match channel {
            "preview" => release.prerelease,
            "stable" => !release.prerelease,
            _ => false,
        })
        .ok_or_else(|| {
            NoetError::NotFound(format!(
                "no GitHub {channel} release with noether-release.json found for {repo}"
            ))
        })?;
    let manifest_asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name == "noether-release.json")
        .ok_or_else(|| {
            NoetError::NotFound(format!(
                "release {} has no noether-release.json asset",
                release.tag_name
            ))
        })?;
    let response = http_client()
        .get(&manifest_asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?;
    Ok(response.json::<ReleaseManifest>().await?)
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("noet/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("valid update HTTP client")
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    prerelease: bool,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

pub async fn apply_update(plan: &UpdatePlan) -> Result<PathBuf, NoetError> {
    if plan.latest <= plan.current {
        return Err(NoetError::InvalidConfig(format!(
            "no update available; current version is {}",
            plan.current
        )));
    }
    if !plan.auto_update_allowed {
        return Err(NoetError::InvalidConfig(format!(
            "release {} is not auto-update eligible from {}",
            plan.latest, plan.current
        )));
    }
    let artifact = plan.artifact.as_ref().ok_or_else(|| {
        NoetError::InvalidConfig(format!(
            "release {} has no binary artifact for target {}",
            plan.latest, plan.target
        ))
    })?;

    let response = http_client()
        .get(&artifact.url)
        .send()
        .await?
        .error_for_status()?;
    let bytes = response.bytes().await?;
    let actual_sha = sha256_hex(&bytes);
    if !actual_sha.eq_ignore_ascii_case(artifact.sha256.trim()) {
        return Err(NoetError::InvalidConfig(format!(
            "checksum mismatch for {}; expected {}, got {}",
            artifact.file, artifact.sha256, actual_sha
        )));
    }

    let current_exe = std::env::current_exe()?;
    let temp_path = current_exe.with_file_name(format!(
        ".{}.update-{}",
        current_exe
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("noet"),
        std::process::id()
    ));
    fs::write(&temp_path, bytes).await?;
    make_executable(&temp_path).await?;
    install_replacement(&current_exe, &temp_path).await
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(unix)]
async fn make_executable(path: &Path) -> Result<(), NoetError> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path).await?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn make_executable(_path: &Path) -> Result<(), NoetError> {
    Ok(())
}

#[cfg(not(windows))]
async fn install_replacement(current_exe: &Path, temp_path: &Path) -> Result<PathBuf, NoetError> {
    let backup_path = current_exe.with_extension("old");
    if backup_path.exists() {
        fs::remove_file(&backup_path).await?;
    }
    fs::rename(current_exe, &backup_path).await?;
    match fs::rename(temp_path, current_exe).await {
        Ok(()) => {
            let _ = fs::remove_file(&backup_path).await;
            Ok(current_exe.to_path_buf())
        }
        Err(error) => {
            let _ = fs::rename(&backup_path, current_exe).await;
            Err(error.into())
        }
    }
}

#[cfg(windows)]
async fn install_replacement(current_exe: &Path, temp_path: &Path) -> Result<PathBuf, NoetError> {
    use std::process::Command;

    let script_path = temp_path.with_extension("cmd");
    let script = format!(
        "@echo off\r\n\
         ping 127.0.0.1 -n 2 > nul\r\n\
         move /Y \"{}\" \"{}\" > nul\r\n\
         del \"%~f0\" > nul\r\n",
        temp_path.display(),
        current_exe.display()
    );
    fs::write(&script_path, script).await?;
    Command::new("cmd")
        .args(["/C", "start", "", "/B"])
        .arg(&script_path)
        .spawn()?;
    Ok(current_exe.to_path_buf())
}

pub fn target_name() -> Result<String, NoetError> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    match (os, arch) {
        ("linux", "x86_64") => Ok("linux-x86_64".to_owned()),
        ("macos", "aarch64") => Ok("macos-aarch64".to_owned()),
        ("macos", "x86_64") => Ok("macos-x86_64".to_owned()),
        ("windows", "x86_64") => Ok("windows-x86_64".to_owned()),
        _ => Err(NoetError::InvalidConfig(format!(
            "unsupported update target {os}-{arch}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(version: &str, auto_update_eligible: bool) -> ReleaseManifest {
        ReleaseManifest {
            version: version.to_owned(),
            channel: "preview".to_owned(),
            release_type: "patch".to_owned(),
            auto_update_eligible,
            changes_contract: false,
            changes_defaults: false,
            changes_policy_semantics: false,
            changes_enforcement_semantics: false,
            changes_audit_semantics: false,
            artifacts: Vec::new(),
        }
    }

    #[test]
    fn preview_patch_train_is_auto_update_allowed() {
        let current = Version::parse("0.1.0").unwrap();
        let latest = Version::parse("0.1.1").unwrap();

        assert!(current.is_auto_update_to(latest, &manifest("0.1.1", true)));
    }

    #[test]
    fn preview_train_bump_is_not_auto_update_allowed() {
        let current = Version::parse("0.1.2").unwrap();
        let latest = Version::parse("0.2.0").unwrap();

        assert!(!current.is_auto_update_to(latest, &manifest("0.2.0", true)));
    }

    #[test]
    fn contract_changes_are_not_auto_update_allowed() {
        let current = Version::parse("0.1.0").unwrap();
        let latest = Version::parse("0.1.1").unwrap();
        let mut manifest = manifest("0.1.1", true);
        manifest.changes_policy_semantics = true;

        assert!(!current.is_auto_update_to(latest, &manifest));
    }
}
