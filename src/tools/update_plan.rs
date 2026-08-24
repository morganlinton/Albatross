use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::Tool;

/// A no-side-effect tool the model uses to maintain a short, visible task plan
/// across a multi-step turn. The plan lives in the conversation: each call
/// echoes the normalized steps back as the tool result, so the model re-reads
/// its own plan on the next step. The renderer special-cases this tool to draw
/// the plan as a checklist instead of a generic tool line.
pub struct UpdatePlanTool;

#[derive(Deserialize)]
struct Step {
    step: String,
    #[serde(default)]
    status: String,
}

#[derive(Deserialize)]
struct Args {
    steps: Vec<Step>,
}

fn normalize_status(raw: &str) -> &str {
    match raw.trim().to_lowercase().as_str() {
        "in_progress" | "in-progress" | "active" | "doing" => "in_progress",
        "done" | "completed" | "complete" | "finished" => "done",
        _ => "pending",
    }
}

#[async_trait]
impl Tool for UpdatePlanTool {
    fn name(&self) -> &str {
        "update_plan"
    }
    fn description(&self) -> &str {
        "Record or update a short step-by-step plan for a multi-step task. Call once with all steps before you start, then call again to flip a step's status as you go. Has no side effects. Use it for tasks of 3+ steps; skip it for trivial one-shot requests."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "steps": {
                    "type": "array",
                    "minItems": 1,
                    "description": "The full ordered plan. Re-send every step each call.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "step": { "type": "string", "description": "Short imperative description of the step" },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "done"],
                                "description": "Step state (default: pending). Keep exactly one step in_progress."
                            }
                        },
                        "required": ["step"]
                    }
                }
            },
            "required": ["steps"]
        })
    }
    async fn execute(&self, args: Value) -> Value {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return json!({ "error": format!("invalid args: {e}") }),
        };
        if args.steps.is_empty() {
            return json!({ "error": "steps must not be empty" });
        }
        let steps: Vec<Value> = args
            .steps
            .iter()
            .map(|s| json!({ "step": s.step, "status": normalize_status(&s.status) }))
            .collect();
        let done = steps.iter().filter(|s| s["status"] == "done").count();
        json!({
            "plan_updated": true,
            "total": steps.len(),
            "done": done,
            "steps": steps
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn normalizes_status_and_counts_done() {
        let tool = UpdatePlanTool;
        let out = tool
            .execute(json!({
                "steps": [
                    { "step": "read code", "status": "completed" },
                    { "step": "edit", "status": "doing" },
                    { "step": "test" }
                ]
            }))
            .await;
        assert_eq!(out["plan_updated"], true);
        assert_eq!(out["total"], 3);
        assert_eq!(out["done"], 1);
        assert_eq!(out["steps"][0]["status"], "done");
        assert_eq!(out["steps"][1]["status"], "in_progress");
        assert_eq!(out["steps"][2]["status"], "pending");
    }

    #[tokio::test]
    async fn empty_steps_is_an_error() {
        let tool = UpdatePlanTool;
        let out = tool.execute(json!({ "steps": [] })).await;
        assert!(out.get("error").is_some());
    }
}
