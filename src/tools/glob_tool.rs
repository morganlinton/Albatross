use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;

use super::{PathPolicy, Tool};

pub struct GlobTool {
    pub path_policy: PathPolicy,
}

#[derive(Deserialize)]
struct Args {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

const IGNORE_DIRS: &[&str] = &["node_modules", ".git", "dist"];

fn is_ignored(rel: &std::path::Path) -> bool {
    rel.components().any(|c| {
        if let std::path::Component::Normal(name) = c {
            if let Some(s) = name.to_str() {
                return IGNORE_DIRS.contains(&s);
            }
        }
        false
    })
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }
    fn description(&self) -> &str {
        "Find files by glob pattern. Skips node_modules/.git/dist. Returns up to 1000 paths."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern, e.g. \"src/**/*.ts\"" },
                "path": { "type": "string", "description": "Directory to search in (default: cwd)" }
            },
            "required": ["pattern"]
        })
    }
    fn require_approval(&self, args: &Value) -> bool {
        args.get("path")
            .and_then(Value::as_str)
            .map(|p| self.path_policy.require_prompt_for_path(p))
            .unwrap_or(false)
    }
    async fn execute(&self, args: Value) -> Value {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return json!({ "error": format!("invalid args: {e}") }),
        };
        let glob = match globset::GlobBuilder::new(&args.pattern)
            .literal_separator(true)
            .build()
        {
            Ok(g) => g.compile_matcher(),
            Err(e) => return json!({ "error": e.to_string() }),
        };
        let requested = args.path.unwrap_or_else(|| ".".into());
        if let Some(error) = self.path_policy.deny_path(&requested) {
            return json!({ "error": error });
        }
        let root: PathBuf = self.path_policy.resolve(&requested).normalized;

        let pattern = args.pattern.clone();
        let root_for_walk = root.clone();
        let results: Vec<String> = tokio::task::spawn_blocking(move || {
            let mut out: Vec<String> = Vec::new();
            let walker = walkdir::WalkDir::new(&root_for_walk)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| {
                    if let Some(name) = e.file_name().to_str() {
                        !IGNORE_DIRS.contains(&name)
                    } else {
                        true
                    }
                });
            for entry in walker.flatten() {
                if !entry.file_type().is_file() {
                    continue;
                }
                let rel = entry
                    .path()
                    .strip_prefix(&root_for_walk)
                    .unwrap_or(entry.path());
                if is_ignored(rel) {
                    continue;
                }
                if glob.is_match(rel) {
                    out.push(rel.to_string_lossy().into_owned());
                }
                if out.len() >= 2000 {
                    break;
                }
            }
            let _ = pattern;
            out
        })
        .await
        .unwrap_or_default();

        let count = results.len();
        let truncated = count > 1000;
        let matches: Vec<String> = results.into_iter().take(1000).collect();
        json!({ "matches": matches, "count": count, "truncated": truncated })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn ignored_dirs_match_at_any_depth() {
        assert!(is_ignored(Path::new("node_modules/foo.js")));
        assert!(is_ignored(Path::new("a/b/node_modules/c.js")));
        assert!(is_ignored(Path::new(".git/HEAD")));
        assert!(is_ignored(Path::new("nested/.git/HEAD")));
        assert!(is_ignored(Path::new("dist/main.js")));
    }

    #[test]
    fn normal_paths_not_ignored() {
        assert!(!is_ignored(Path::new("src/main.rs")));
        assert!(!is_ignored(Path::new("docs/notes.md")));
        assert!(!is_ignored(Path::new("Cargo.toml")));
    }

    #[test]
    fn similarly_named_dirs_not_ignored() {
        assert!(!is_ignored(Path::new("src/distance.rs")));
        assert!(!is_ignored(Path::new("docs/git-tips.md")));
    }
}
