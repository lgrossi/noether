use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    match (args.next().as_deref(), args.next().as_deref()) {
        (Some("release"), Some("check")) => {
            let version = args.next().ok_or("usage: xtask release check <version>")?;
            release_check(&version)
        }
        (Some("release"), Some("tag")) => {
            let version = args.next().ok_or("usage: xtask release tag <version>")?;
            release_check(&version)?;
            ensure_clean_worktree()?;
            ensure_tag_absent(&version)?;
            git([
                "tag",
                "-a",
                &normalize_tag(&version)?,
                "-m",
                &normalize_tag(&version)?,
            ])?;
            Ok(())
        }
        (Some("release"), Some("manifest")) => release_manifest(args.collect()),
        _ => Err("usage: xtask release <check|tag|manifest> ...\n\
             examples:\n\
             cargo run -p xtask -- release check v0.1.0\n\
             cargo run -p xtask -- release tag v0.1.0"
            .to_owned()),
    }
}

fn release_check(version: &str) -> Result<(), String> {
    let requested = Version::parse(version)?;
    let cargo_version = Version::parse(&root_package_version()?)?;
    if requested != cargo_version {
        return Err(format!(
            "root Cargo.toml version {cargo_version} does not match requested {requested}"
        ));
    }

    if let Some(previous) = previous_release_tag(&requested)? {
        validate_release_step(previous, requested)?;
    }

    println!("release check ok: v{requested}");
    Ok(())
}

