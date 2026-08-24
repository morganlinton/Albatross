use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;

use super::{Tool, ToolPreview};
use crate::batch_operations::{
    execute_batch_operations, preview_batch_operations, BatchEditOperation,
};

pub struct BatchEditTool {
    pub workspace_root: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Args {
    operations: Vec<BatchEditOperation>,
    #[serde(default = "default_dry_run")]
    dry_run: bool,
}

fn default_dry_run() -> bool {
    true
}

fn op_summary(operations: &[BatchEditOperation]) -> String {
    let files: Vec<&str> = operations.iter().map(|op| op.file_path.as_str()).collect();
    format!(
        "{} operation(s) across {} file(s)",
        operations.len(),
        files.len()
    )
}

#[async_trait]
impl Tool for BatchEditTool {
    fn name(&self) -> &str {
        "batch_edit"
    }
    fn description(&self) -> &str {
        "Preview or apply coordinated multi-file edits. Always returns a dry-run preview; set dry_run=false to apply after approval."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operations": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "filePath": { "type": "string" },
                            "operation": {
                                "type": "object",
                                "description": "replace, insert, or delete operation (same schema as /batch JSON)"
                            }
                        },
                        "required": ["filePath", "operation"]
                    }
                },
                "dryRun": {
                    "type": "boolean",
                    "description": "Preview only when true (default). Apply when false."
                }
            },
            "required": ["operations"]
        })
    }
    fn require_approval(&self, args: &Value) -> bool {
        !args
            .get("dryRun")
            .or_else(|| args.get("dry_run"))
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }
    async fn preview(&self, args: &Value) -> Option<ToolPreview> {
        let args: Args = serde_json::from_value(args.clone()).ok()?;
        if args.dry_run {
            return None;
        }
        let summary = op_summary(&args.operations);
        let file_list = args
            .operations
            .iter()
            .map(|op| op.file_path.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        Some(ToolPreview {
            summary: format!("Apply batch edit: {summary}"),
            diff: None,
            risk: Some(format!("Will modify: {file_list}")),
        })
    }
    async fn execute(&self, args: Value) -> Value {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return json!({ "error": format!("invalid args: {e}") }),
        };
        if args.operations.is_empty() {
            return json!({ "error": "operations must not be empty" });
        }
        let preview = preview_batch_operations(&args.operations);
        let workspace = PathBuf::from(&self.workspace_root);
        let validation = match execute_batch_operations(&args.operations, &workspace, true) {
            Ok(v) => v,
            Err(e) => return json!({ "error": e.to_string(), "preview": preview }),
        };
        if !validation.failed.is_empty() {
            return json!({
                "preview": preview,
                "applied": false,
                "successful": validation.successful,
                "failed": validation.failed,
            });
        }
        if args.dry_run {
            return json!({
                "preview": preview,
                "applied": false,
                "successful": validation.successful,
                "failed": validation.failed,
                "dryRun": true,
            });
        }
        match execute_batch_operations(&args.operations, &workspace, false) {
            Ok(applied) => json!({
                "preview": preview,
                "applied": true,
                "successful": applied.successful,
                "failed": applied.failed,
            }),
            Err(e) => json!({ "error": e.to_string(), "preview": preview }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parses_tool_args_json() {
        let v = json!({
            "operations": [{
                "filePath": "hello.txt",
                "operation": { "type": "replace", "old_string": "world", "new_string": "rust" }
            }],
            "dryRun": true
        });
        let parsed: Args = serde_json::from_value(v).expect("parse tool args");
        assert_eq!(parsed.operations.len(), 1);
        assert!(parsed.dry_run);
    }

    #[tokio::test]
    async fn dry_run_preview_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        fs::write(&path, "hello world\n").unwrap();
        let tool = BatchEditTool {
            workspace_root: dir.path().to_str().unwrap().into(),
        };
        let out = tool
            .execute(json!({
                "operations": [{
                    "filePath": "hello.txt",
                    "operation": { "type": "replace", "old_string": "world", "new_string": "rust" }
                }],
                "dryRun": true
            }))
            .await;
        if out.get("applied").is_none() {
            panic!("unexpected batch_edit output: {out}");
        }
        assert_eq!(out.get("applied").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello world\n");
    }

    #[tokio::test]
    async fn apply_writes_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        fs::write(&path, "hello world\n").unwrap();
        let tool = BatchEditTool {
            workspace_root: dir.path().to_str().unwrap().into(),
        };
        let out = tool
            .execute(json!({
                "operations": [{
                    "filePath": "hello.txt",
                    "operation": { "type": "replace", "old_string": "world", "new_string": "rust" }
                }],
                "dryRun": false
            }))
            .await;
        assert_eq!(out.get("applied").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello rust\n");
    }
}
