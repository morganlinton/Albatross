//! Agent Skills standard discovery and progressive activation.

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::{DirEntry, WalkDir};

use crate::packages::TextResource;
use crate::tools::{Tool, ToolPreview};

const MAX_SKILL_BYTES: u64 = 1024 * 1024;
const MAX_DISCOVERY_DEPTH: usize = 6;
const MAX_RESOURCE_DEPTH: usize = 5;
const MAX_LISTED_RESOURCES: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillScope {
    User,
    Package,
    Project,
}

impl SkillScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Package => "package",
            Self::Project => "project",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub license: Option<String>,
    pub compatibility: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub allowed_tools: Option<String>,
    pub path: PathBuf,
    pub directory: PathBuf,
    pub scope: SkillScope,
    pub source: String,
}

#[derive(Debug, Clone, Default)]
pub struct SkillRegistry {
    skills: BTreeMap<String, Skill>,
    aliases: BTreeMap<String, Skill>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    compatibility: Option<String>,
    #[serde(default)]
    metadata: BTreeMap<String, String>,
    #[serde(default)]
    allowed_tools: Option<String>,
}

#[derive(Debug)]
struct Candidate {
    path: PathBuf,
    scope: SkillScope,
    source: String,
    priority: u8,
    alias_prefix: Option<String>,
}

impl SkillRegistry {
    pub fn discover(workspace_root: &str, packaged: &BTreeMap<String, TextResource>) -> Self {
        let mut registry = Self::default();
        let mut candidates = Vec::new();

        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            collect_root(
                &home.join(".agents/skills"),
                SkillScope::User,
                "~/.agents/skills",
                10,
                &mut candidates,
                &mut registry.diagnostics,
            );
        }
        if let Some(root) = crate::auth::config_dir().map(|dir| dir.join("skills")) {
            collect_root(
                &root,
                SkillScope::User,
                "albatross user skills",
                20,
                &mut candidates,
                &mut registry.diagnostics,
            );
        }
        for resource in packaged.values() {
            let prefix = resource
                .name
                .split_once(':')
                .map(|(prefix, _)| prefix)
                .unwrap_or(&resource.package)
                .to_string();
            candidates.push(Candidate {
                path: resource.path.clone(),
                scope: SkillScope::Package,
                source: resource.package.clone(),
                priority: 30,
                alias_prefix: Some(prefix),
            });
        }

        let workspace = PathBuf::from(workspace_root);
        collect_root(
            &workspace.join(".agents/skills"),
            SkillScope::Project,
            ".agents/skills",
            40,
            &mut candidates,
            &mut registry.diagnostics,
        );
        collect_root(
            &workspace.join(".albatross/skills"),
            SkillScope::Project,
            ".albatross/skills",
            50,
            &mut candidates,
            &mut registry.diagnostics,
        );

        Self::from_candidates(candidates, registry.diagnostics)
    }

    fn from_candidates(mut candidates: Vec<Candidate>, diagnostics: Vec<String>) -> Self {
        let mut registry = Self {
            diagnostics,
            ..Self::default()
        };
        candidates.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.path.cmp(&right.path))
        });
        let mut seen_paths = BTreeSet::new();
        for candidate in candidates {
            let canonical = match candidate.path.canonicalize() {
                Ok(path) => path,
                Err(error) => {
                    registry.diagnostics.push(format!(
                        "{}: could not resolve {}: {error}",
                        candidate.source,
                        candidate.path.display()
                    ));
                    continue;
                }
            };
            if !seen_paths.insert(canonical.clone()) {
                continue;
            }
            match parse_skill(&canonical, candidate.scope, &candidate.source) {
                Ok(skill) => {
                    if let Some(prefix) = candidate.alias_prefix {
                        registry
                            .aliases
                            .insert(format!("{prefix}:{}", skill.name), skill.clone());
                    }
                    if let Some(shadowed) =
                        registry.skills.insert(skill.name.clone(), skill.clone())
                    {
                        registry.diagnostics.push(format!(
                            "skill `{}` from {} shadows {}",
                            skill.name, skill.source, shadowed.source
                        ));
                    }
                }
                Err(error) => registry.diagnostics.push(format!(
                    "{}: {}: {error}",
                    candidate.source,
                    canonical.display()
                )),
            }
        }
        registry
    }

    pub fn skills(&self) -> impl Iterator<Item = &Skill> {
        self.skills.values()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name).or_else(|| self.aliases.get(name))
    }

    pub fn command_entries(&self) -> Vec<(String, String)> {
        let mut entries = self
            .skills
            .values()
            .map(|skill| (format!("/skill:{}", skill.name), skill.description.clone()))
            .collect::<Vec<_>>();
        entries.extend(self.aliases.iter().map(|(alias, skill)| {
            (
                format!("/skill:{alias}"),
                format!("{} ({})", skill.description, skill.source),
            )
        }));
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries.dedup_by(|a, b| a.0 == b.0);
        entries
    }

    pub fn activate(&self, name: &str) -> Result<String> {
        let skill = self
            .get(name)
            .ok_or_else(|| anyhow!("unknown skill: {name}"))?;
        activate_skill(skill)
    }
}

