use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

use crate::config::AgentConfig;
use crate::project_memory::{add_snippets, load_project_index, search_index};

use super::Tool;

pub struct RepoSearchTool {
    pub config: AgentConfig,
}

#[derive(Deserialize)]
struct Args {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl Tool for RepoSearchTool {
    fn name(&self) -> &str {
        "repo_search"
    }

    fn description(&self) -> &str {
        "Search the local project memory index for relevant files, symbols, headings, imports, and short snippets. Run /index first if the index is missing."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Repo/code question or keywords to search for" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 20, "description": "Maximum hits to return" }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: Value) -> Value {
        if !self.config.project_memory.enabled {
            return json!({ "error": "project memory is disabled. Run /memory on to enable it." });
        }
        let args: Args = match serde_json::from_value(args) {
            Ok(args) => args,
            Err(e) => return json!({ "error": format!("invalid args: {e}") }),
        };
        let index = match load_project_index(&self.config) {
            Ok(Some(index)) => index,
            Ok(None) => {
                return json!({ "error": "project memory index not found. Run /index first." })
            }
            Err(e) => return json!({ "error": e.to_string() }),
        };
        let limit = args.limit.unwrap_or(8).clamp(1, 20);
        let mut hits = search_index(&index, &args.query, limit);
        add_snippets(Path::new(&index.workspace_root), &args.query, &mut hits);
        json!({
            "query": args.query,
            "count": hits.len(),
            "workspaceRoot": index.workspace_root,
            "hits": hits,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_memory::build_project_index;

    #[tokio::test]
    async fn searches_project_memory_index_with_snippets() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/commands.rs"),
            "pub fn dispatch() {\n    println!(\"slash command\");\n}\n",
        )
        .unwrap();
        let config = AgentConfig {
            workspace_root: dir.path().display().to_string(),
            session_dir: dir.path().join(".sessions").display().to_string(),
            ..Default::default()
        };
        build_project_index(&config).unwrap();

        let result = RepoSearchTool { config }
            .execute(json!({ "query": "dispatch command", "limit": 3 }))
            .await;

        assert_eq!(result["count"].as_u64(), Some(1));
        assert_eq!(result["hits"][0]["path"].as_str(), Some("src/commands.rs"));
        assert!(result["hits"][0]["snippet"]
            .as_str()
            .unwrap()
            .contains("dispatch"));
    }
}
