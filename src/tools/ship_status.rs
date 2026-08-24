use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::Tool;
use crate::shipcheck::collect_shipcheck_with_tests;

pub struct ShipStatusTool {
    pub workspace_root: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Args {
    #[serde(default)]
    include_tests: bool,
}

#[async_trait]
impl Tool for ShipStatusTool {
    fn name(&self) -> &str {
        "ship_status"
    }
    fn description(&self) -> &str {
        "Read-only git ship readiness snapshot: branch drift, dirty files, diff stats, optional tests, ready_to_ship heuristic."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "includeTests": {
                    "type": "boolean",
                    "description": "Run the test suite as part of the snapshot (default: false)"
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> Value {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return json!({ "error": format!("invalid args: {e}") }),
        };
        match collect_shipcheck_with_tests(&self.workspace_root, args.include_tests) {
            Ok(snapshot) => serde_json::to_value(snapshot.to_agent_json())
                .unwrap_or_else(|e| json!({ "error": e.to_string() })),
            Err(e) => json!({ "error": e.to_string() }),
        }
    }
}