fn collect_root(
    root: &Path,
    scope: SkillScope,
    source: &str,
    priority: u8,
    candidates: &mut Vec<Candidate>,
    diagnostics: &mut Vec<String>,
) {
    if !root.is_dir() {
        return;
    }
    for entry in WalkDir::new(root)
        .max_depth(MAX_DISCOVERY_DEPTH)
        .follow_links(false)
        .into_iter()
        .filter_entry(scan_entry)
    {
        match entry {
            Ok(entry)
                if entry.file_type().is_file()
                    && entry.file_name().to_str() == Some("SKILL.md") =>
            {
                candidates.push(Candidate {
                    path: entry.into_path(),
                    scope,
                    source: source.to_string(),
                    priority,
                    alias_prefix: None,
                });
            }
            Ok(_) => {}
            Err(error) => diagnostics.push(format!("{source}: discovery error: {error}")),
        }
    }
}

fn scan_entry(entry: &DirEntry) -> bool {
    if entry.file_type().is_symlink() {
        return false;
    }
    !matches!(
        entry.file_name().to_str(),
        Some(".git" | "node_modules" | "target")
    )
}

fn parse_skill(path: &Path, scope: SkillScope, source: &str) -> Result<Skill> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_SKILL_BYTES {
        bail!("SKILL.md exceeds the 1 MiB safety limit");
    }
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let (frontmatter, _) = split_frontmatter(&text)?;
    let frontmatter: SkillFrontmatter =
        serde_yaml::from_str(frontmatter).context("invalid YAML frontmatter")?;
    validate_frontmatter(&frontmatter, path)?;
    let directory = path
        .parent()
        .ok_or_else(|| anyhow!("SKILL.md has no parent directory"))?
        .to_path_buf();
    Ok(Skill {
        name: frontmatter.name,
        description: frontmatter.description,
        license: frontmatter.license,
        compatibility: frontmatter.compatibility,
        metadata: frontmatter.metadata,
        allowed_tools: frontmatter.allowed_tools,
        path: path.to_path_buf(),
        directory,
        scope,
        source: source.to_string(),
    })
}

fn split_frontmatter(text: &str) -> Result<(&str, &str)> {
    let mut offset = 0usize;
    let mut lines = text.split_inclusive('\n');
    let first = lines.next().ok_or_else(|| anyhow!("SKILL.md is empty"))?;
    if first.trim_end_matches(['\r', '\n']) != "---" {
        bail!("SKILL.md must start with YAML frontmatter (`---`)");
    }
    offset += first.len();
    let yaml_start = offset;
    for line in lines {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let yaml_end = offset;
            offset += line.len();
            return Ok((&text[yaml_start..yaml_end], text[offset..].trim()));
        }
        offset += line.len();
    }
    bail!("SKILL.md frontmatter has no closing `---`")
}

fn validate_frontmatter(frontmatter: &SkillFrontmatter, path: &Path) -> Result<()> {
    let name = frontmatter.name.as_str();
    if name.is_empty() || name.chars().count() > 64 {
        bail!("name must contain 1-64 characters");
    }
    if name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
        || !name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        bail!("name must use lowercase ASCII letters, numbers, and single hyphens");
    }
    let parent = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if parent != name {
        bail!("name `{name}` must match parent directory `{parent}`");
    }
    let description_len = frontmatter.description.chars().count();
    if frontmatter.description.trim().is_empty() || description_len > 1024 {
        bail!("description must contain 1-1024 characters");
    }
    if frontmatter
        .compatibility
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.chars().count() > 500)
    {
        bail!("compatibility must contain 1-500 characters when present");
    }
    Ok(())
}