fn release_manifest(args: Vec<String>) -> Result<(), String> {
    let mut version = None;
    let mut repo = "lgrossi/noether".to_owned();
    let mut out = None;
    let mut release_type = None;
    let mut assets = Vec::new();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--version" => version = iter.next(),
            "--repo" => repo = iter.next().ok_or("--repo requires a value")?,
            "--out" => out = iter.next().map(PathBuf::from),
            "--release-type" => release_type = iter.next(),
            "--asset" => assets.push(iter.next().ok_or("--asset requires a value")?),
            _ => return Err(format!("unknown manifest argument `{arg}`")),
        }
    }

    let version = Version::parse(&version.ok_or("--version is required")?)?;
    let out = out.ok_or("--out is required")?;
    let release_type = release_type.unwrap_or_else(|| {
        if version.major == 0 && version.patch == 0 {
            "manual_train".to_owned()
        } else {
            "patch".to_owned()
        }
    });
    let auto_update_eligible = release_type == "patch" && version.patch > 0;
    let tag = format!("v{version}");

    let artifact_json = assets
        .iter()
        .map(|asset| {
            let artifact = ManifestArtifact::parse(asset)?;
            let url = format!(
                "https://github.com/{repo}/releases/download/{tag}/{}",
                artifact.file
            );
            Ok(format!(
                "    {{\n\
                 \"target\": \"{}\",\n\
                 \"kind\": \"{}\",\n\
                 \"file\": \"{}\",\n\
                 \"sha256\": \"{}\",\n\
                 \"url\": \"{}\"\n\
                 }}",
                json_escape(&artifact.target),
                json_escape(&artifact.kind),
                json_escape(&artifact.file),
                json_escape(&artifact.sha256),
                json_escape(&url)
            ))
        })
        .collect::<Result<Vec<_>, String>>()?
        .join(",\n");

    let manifest = format!(
        "{{\n\
         \"version\": \"{version}\",\n\
         \"channel\": \"preview\",\n\
         \"release_type\": \"{}\",\n\
         \"auto_update_eligible\": {},\n\
         \"changes_contract\": false,\n\
         \"changes_defaults\": false,\n\
         \"changes_policy_semantics\": false,\n\
         \"changes_enforcement_semantics\": false,\n\
         \"changes_audit_semantics\": false,\n\
         \"artifacts\": [\n{}\n\
         ]\n\
         }}\n",
        json_escape(&release_type),
        auto_update_eligible,
        artifact_json
    );
    fs::write(out, manifest).map_err(|error| error.to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    fn parse(input: &str) -> Result<Self, String> {
        let value = input.trim().strip_prefix('v').unwrap_or(input.trim());
        let parts = value.split('.').collect::<Vec<_>>();
        if parts.len() != 3 {
            return Err(format!("invalid version `{input}`; expected vX.Y.Z"));
        }
        Ok(Self {
            major: parts[0]
                .parse()
                .map_err(|_| format!("invalid version `{input}`"))?,
            minor: parts[1]
                .parse()
                .map_err(|_| format!("invalid version `{input}`"))?,
            patch: parts[2]
                .parse()
                .map_err(|_| format!("invalid version `{input}`"))?,
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

struct ManifestArtifact {
    target: String,
    kind: String,
    file: String,
    sha256: String,
}

impl ManifestArtifact {
    fn parse(input: &str) -> Result<Self, String> {
        let mut target = None;
        let mut kind = None;
        let mut file = None;
        let mut sha256 = None;
        for part in input.split(',') {
            let (key, value) = part
                .split_once('=')
                .ok_or_else(|| format!("invalid asset `{input}`"))?;
            match key {
                "target" => target = Some(value.to_owned()),
                "kind" => kind = Some(value.to_owned()),
                "file" => file = Some(value.to_owned()),
                "sha256" => sha256 = Some(value.to_owned()),
                _ => return Err(format!("unknown asset key `{key}`")),
            }
        }
        Ok(Self {
            target: target.ok_or("asset target is required")?,
            kind: kind.ok_or("asset kind is required")?,
            file: file.ok_or("asset file is required")?,
            sha256: sha256.ok_or("asset sha256 is required")?,
        })
    }
}

fn root_package_version() -> Result<String, String> {
    let cargo = fs::read_to_string("Cargo.toml").map_err(|error| error.to_string())?;
    let mut in_package = false;
    for line in cargo.lines() {
        let trimmed = line.trim();
        if trimmed == "[package]" {
            in_package = true;
            continue;
        }
        if trimmed.starts_with('[') && trimmed != "[package]" {
            in_package = false;
        }
        if in_package && trimmed.starts_with("version") {
            return trimmed
                .split_once('=')
                .map(|(_, value)| value.trim().trim_matches('"').to_owned())
                .ok_or_else(|| "invalid package version line".to_owned());
        }
    }
    Err("root Cargo.toml package version not found".to_owned())
}

fn previous_release_tag(requested: &Version) -> Result<Option<Version>, String> {
    let output = git_output([
        "tag",
        "--list",
        "v[0-9]*.[0-9]*.[0-9]*",
        "--sort=-v:refname",
    ])?;
    for line in output.lines() {
        let version = Version::parse(line)?;
        if version < *requested {
            return Ok(Some(version));
        }
        if version == *requested {
            continue;
        }
        if version > *requested {
            return Err(format!(
                "requested v{requested} is older than existing tag v{version}"
            ));
        }
    }
    Ok(None)
}

fn validate_release_step(previous: Version, requested: Version) -> Result<(), String> {
    if requested.major == 0 {
        let patch = requested.major == 0
            && previous.major == 0
            && requested.minor == previous.minor
            && requested.patch > previous.patch;
        let train = requested.major == 0
            && previous.major == 0
            && requested.minor == previous.minor + 1
            && requested.patch == 0;
        if patch || train {
            return Ok(());
        }
        return Err(format!(
            "invalid preview release step v{previous} -> v{requested}; expected patch within 0.y.z or train bump to 0.(y+1).0"
        ));
    }
    if requested > previous {
        Ok(())
    } else {
        Err(format!(
            "release version v{requested} must be newer than v{previous}"
        ))
    }
}

fn ensure_clean_worktree() -> Result<(), String> {
    let status = git_output(["status", "--porcelain"])?;
    if status.trim().is_empty() {
        Ok(())
    } else {
        Err("worktree must be clean before tagging".to_owned())
    }
}

fn ensure_tag_absent(version: &str) -> Result<(), String> {
    let tag = normalize_tag(version)?;
    let local = git_output(["tag", "--list", &tag])?;
    if !local.trim().is_empty() {
        return Err(format!("local tag {tag} already exists"));
    }
    let remote = git_output(["ls-remote", "--tags", "origin", &format!("refs/tags/{tag}")])?;
    if !remote.trim().is_empty() {
        return Err(format!("remote tag {tag} already exists"));
    }
    Ok(())
}

fn normalize_tag(version: &str) -> Result<String, String> {
    Ok(format!("v{}", Version::parse(version)?))
}

fn git<const N: usize>(args: [&str; N]) -> Result<(), String> {
    let status = Command::new("git")
        .args(args)
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("git command failed".to_owned())
    }
}

fn git_output<const N: usize>(args: [&str; N]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err("git command failed".to_owned());
    }
    String::from_utf8(output.stdout).map_err(|error| error.to_string())
}

fn json_escape(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}
