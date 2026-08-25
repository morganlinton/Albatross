//! Installable resource packages sourced from npm or Git.
//!
//! Packages live under Albatross's config directory and expose resources via
//! an `albatross` object in package.json or conventional resource folders.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

const REGISTRY_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PackageKind {
    Npm,
    Git,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPackage {
    pub id: String,
    pub source: String,
    pub kind: PackageKind,
    pub package_root: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageRegistryFile {
    #[serde(default = "registry_version")]
    version: u32,
    #[serde(default)]
    packages: BTreeMap<String, InstalledPackage>,
}

fn registry_version() -> u32 {
    REGISTRY_VERSION
}

impl Default for PackageRegistryFile {
    fn default() -> Self {
        Self {
            version: REGISTRY_VERSION,
            packages: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PackageResources {
    pub extensions: BTreeMap<String, crate::extensions::ExtensionConfig>,
    pub skills: BTreeMap<String, TextResource>,
    pub prompts: BTreeMap<String, TextResource>,
    pub themes: BTreeMap<String, ThemeResource>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TextResource {
    pub name: String,
    pub package: String,
    pub description: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemePalette {
    pub name: String,
    #[serde(default = "default_accent")]
    pub accent: u8,
    #[serde(default = "default_accent_deep")]
    pub accent_deep: u8,
    #[serde(default = "default_muted")]
    pub muted: u8,
    #[serde(default = "default_success")]
    pub success: u8,
    #[serde(default = "default_warn")]
    pub warn: u8,
    #[serde(default = "default_error")]
    pub error: u8,
    #[serde(default = "default_magenta")]
    pub magenta: u8,
    #[serde(default = "default_fade")]
    pub fade: [u8; 12],
}

fn default_accent() -> u8 {
    51
}
fn default_accent_deep() -> u8 {
    37
}
fn default_muted() -> u8 {
    244
}
fn default_success() -> u8 {
    84
}
fn default_warn() -> u8 {
    220
}
fn default_error() -> u8 {
    203
}
fn default_magenta() -> u8 {
    213
}
fn default_fade() -> [u8; 12] {
    [51, 45, 39, 38, 37, 31, 30, 24, 23, 237, 235, 234]
}

#[derive(Debug, Clone)]
pub struct ThemeResource {
    pub package: String,
    pub path: PathBuf,
    pub palette: ThemePalette,
}

#[derive(Debug, Default, Deserialize)]
struct PackageJson {
    name: Option<String>,
    version: Option<String>,
    #[serde(default)]
    albatross: Option<PackageManifest>,
    /// Portable, non-executable resources from a Pi package can be reused.
    /// Pi's in-process TypeScript extensions are intentionally incompatible
    /// with Albatross's JSON-RPC subprocess protocol and are ignored.
    #[serde(default)]
    pi: Option<PortableManifest>,
}

#[derive(Debug, Default, Deserialize)]
struct PackageManifest {
    #[serde(default)]
    extensions: Vec<PackagedExtension>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    prompts: Vec<String>,
    #[serde(default)]
    themes: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct PortableManifest {
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    prompts: Vec<String>,
    #[serde(default)]
    themes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PackagedExtension {
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone)]
enum ParsedSource {
    Npm {
        spec: String,
        name: String,
        pinned: bool,
    },
    Git {
        url: String,
        reference: Option<String>,
    },
}

pub fn registry_path() -> Option<PathBuf> {
    crate::auth::config_dir().map(|dir| dir.join("packages.json"))
}

fn packages_dir() -> Result<PathBuf> {
    crate::auth::config_dir()
        .map(|dir| dir.join("packages"))
        .ok_or_else(|| anyhow!("HOME or XDG_CONFIG_HOME is required to manage packages"))
}

fn load_registry() -> Result<PackageRegistryFile> {
    let Some(path) = registry_path() else {
        return Ok(PackageRegistryFile::default());
    };
    if !path.exists() {
        return Ok(PackageRegistryFile::default());
    }
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let registry: PackageRegistryFile =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if registry.version != REGISTRY_VERSION {
        bail!(
            "unsupported package registry version {} in {}",
            registry.version,
            path.display()
        );
    }
    Ok(registry)
}

fn save_registry(registry: &PackageRegistryFile) -> Result<()> {
    let path = registry_path().ok_or_else(|| anyhow!("no package registry path"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(registry)? + "\n";
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, body).with_context(|| format!("writing {}", temp.display()))?;
    fs::rename(&temp, &path).with_context(|| format!("replacing {}", path.display()))
}

pub fn list_installed() -> Result<Vec<InstalledPackage>> {
    Ok(load_registry()?.packages.into_values().collect())
}

pub fn install(source: &str) -> Result<InstalledPackage> {
    let parsed = parse_source(source)?;
    // Refuse to mutate installations when the existing registry cannot be
    // parsed or belongs to a newer format.
    let mut registry = load_registry()?;
    let base = packages_dir()?;
    fs::create_dir_all(&base).with_context(|| format!("creating {}", base.display()))?;
    let staging = tempfile::Builder::new()
        .prefix(".install-")
        .tempdir_in(&base)?;

    let (id, kind, package_root, version, revision, pinned) = match &parsed {
        ParsedSource::Npm { spec, name, pinned } => {
            install_npm(spec, name, staging.path())?;
            let root = staging.path().join("node_modules").join(name);
            let package = read_package_json(&root)?;
            let id = package.name.clone().unwrap_or_else(|| name.clone());
            (id, PackageKind::Npm, root, package.version, None, *pinned)
        }
        ParsedSource::Git { url, reference } => {
            let root = staging.path().join("repo");
            install_git(url, reference.as_deref(), &root)?;
            install_git_dependencies(&root)?;
            let package = read_package_json(&root)?;
            let id = package.name.clone().unwrap_or_else(|| git_name(url));
            let revision = git_revision(&root).ok();
            (
                id,
                PackageKind::Git,
                root,
                package.version,
                revision,
                reference.is_some(),
            )
        }
    };

    validate_package_root(&package_root)?;
    let slug = package_storage_slug(&id);
    let destination = base
        .join(match kind {
            PackageKind::Npm => "npm",
            PackageKind::Git => "git",
        })
        .join(&slug);
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }

    let staged_root = staging.keep();
    let backup = replace_directory(&staged_root, &destination)?;
    let relative_root = package_root
        .strip_prefix(&staged_root)
        .expect("package root is staged");
    let installed_root = destination.join(relative_root);
    let record = InstalledPackage {
        id: id.clone(),
        source: source.to_string(),
        kind,
        package_root: installed_root.display().to_string(),
        version,
        revision,
        pinned,
    };
    registry.packages.insert(id, record.clone());
    if let Err(error) = save_registry(&registry) {
        let _ = fs::remove_dir_all(&destination);
        if let Some(backup) = backup.as_ref() {
            let _ = fs::rename(backup, &destination);
        }
        return Err(error).context("saving package registry; installation rolled back");
    }
    if let Some(backup) = backup {
        fs::remove_dir_all(backup)?;
    }
    Ok(record)
}

pub fn remove(id_or_source: &str) -> Result<InstalledPackage> {
    let mut registry = load_registry()?;
    let id = registry
        .packages
        .iter()
        .find(|(id, package)| id.as_str() == id_or_source || package.source == id_or_source)
        .map(|(id, _)| id.clone())
        .ok_or_else(|| anyhow!("package not installed: {id_or_source}"))?;
    let package = registry.packages.remove(&id).expect("located package");
    let root = install_container(&package)?;
    let removed = root.with_extension("remove");
    if removed.exists() {
        fs::remove_dir_all(&removed)?;
    }
    if root.exists() {
        fs::rename(&root, &removed)
            .with_context(|| format!("staging removal of {}", root.display()))?;
    }
    if let Err(error) = save_registry(&registry) {
        if removed.exists() {
            let _ = fs::rename(&removed, &root);
        }
        return Err(error).context("saving package registry; removal rolled back");
    }
    if removed.exists() {
        fs::remove_dir_all(&removed).with_context(|| format!("removing {}", removed.display()))?;
    }
    Ok(package)
}

pub fn update(id: Option<&str>) -> Result<Vec<InstalledPackage>> {
    let current = list_installed()?;
    let selected = current
        .into_iter()
        .filter(|package| {
            id.map(|id| id == package.id || id == package.source)
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    if selected.is_empty() {
        bail!("no matching installed packages");
    }
    let mut updated = Vec::new();
    for package in selected {
        if package.pinned {
            continue;
        }
        updated.push(install(&package.source)?);
    }
    Ok(updated)
}

pub fn discover_installed() -> PackageResources {
    let mut resources = PackageResources::default();
    let packages = match list_installed() {
        Ok(packages) => packages,
        Err(error) => {
            resources.diagnostics.push(error.to_string());
            return resources;
        }
    };
    for package in packages {
        let root = PathBuf::from(&package.package_root);
        if let Err(error) = discover_package(&package, &root, &mut resources) {
            resources
                .diagnostics
                .push(format!("{}: {error}", package.id));
        }
    }
    resources
}

pub fn read_text_resource(resource: &TextResource) -> Result<String> {
    fs::read_to_string(&resource.path)
        .with_context(|| format!("reading {}", resource.path.display()))
}

fn discover_package(
    package: &InstalledPackage,
    root: &Path,
    out: &mut PackageResources,
) -> Result<()> {
    validate_package_root(root)?;
    let package_json = read_package_json(root)?;
    let mut manifest = package_json.albatross.unwrap_or_default();
    if let Some(portable) = package_json.pi {
        if manifest.skills.is_empty() {
            manifest.skills = portable.skills;
        }
        if manifest.prompts.is_empty() {
            manifest.prompts = portable.prompts;
        }
        if manifest.themes.is_empty() {
            manifest.themes = portable.themes;
        }
    }
    let mut extensions = manifest.extensions;
    if extensions.is_empty() {
        let directory = root.join("extensions");
        if directory.is_dir() {
            for entry in fs::read_dir(directory)? {
                let path = entry?.path();
                if path.extension().and_then(|value| value.to_str()) == Some("json") {
                    let text = fs::read_to_string(&path)?;
                    extensions.push(
                        serde_json::from_str(&text)
                            .with_context(|| format!("parsing {}", path.display()))?,
                    );
                }
            }
        }
    }

    for extension in extensions {
        let key = format!("{}--{}", safe_slug(&package.id), safe_slug(&extension.name));
        let command = resolve_command(root, &extension.command)?;
        out.extensions.insert(
            key,
            crate::extensions::ExtensionConfig {
                command,
                args: extension.args,
                env: extension.env,
                enabled: extension.enabled,
                working_directory: Some(root.display().to_string()),
            },
        );
    }

    let skill_paths = resource_files(root, &manifest.skills, "skills", ResourceKind::Skill)?;
    for path in skill_paths {
        insert_text_resource(&mut out.skills, package, path, ResourceKind::Skill);
    }
    let prompt_paths = resource_files(root, &manifest.prompts, "prompts", ResourceKind::Prompt)?;
    for path in prompt_paths {
        insert_text_resource(&mut out.prompts, package, path, ResourceKind::Prompt);
    }
    let theme_paths = resource_files(root, &manifest.themes, "themes", ResourceKind::Theme)?;
    for path in theme_paths {
        match fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))
            .and_then(|text| {
                serde_json::from_str::<ThemePalette>(&text)
                    .with_context(|| format!("parsing {}", path.display()))
            }) {
            Ok(palette) => {
                let name = palette.name.clone();
                if out
                    .themes
                    .insert(
                        name.clone(),
                        ThemeResource {
                            package: package.id.clone(),
                            path,
                            palette,
                        },
                    )
                    .is_some()
                {
                    out.diagnostics
                        .push(format!("duplicate theme `{name}`; later package won"));
                }
            }
            Err(error) => out.diagnostics.push(format!("{}: {error}", package.id)),
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ResourceKind {
    Skill,
    Prompt,
    Theme,
}

fn resource_files(
    root: &Path,
    declared: &[String],
    conventional: &str,
    kind: ResourceKind,
) -> Result<Vec<PathBuf>> {
    let entries = if declared.is_empty() {
        vec![conventional.to_string()]
    } else {
        declared.to_vec()
    };
    let mut files = BTreeSet::new();
    for entry in entries {
        let path = safe_join(root, &entry)?;
        if !path.exists() {
            continue;
        }
        if path.is_file() {
            if resource_matches(&path, kind) {
                files.insert(path);
            }
            continue;
        }
        for item in WalkDir::new(&path).follow_links(false) {
            let item = item?;
            if item.file_type().is_symlink() {
                continue;
            }
            if item.file_type().is_file() && resource_matches(item.path(), kind) {
                files.insert(item.path().to_path_buf());
            }
        }
    }
    Ok(files.into_iter().collect())
}

fn resource_matches(path: &Path, kind: ResourceKind) -> bool {
    match kind {
        ResourceKind::Skill => path.file_name().and_then(|s| s.to_str()) == Some("SKILL.md"),
        ResourceKind::Prompt => path.extension().and_then(|s| s.to_str()) == Some("md"),
        ResourceKind::Theme => path.extension().and_then(|s| s.to_str()) == Some("json"),
    }
}

fn insert_text_resource(
    target: &mut BTreeMap<String, TextResource>,
    package: &InstalledPackage,
    path: PathBuf,
    kind: ResourceKind,
) {
    let base = if matches!(kind, ResourceKind::Skill)
        && path.file_name().and_then(|s| s.to_str()) == Some("SKILL.md")
    {
        path.parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            .unwrap_or("skill")
    } else {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("resource")
    };
    let name = format!("{}:{}", safe_slug(&package.id), safe_slug(base));
    let description = fs::read_to_string(&path)
        .ok()
        .and_then(|text| markdown_description(&text))
        .unwrap_or_default();
    target.insert(
        name.clone(),
        TextResource {
            name,
            package: package.id.clone(),
            description,
            path,
        },
    );
}

fn markdown_description(text: &str) -> Option<String> {
    if let Some(description) = text
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("description:").map(str::trim))
        .filter(|line| !line.is_empty())
    {
        return Some(description.trim_matches(['\'', '"']).to_string());
    }
    text.lines()
        .map(str::trim)
        .find(|line| {
            !line.is_empty()
                && *line != "---"
                && !line.starts_with("name:")
                && !line.starts_with("description:")
        })
        .map(|line| line.trim_start_matches('#').trim().to_string())
}

fn read_package_json(root: &Path) -> Result<PackageJson> {
    let path = root.join("package.json");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("package must contain {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn validate_package_root(root: &Path) -> Result<()> {
    if !root.is_dir() {
        bail!("package root does not exist: {}", root.display());
    }
    let package = read_package_json(root)?;
    let conventional = ["extensions", "skills", "prompts", "themes"]
        .iter()
        .any(|name| root.join(name).is_dir());
    if package.albatross.is_none() && package.pi.is_none() && !conventional {
        bail!("package has neither an `albatross` manifest nor conventional resource directories");
    }
    Ok(())
}

fn resolve_command(root: &Path, command: &str) -> Result<String> {
    if command.starts_with("./") || command.starts_with("../") {
        Ok(safe_join(root, command)?.display().to_string())
    } else {
        Ok(command.to_string())
    }
}

fn safe_join(root: &Path, value: &str) -> Result<PathBuf> {
    let relative = Path::new(value);
    if relative.is_absolute()
        || relative.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("package resource path must stay inside the package: {value}");
    }
    let joined = root.join(relative);
    if joined.exists() {
        if fs::symlink_metadata(&joined)?.file_type().is_symlink() {
            bail!("package resource path cannot be a symbolic link: {value}");
        }
        let canonical_root = root.canonicalize()?;
        let canonical_joined = joined.canonicalize()?;
        if !canonical_joined.starts_with(&canonical_root) {
            bail!("package resource resolves outside the package: {value}");
        }
    }
    Ok(joined)
}

fn parse_source(source: &str) -> Result<ParsedSource> {
    if let Some(spec) = source.strip_prefix("npm:") {
        let name = npm_name(spec)?;
        let suffix = spec.strip_prefix(&name).unwrap_or("");
        return Ok(ParsedSource::Npm {
            spec: spec.to_string(),
            name,
            pinned: suffix.starts_with('@'),
        });
    }
    let raw = source.strip_prefix("git:").unwrap_or(source);
    if raw.starts_with("https://")
        || raw.starts_with("http://")
        || raw.starts_with("ssh://")
        || raw.starts_with("file://")
        || raw.starts_with("git@")
        || raw.starts_with("github.com/")
    {
        let (url, reference) = split_git_ref(raw);
        let url = if url.starts_with("github.com/") {
            format!("https://{url}")
        } else {
            url
        };
        return Ok(ParsedSource::Git { url, reference });
    }
    bail!("package source must be npm:<package> or git:<repository URL>")
}

fn npm_name(spec: &str) -> Result<String> {
    let name = if spec.starts_with('@') {
        let slash = spec
            .find('/')
            .ok_or_else(|| anyhow!("invalid scoped npm package: {spec}"))?;
        let rest = &spec[slash + 1..];
        let end = rest
            .find('@')
            .map(|pos| slash + 1 + pos)
            .unwrap_or(spec.len());
        &spec[..end]
    } else {
        spec.split('@').next().unwrap_or("")
    };
    if name.is_empty() || name.contains(char::is_whitespace) || name.contains("..") {
        bail!("invalid npm package name: {spec}");
    }
    Ok(name.to_string())
}

fn split_git_ref(raw: &str) -> (String, Option<String>) {
    if let Some((url, reference)) = raw.rsplit_once('#') {
        return (url.to_string(), Some(reference.to_string()));
    }
    if let Some(pos) = raw.rfind('@') {
        if pos > raw.rfind('/').unwrap_or(0) {
            return (raw[..pos].to_string(), Some(raw[pos + 1..].to_string()));
        }
    }
    (raw.to_string(), None)
}

fn install_npm(spec: &str, _name: &str, staging: &Path) -> Result<()> {
    let status = Command::new("npm")
        .arg("install")
        .arg("--ignore-scripts")
        .arg("--no-audit")
        .arg("--no-fund")
        .arg("--prefix")
        .arg(staging)
        .arg(spec)
        .status()
        .context("running npm install (is npm installed?)")?;
    if !status.success() {
        bail!("npm install failed with {status}");
    }
    Ok(())
}

fn install_git(url: &str, reference: Option<&str>, root: &Path) -> Result<()> {
    let status = Command::new("git")
        .args(["clone", "--depth", "1", url])
        .arg(root)
        .status()
        .context("running git clone")?;
    if !status.success() {
        bail!("git clone failed with {status}");
    }
    if let Some(reference) = reference {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["fetch", "--depth", "1", "origin", reference])
            .status()?;
        if !status.success() {
            bail!("git fetch of ref `{reference}` failed with {status}");
        }
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["checkout", "--detach", "FETCH_HEAD"])
            .status()?;
        if !status.success() {
            bail!("git checkout of ref `{reference}` failed with {status}");
        }
    }
    Ok(())
}

fn install_git_dependencies(root: &Path) -> Result<()> {
    let package = read_package_json(root)?;
    let text = fs::read_to_string(root.join("package.json"))?;
    let value: serde_json::Value = serde_json::from_str(&text)?;
    let has_dependencies = value
        .get("dependencies")
        .and_then(|v| v.as_object())
        .is_some_and(|v| !v.is_empty());
    if has_dependencies {
        let status = Command::new("npm")
            .arg("install")
            .arg("--ignore-scripts")
            .arg("--no-audit")
            .arg("--no-fund")
            .current_dir(root)
            .status()
            .context("running npm install for Git package dependencies")?;
        if !status.success() {
            bail!("npm install for Git package failed with {status}");
        }
    }
    drop(package);
    Ok(())
}

fn git_revision(root: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()?;
    if !output.status.success() {
        bail!("git rev-parse failed");
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn replace_directory(staged: &Path, destination: &Path) -> Result<Option<PathBuf>> {
    let backup = destination.with_extension("old");
    if backup.exists() {
        fs::remove_dir_all(&backup)?;
    }
    if destination.exists() {
        fs::rename(destination, &backup)?;
    }
    if let Err(error) = fs::rename(staged, destination) {
        if backup.exists() {
            let _ = fs::rename(&backup, destination);
        }
        return Err(error).with_context(|| format!("installing {}", destination.display()));
    }
    Ok(backup.exists().then_some(backup))
}

fn install_container(package: &InstalledPackage) -> Result<PathBuf> {
    let base = packages_dir()?;
    let kind = match package.kind {
        PackageKind::Npm => "npm",
        PackageKind::Git => "git",
    };
    Ok(base.join(kind).join(package_storage_slug(&package.id)))
}

fn safe_slug(value: &str) -> String {
    let slug = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    slug.trim_matches('-').to_string()
}

fn package_storage_slug(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let suffix = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{}-{suffix}", safe_slug(value))
}

fn git_name(url: &str) -> String {
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("package")
        .trim_end_matches(".git")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_npm_scopes_and_versions() {
        match parse_source("npm:@acme/tools@1.2.3").unwrap() {
            ParsedSource::Npm { name, pinned, .. } => {
                assert_eq!(name, "@acme/tools");
                assert!(pinned);
            }
            _ => panic!("expected npm"),
        }
        match parse_source("npm:tools").unwrap() {
            ParsedSource::Npm { name, pinned, .. } => {
                assert_eq!(name, "tools");
                assert!(!pinned);
            }
            _ => panic!("expected npm"),
        }
    }

    #[test]
    fn parses_git_refs_without_confusing_ssh_user() {
        match parse_source("git:git@github.com:acme/tools.git@v2").unwrap() {
            ParsedSource::Git { url, reference } => {
                assert_eq!(url, "git@github.com:acme/tools.git");
                assert_eq!(reference.as_deref(), Some("v2"));
            }
            _ => panic!("expected git"),
        }
    }

    #[test]
    fn refuses_escaping_resource_paths() {
        let root = Path::new("/tmp/package");
        assert!(safe_join(root, "../secret").is_err());
        assert!(safe_join(root, "/etc/passwd").is_err());
        assert_eq!(
            safe_join(root, "skills/review").unwrap(),
            root.join("skills/review")
        );
    }

    #[test]
    fn storage_names_do_not_collide_after_slug_normalization() {
        assert_eq!(safe_slug("@a/b"), safe_slug("a-b"));
        assert_ne!(package_storage_slug("@a/b"), package_storage_slug("a-b"));
    }

    #[test]
    fn discovers_conventional_resources_and_namespaces_them() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("package.json"),
            r#"{"name":"demo","version":"1.0.0"}"#,
        )
        .unwrap();
        fs::create_dir_all(temp.path().join("skills/review")).unwrap();
        fs::create_dir(temp.path().join("prompts")).unwrap();
        fs::create_dir(temp.path().join("themes")).unwrap();
        fs::write(
            temp.path().join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: Review carefully.\n---\n\n# Review carefully",
        )
        .unwrap();
        fs::write(temp.path().join("prompts/fix.md"), "Fix this").unwrap();
        fs::write(temp.path().join("themes/ocean.json"), r#"{"name":"ocean"}"#).unwrap();
        let package = InstalledPackage {
            id: "demo".into(),
            source: "git:x".into(),
            kind: PackageKind::Git,
            package_root: temp.path().display().to_string(),
            version: None,
            revision: None,
            pinned: false,
        };
        let mut resources = PackageResources::default();
        discover_package(&package, temp.path(), &mut resources).unwrap();
        assert!(resources.skills.contains_key("demo:review"));
        assert!(resources.prompts.contains_key("demo:fix"));
        assert!(resources.themes.contains_key("ocean"));
    }

    #[test]
    fn reads_portable_resources_from_pi_manifest() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("package.json"),
            r#"{"name":"portable","pi":{"skills":["shared/skills"],"prompts":["shared/prompts"]}}"#,
        )
        .unwrap();
        fs::create_dir_all(temp.path().join("shared/skills/check")).unwrap();
        fs::create_dir_all(temp.path().join("shared/prompts")).unwrap();
        fs::write(
            temp.path().join("shared/skills/check/SKILL.md"),
            "---\nname: check\ndescription: Check the work.\n---\n\n# Check",
        )
        .unwrap();
        fs::write(temp.path().join("shared/prompts/ask.md"), "Ask").unwrap();
        let package = InstalledPackage {
            id: "portable".into(),
            source: "npm:portable".into(),
            kind: PackageKind::Npm,
            package_root: temp.path().display().to_string(),
            version: None,
            revision: None,
            pinned: false,
        };
        let mut resources = PackageResources::default();
        discover_package(&package, temp.path(), &mut resources).unwrap();
        assert!(resources.skills.contains_key("portable:check"));
        assert!(resources.prompts.contains_key("portable:ask"));
    }
}
