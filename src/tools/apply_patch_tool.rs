use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

use super::{PathPolicy, Tool, ToolPreview};

pub struct ApplyPatchTool {
    pub approve: bool,
    pub path_policy: PathPolicy,
}

#[derive(Deserialize)]
struct Args {
    patch: String,
    #[serde(default)]
    path: Option<String>,
}

pub fn patch_changed_files(patch: &str) -> Vec<String> {
    changed_files(patch)
}

fn changed_files(patch: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in patch.lines() {
        let Some(path) = line
            .strip_prefix("+++ b/")
            .or_else(|| line.strip_prefix("--- a/"))
        else {
            continue;
        };
        if path != "/dev/null" && !out.iter().any(|p| p == path) {
            out.push(path.to_string());
        }
    }
    out
}

fn patch_has_unsafe_paths(patch: &str) -> bool {
    changed_files(patch).into_iter().any(|path| {
        path.starts_with('/')
            || path
                .split('/')
                .any(|segment| segment == ".." || segment.is_empty())
    })
}

async fn git_apply(cwd: &std::path::Path, patch: &str, check_only: bool) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.current_dir(cwd)
        .arg("apply")
        .arg("--whitespace=nowarn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if check_only {
        cmd.arg("--check");
    }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(patch.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
    }
    let output = child.wait_with_output().await.map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Apply a unified diff patch after validating it with `git apply --check`."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "description": "Unified diff text to apply" },
                "path": { "type": "string", "description": "Directory to apply the patch in (default: cwd)" }
            },
            "required": ["patch"]
        })
    }

    fn require_approval(&self, args: &Value) -> bool {
        let path_prompt = args
            .get("path")
            .and_then(Value::as_str)
            .map(|p| self.path_policy.require_prompt_for_path(p))
            .unwrap_or_else(|| self.path_policy.require_prompt_for_cwd());
        self.approve || path_prompt
    }

    async fn preview(&self, args: &Value) -> Option<ToolPreview> {
        let args: Args = serde_json::from_value(args.clone()).ok()?;
        let files = changed_files(&args.patch);
        let mut risk = None;
        if patch_has_unsafe_paths(&args.patch) {
            risk = Some("patch contains unsafe absolute, empty, or parent path segments".into());
        } else if let Some(path) = args.path.as_deref() {
            if self.path_policy.resolve(path).outside_workspace {
                risk = Some(format!(
                    "outside workspace root {}",
                    self.path_policy.root().display()
                ));
            }
        }
        Some(ToolPreview {
            summary: format!("Apply patch touching {} file(s)", files.len()),
            diff: Some(args.patch),
            risk,
        })
    }

    async fn execute(&self, args: Value) -> Value {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return json!({ "error": format!("invalid args: {e}") }),
        };
        if patch_has_unsafe_paths(&args.patch) {
            return json!({ "error": "patch contains unsafe absolute, empty, or parent path segments" });
        }
        let requested = args.path.unwrap_or_else(|| ".".into());
        if let Some(error) = self.path_policy.deny_path(&requested) {
            return json!({ "error": error });
        }
        let cwd = self.path_policy.resolve(&requested).normalized;
        let files = changed_files(&args.patch);
        if let Err(e) = git_apply(&cwd, &args.patch, true).await {
            return json!({ "applied": false, "error": e, "files": files });
        }
        match git_apply(&cwd, &args.patch, false).await {
            Ok(_) => json!({
                "applied": true,
                "path": cwd.display().to_string(),
                "files": files,
                "diff": args.patch,
            }),
            Err(e) => json!({ "applied": false, "error": e, "files": files }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OutsideWorkspace;

    #[test]
    fn extracts_changed_files() {
        let patch = "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n";
        assert_eq!(changed_files(patch), vec!["a.txt".to_string()]);
    }

    #[test]
    fn rejects_parent_paths() {
        let patch = "--- a/../x\n+++ b/../x\n";
        assert!(patch_has_unsafe_paths(patch));
    }

    #[tokio::test]
    async fn applies_valid_patch() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        tokio::fs::write(&file, "old\n").await.unwrap();
        let patch = "--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n";
        let tool = ApplyPatchTool {
            approve: false,
            path_policy: PathPolicy::new(dir.path().to_str().unwrap(), OutsideWorkspace::Allow),
        };
        let result = tool
            .execute(json!({ "path": dir.path().to_str().unwrap(), "patch": patch }))
            .await;
        assert!(result["applied"].as_bool().unwrap());
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), "new\n");
    }
}