fn activate_skill(skill: &Skill) -> Result<String> {
    let text = fs::read_to_string(&skill.path)
        .with_context(|| format!("reading {}", skill.path.display()))?;
    let (_, body) = split_frontmatter(&text)?;
    let resources = list_resources(&skill.directory)?;
    let mut output = format!(
        "<skill_content name=\"{}\">\n{}\n\nSkill directory: {}\nRelative paths in this skill are relative to the skill directory.",
        xml_escape(&skill.name),
        body,
        skill.directory.display()
    );
    if skill.license.is_some()
        || skill.compatibility.is_some()
        || skill.allowed_tools.is_some()
        || !skill.metadata.is_empty()
    {
        output.push_str("\n\n<skill_metadata>");
        if let Some(license) = &skill.license {
            output.push_str("\n  <license>");
            output.push_str(&xml_escape(license));
            output.push_str("</license>");
        }
        if let Some(compatibility) = &skill.compatibility {
            output.push_str("\n  <compatibility>");
            output.push_str(&xml_escape(compatibility));
            output.push_str("</compatibility>");
        }
        if let Some(allowed_tools) = &skill.allowed_tools {
            output.push_str("\n  <allowed-tools experimental=\"true\">");
            output.push_str(&xml_escape(allowed_tools));
            output.push_str("</allowed-tools>");
        }
        for (key, value) in &skill.metadata {
            output.push_str("\n  <entry key=\"");
            output.push_str(&xml_escape(key));
            output.push_str("\">");
            output.push_str(&xml_escape(value));
            output.push_str("</entry>");
        }
        output.push_str("\n</skill_metadata>");
    }
    if !resources.is_empty() {
        output.push_str("\n\n<skill_resources>");
        for resource in resources {
            output.push_str("\n  <file>");
            output.push_str(&xml_escape(&resource));
            output.push_str("</file>");
        }
        output.push_str("\n</skill_resources>");
    }
    output.push_str("\n</skill_content>");
    Ok(output)
}

