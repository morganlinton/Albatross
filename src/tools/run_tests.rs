use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::Tool;
use crate::config::ApprovalPolicy;
use crate::test_integration::{
    discover_tests, run_selected_tests, run_tests, smart_test_selection, test_result_to_agent_json,
};

pub struct RunTestsTool {
    pub workspace_root: String,
    pub policy: ApprovalPolicy,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Args {
    #[serde(default = "default_mode")]
    mode: String,
    pattern: Option<String>,
}

fn default_mode() -> String {
    "smart".into()
}

#[async_trait]
impl Tool for RunTestsTool {
    fn name(&self) -> &str {
        "run_tests"
    }
    fn description(&self) -> &str {
        "Run project tests with structured JSON results. Modes: discover (list framework/files), smart (changed files), all (full suite), pattern (filter by name)."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "mode": {
                    "type": "string",
                    "enum": ["discover", "smart", "all", "pattern"],
                    "description": "Test run mode (default: smart)"
                },
                "pattern": {
                    "type": "string",
                    "description": "Test name filter when mode is pattern"
                }
            }
        })
    }
    fn require_approval(&self, _args: &Value) -> bool {
        self.policy == ApprovalPolicy::Always
    }
    async fn execute(&self, args: Value) -> Value {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return json!({ "error": format!("invalid args: {e}") }),
        };
        let root = self.workspace_root.as_str();
        match args.mode.as_str() {
            "discover" => match discover_tests(root) {
                Ok(discovery) => json!(discovery),
                Err(e) => json!({ "error": e.to_string() }),
            },
            "smart" => {
                let selected = match smart_test_selection(root) {
                    Ok(s) => s,
                    Err(e) => return json!({ "error": e.to_string() }),
                };
                if selected.is_empty() {
                    return json!({
                        "framework": discover_tests(root).map(|d| d.framework).unwrap_or_else(|_| "unknown".into()),
                        "total": 0,
                        "passed": 0,
                        "failed": 0,
                        "skipped": 0,
                        "exitCode": 0,
                        "failures": [],
                        "outputExcerpt": "No changed files; no tests selected.",
                        "selectedTests": selected,
                    });
                }
                match run_selected_tests(root, &selected) {
                    Ok(result) => {
                        let framework = discover_tests(root)
                            .map(|d| d.framework)
                            .unwrap_or_else(|_| "unknown".into());
                        let mut out =
                            serde_json::to_value(test_result_to_agent_json(&framework, &result))
                                .unwrap_or_else(|e| json!({ "error": e.to_string() }));
                        if let Value::Object(ref mut map) = out {
                            map.insert("selectedTests".into(), json!(selected));
                        }
                        out
                    }
                    Err(e) => json!({ "error": e.to_string() }),
                }
            }
            "all" => match run_tests(root, None) {
                Ok(result) => {
                    let framework = discover_tests(root)
                        .map(|d| d.framework)
                        .unwrap_or_else(|_| "unknown".into());
                    serde_json::to_value(test_result_to_agent_json(&framework, &result))
                        .unwrap_or_else(|e| json!({ "error": e.to_string() }))
                }
                Err(e) => json!({ "error": e.to_string() }),
            },
            "pattern" => {
                let pattern = args.pattern.as_deref().filter(|p| !p.is_empty());
                if pattern.is_none() {
                    return json!({ "error": "pattern is required when mode is pattern" });
                }
                match run_tests(root, pattern) {
                    Ok(result) => {
                        let framework = discover_tests(root)
                            .map(|d| d.framework)
                            .unwrap_or_else(|_| "unknown".into());
                        serde_json::to_value(test_result_to_agent_json(&framework, &result))
                            .unwrap_or_else(|e| json!({ "error": e.to_string() }))
                    }
                    Err(e) => json!({ "error": e.to_string() }),
                }
            }
            other => json!({ "error": format!("unknown mode: {other}") }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ApprovalPolicy;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    fn init_git(dir: &Path) {
        if Command::new("git")
            .args(["-C", dir.to_str().unwrap(), "init"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            let _ = Command::new("git")
                .args(["-C", dir.to_str().unwrap(), "add", "."])
                .output();
        }
    }

    #[tokio::test]
    async fn discover_mode_on_cargo_project() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "pub fn ok() {}\n").unwrap();
        init_git(dir.path());
        let tool = RunTestsTool {
            workspace_root: dir.path().to_str().unwrap().into(),
            policy: ApprovalPolicy::Never,
        };
        let out = tool.execute(json!({ "mode": "discover" })).await;
        assert_eq!(out.get("framework").and_then(|v| v.as_str()), Some("cargo"));
    }
}
