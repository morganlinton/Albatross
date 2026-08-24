//! A fixed-surface test runner for the evaluator (#5 live verification).
//!
//! The critic runs read-only with `approval=None`, so any approval-gated tool
//! (including `shell` and the normal `run_tests`) is auto-denied. `verify` is
//! the deliberate exception: it accepts NO arbitrary command — only the
//! project's configured tests, optionally filtered by a name pattern — and the
//! invocation is argv-based (see `test_integration::build_test_invocation`), so a
//! pattern cannot inject a shell command. That fixed surface is why it can run
//! without an approval gate. A timeout bounds a hung suite so the critic can't
//! stall the `/iterate` loop forever.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

use super::Tool;
use crate::test_integration::{discover_tests, run_tests, test_result_to_agent_json};

pub struct VerifyTool {
    pub workspace_root: String,
    pub timeout: Duration,
}

#[derive(Deserialize)]
struct Args {
    #[serde(default)]
    pattern: Option<String>,
}

#[async_trait]
impl Tool for VerifyTool {
    fn name(&self) -> &str {
        "verify"
    }
    fn description(&self) -> &str {
        "Run the project's test suite (optionally filtered by a name pattern) and return structured pass/fail results. Takes no arbitrary command — only the project's configured tests run. Call this to verify the work actually functions before scoring functionality."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Optional test-name filter; omit to run the whole suite."
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> Value {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return json!({ "error": format!("invalid args: {e}") }),
        };
        let root = self.workspace_root.clone();
        let pattern = args.pattern.filter(|p| !p.trim().is_empty());
        let timeout = self.timeout;

        // run_tests is blocking; run it off the async executor so the timeout can
        // actually fire (a hung suite continues on its thread but we stop waiting).
        let job = tokio::task::spawn_blocking(move || {
            let result = run_tests(&root, pattern.as_deref())?;
            let framework = discover_tests(&root)
                .map(|d| d.framework)
                .unwrap_or_else(|_| "unknown".into());
            anyhow::Ok(test_result_to_agent_json(&framework, &result))
        });

        match tokio::time::timeout(timeout, job).await {
            Ok(Ok(Ok(value))) => {
                serde_json::to_value(value).unwrap_or_else(|e| json!({ "error": e.to_string() }))
            }
            Ok(Ok(Err(e))) => json!({ "error": e.to_string() }),
            Ok(Err(join_err)) => json!({ "error": format!("verify task failed: {join_err}") }),
            Err(_) => json!({
                "error": format!("verify timed out after {}s", timeout.as_secs()),
                "timedOut": true,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(root: &str) -> VerifyTool {
        VerifyTool {
            workspace_root: root.into(),
            timeout: Duration::from_secs(60),
        }
    }

    #[test]
    fn verify_never_requires_approval() {
        // The fixed surface is the whole reason it runs inside the read-only
        // critic without an approval provider.
        let t = tool("/tmp");
        assert!(!t.require_approval(&json!({})));
    }

    #[tokio::test]
    async fn verify_on_non_project_returns_json_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let out = tool(dir.path().to_str().unwrap()).execute(json!({})).await;
        // Either a structured result or a graceful error — never a panic.
        assert!(out.is_object());
    }
}