fn list_resources(directory: &Path) -> Result<Vec<String>> {
    let mut resources = Vec::new();
    for entry in WalkDir::new(directory)
        .max_depth(MAX_RESOURCE_DEPTH)
        .follow_links(false)
        .into_iter()
        .filter_entry(scan_entry)
    {
        let entry = entry?;
        if !entry.file_type().is_file() || entry.path() == directory.join("SKILL.md") {
            continue;
        }
        if let Ok(relative) = entry.path().strip_prefix(directory) {
            resources.push(relative.display().to_string());
            if resources.len() == MAX_LISTED_RESOURCES {
                break;
            }
        }
    }
    resources.sort();
    Ok(resources)
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn append_catalog(prompt: &mut String, registry: &SkillRegistry) {
    if registry.is_empty() {
        return;
    }
    prompt.push_str("\n\nAvailable Agent Skills:\n");
    prompt.push_str("When a task matches a skill description, call activate_skill with its name before proceeding. Load referenced resources only as needed.\n<available_skills>");
    for skill in registry.skills() {
        prompt.push_str("\n  <skill>\n    <name>");
        prompt.push_str(&xml_escape(&skill.name));
        prompt.push_str("</name>\n    <description>");
        prompt.push_str(&xml_escape(&skill.description));
        prompt.push_str("</description>\n  </skill>");
    }
    prompt.push_str("\n</available_skills>");
}

pub fn activation_tool(registry: &SkillRegistry) -> Option<Arc<dyn Tool>> {
    (!registry.is_empty()).then(|| {
        Arc::new(ActivateSkillTool {
            registry: registry.clone(),
        }) as Arc<dyn Tool>
    })
}

struct ActivateSkillTool {
    registry: SkillRegistry,
}

#[async_trait]
impl Tool for ActivateSkillTool {
    fn name(&self) -> &str {
        "activate_skill"
    }

    fn description(&self) -> &str {
        "Load the full instructions and bundled-resource index for an available Agent Skill. Call this before acting when the user's task matches a skill description."
    }

    fn input_schema(&self) -> Value {
        let names = self
            .registry
            .skills()
            .map(|skill| Value::String(skill.name.clone()))
            .collect::<Vec<_>>();
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "enum": names,
                    "description": "Exact skill name from the available skills catalog"
                }
            },
            "required": ["name"]
        })
    }

    fn require_approval(&self, args: &Value) -> bool {
        args.get("name")
            .and_then(Value::as_str)
            .and_then(|name| self.registry.get(name))
            .is_some_and(|skill| skill.scope == SkillScope::Project)
    }

    async fn preview(&self, args: &Value) -> Option<ToolPreview> {
        let name = args.get("name")?.as_str()?;
        let skill = self.registry.get(name)?;
        Some(ToolPreview {
            summary: format!("Activate {} skill `{}`", skill.scope.as_str(), skill.name),
            diff: None,
            risk: (skill.scope == SkillScope::Project).then(|| {
                "Project skills contain repository-controlled instructions and may reference executable scripts.".into()
            }),
        })
    }

    async fn execute(&self, args: Value) -> Value {
        let Some(name) = args.get("name").and_then(Value::as_str) else {
            return json!({"error": "name is required"});
        };
        match self.registry.activate(name) {
            Ok(content) => json!({"skill": name, "content": content}),
            Err(error) => json!({"error": error.to_string()}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, name: &str, description: &str) -> PathBuf {
        let directory = root.join(name);
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("SKILL.md");
        fs::write(
            &path,
            format!("---\nname: {name}\ndescription: {description}\nmetadata:\n  author: test\n---\n\n# Instructions\n\nDo the thing.\n"),
        )
        .unwrap();
        path
    }

    #[test]
    fn parses_standard_frontmatter_and_body() {
        let temp = tempfile::tempdir().unwrap();
        let path = write_skill(temp.path(), "code-review", "Review code safely.");
        let skill = parse_skill(&path, SkillScope::User, "test").unwrap();
        assert_eq!(skill.name, "code-review");
        assert_eq!(
            skill.metadata.get("author").map(String::as_str),
            Some("test")
        );
        assert!(activate_skill(&skill).unwrap().contains("Do the thing."));
    }

    #[test]
    fn enforces_standard_name_and_parent_constraints() {
        let temp = tempfile::tempdir().unwrap();
        let bad_case = write_skill(temp.path(), "BadName", "Description");
        assert!(parse_skill(&bad_case, SkillScope::User, "test").is_err());
        let path = write_skill(temp.path(), "folder", "Description");
        fs::write(
            &path,
            "---\nname: different\ndescription: Description\n---\nBody",
        )
        .unwrap();
        assert!(parse_skill(&path, SkillScope::User, "test").is_err());
    }

    #[test]
    fn activation_lists_resources_without_loading_them() {
        let temp = tempfile::tempdir().unwrap();
        let path = write_skill(temp.path(), "pdf", "Work with PDFs.");
        fs::create_dir_all(path.parent().unwrap().join("references")).unwrap();
        fs::write(
            path.parent().unwrap().join("references/DETAILS.md"),
            "secret details",
        )
        .unwrap();
        let skill = parse_skill(&path, SkillScope::User, "test").unwrap();
        let activated = activate_skill(&skill).unwrap();
        assert!(activated.contains("references/DETAILS.md"));
        assert!(!activated.contains("secret details"));
    }

    #[test]
    fn catalog_discloses_metadata_without_loading_instructions() {
        let temp = tempfile::tempdir().unwrap();
        let path = write_skill(temp.path(), "review", "Review changes.");
        let skill = parse_skill(&path, SkillScope::User, "test").unwrap();
        let mut registry = SkillRegistry::default();
        registry.skills.insert(skill.name.clone(), skill);
        let mut prompt = String::new();
        append_catalog(&mut prompt, &registry);

        assert!(prompt.contains("Review changes."));
        assert!(!prompt.contains("Do the thing."));
    }

    #[tokio::test]
    async fn activation_tool_requires_approval_only_for_project_skills() {
        let temp = tempfile::tempdir().unwrap();
        let path = write_skill(temp.path(), "review", "Review changes.");
        let project = parse_skill(&path, SkillScope::Project, "project").unwrap();
        let mut registry = SkillRegistry::default();
        registry.skills.insert(project.name.clone(), project);
        let tool = ActivateSkillTool { registry };
        assert!(tool.require_approval(&json!({"name": "review"})));
        assert!(!tool.require_approval(&json!({"name": "missing"})));
        let result = tool.execute(json!({"name": "review"})).await;
        assert!(result["content"].as_str().unwrap().contains("Instructions"));
    }

    #[test]
    fn project_skills_override_packages_and_package_aliases_remain_addressable() {
        let temp = tempfile::tempdir().unwrap();
        let user = write_skill(&temp.path().join("user"), "review", "User review.");
        let package = write_skill(&temp.path().join("package"), "review", "Package review.");
        let project = write_skill(&temp.path().join("project"), "review", "Project review.");
        let registry = SkillRegistry::from_candidates(
            vec![
                Candidate {
                    path: user,
                    scope: SkillScope::User,
                    source: "user".into(),
                    priority: 10,
                    alias_prefix: None,
                },
                Candidate {
                    path: package,
                    scope: SkillScope::Package,
                    source: "tools".into(),
                    priority: 30,
                    alias_prefix: Some("tools".into()),
                },
                Candidate {
                    path: project,
                    scope: SkillScope::Project,
                    source: "project".into(),
                    priority: 50,
                    alias_prefix: None,
                },
            ],
            Vec::new(),
        );

        assert_eq!(
            registry.get("review").unwrap().description,
            "Project review."
        );
        assert_eq!(
            registry.get("tools:review").unwrap().description,
            "Package review."
        );
        assert_eq!(registry.diagnostics.len(), 2);
    }
}
