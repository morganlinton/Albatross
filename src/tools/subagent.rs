use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Instant;

use super::{build_tools_for_names, Tool, ToolRuntimeContext};
use crate::agent::{run_agent, AgentEvent};
use crate::backends::BackendDescriptor;
use crate::cancel::CancellationToken;
use crate::config::AgentConfig;
use crate::openai::ChatMessage;
use crate::turn_trace::TracePayload;

/// Hard cap on a subagent's own loop, regardless of the parent's max_steps.
/// Keeps a delegated investigation bounded and cheap.
const SUBAGENT_MAX_STEPS: usize = 12;
const SUBAGENT_DEPTH: u32 = 1;

const SUBAGENT_SYSTEM_PROMPT: &str = concat!(
    "You are a focused investigation subagent. The parent agent has delegated a\n",
    "single, well-scoped question to you to keep its own context small.\n",
    "\n",
    "You have READ-ONLY tools (read files, grep, list, glob, search). You cannot\n",
    "edit files or run shell commands — do not try.\n",
    "\n",
    "Investigate efficiently, then STOP and return a concise answer in plain text:\n",
    "the findings the parent needs, with concrete file paths and line numbers where\n",
    "relevant. Do not pad. Do not ask follow-up questions.\n",
    "\n",
    "Current working directory: {cwd}",
);

/// A subagent that runs a fresh, read-only agent loop on a delegated question
/// and returns only its final answer. This keeps the parent conversation small:
/// dozens of exploratory file reads happen inside the subagent and never enter
/// the parent's context — only the summary comes back.
pub struct SubagentTool {
    pub http: reqwest::Client,
    pub backend: BackendDescriptor,
    pub model: String,
    pub config: AgentConfig,
    pub runtime: Option<ToolRuntimeContext>,
}

#[derive(Deserialize)]
struct Args {
    task: String,
    #[serde(default)]
    context: Option<String>,
}

impl SubagentTool {
    /// Read-only tool names the subagent is allowed to use. Never includes the
    /// subagent tool itself (no recursion) or any mutating/shell tool.
    fn subagent_tool_names(&self) -> Vec<String> {
        let mut names = vec![
            "file_read".to_string(),
            "grep".to_string(),
            "list_dir".to_string(),
            "glob".to_string(),
        ];
        if self.config.project_memory.enabled {
            names.push("repo_search".to_string());
        }
        names
    }

    async fn run(&self, args: Args, cancel: Option<CancellationToken>) -> Value {
        let system = SUBAGENT_SYSTEM_PROMPT.replace("{cwd}", &self.config.workspace_root);
        let mut user = args.task.trim().to_string();
        if let Some(ctx) = args.context.as_ref().filter(|c| !c.trim().is_empty()) {
            user.push_str("\n\nContext from the parent agent:\n");
            user.push_str(ctx.trim());
        }
        let initial = vec![
            ChatMessage::System { content: system },
            ChatMessage::User {
                content: user.into(),
            },
        ];

        let tools = build_tools_for_names(&self.config, &self.subagent_tool_names(), None);
        let trace = self.runtime.as_ref().map(|r| r.trace.clone());
        let trace_enabled = self
            .runtime
            .as_ref()
            .map(|r| r.trace_enabled)
            .unwrap_or(false);
        let event_tx = self.runtime.as_ref().and_then(|r| r.agent_events.clone());
        let hooks = self.runtime.as_ref().and_then(|r| r.hooks.clone());
        let call_id = format!(
            "subagent-{}",
            args.task.chars().take(24).collect::<String>()
        );

        if let Some(trace) = &trace {
            if let Ok(guard) = trace.lock() {
                let _ = guard.append(TracePayload::SubagentStart {
                    call_id: call_id.clone(),
                    task: args.task.trim().to_string(),
                });
            }
        }

        let start = Instant::now();
        let result = run_agent(
            &self.http,
            &self.backend,
            &self.model,
            None,
            initial,
            tools,
            SUBAGENT_MAX_STEPS,
            |event| {
                if trace_enabled {
                    if let Some(tx) = &event_tx {
                        let _ = tx.send(forward_subagent_event(event));
                    }
                }
            },
            None, // no approval provider => any mutating tool would be denied
            cancel,
            None, // no context guard for a short subagent
            None, // no checkpoint capturer
            trace.clone(),
            SUBAGENT_DEPTH,
            hooks,
        )
        .await;

        if let Some(trace) = &trace {
            if let Ok(guard) = trace.lock() {
                let (input_tokens, output_tokens) = result
                    .as_ref()
                    .map(|r| (r.input_tokens, r.output_tokens))
                    .unwrap_or((0, 0));
                let _ = guard.append(TracePayload::SubagentEnd {
                    call_id,
                    input_tokens,
                    output_tokens,
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
        }

        match result {
            Ok(run) => {
                let summary = run
                    .messages
                    .iter()
                    .rev()
                    .find_map(|m| match m {
                        ChatMessage::Assistant {
                            content: Some(c), ..
                        } if !c.trim().is_empty() => Some(c.trim().to_string()),
                        _ => None,
                    })
                    .unwrap_or_else(|| {
                        "Subagent finished without producing a summary.".to_string()
                    });
                json!({
                    "summary": summary,
                    "input_tokens": run.input_tokens,
                    "output_tokens": run.output_tokens
                })
            }
            Err(e) => json!({ "error": format!("subagent failed: {e}") }),
        }
    }
}

pub(crate) fn forward_subagent_event(event: AgentEvent) -> AgentEvent {
    match event {
        AgentEvent::ToolCall {
            name,
            call_id,
            args,
            depth,
        } => AgentEvent::ToolCall {
            name,
            call_id,
            args,
            depth: depth.max(SUBAGENT_DEPTH),
        },
        AgentEvent::ToolResult {
            name,
            call_id,
            output,
            depth,
        } => AgentEvent::ToolResult {
            name,
            call_id,
            output,
            depth: depth.max(SUBAGENT_DEPTH),
        },
        AgentEvent::ToolOutputCompacted {
            name,
            call_id,
            summary,
            depth,
        } => AgentEvent::ToolOutputCompacted {
            name,
            call_id,
            summary,
            depth: depth.max(SUBAGENT_DEPTH),
        },
        other => other,
    }
}

#[async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &str {
        "task"
    }
    fn description(&self) -> &str {
        "Delegate a self-contained, read-only investigation to a subagent and get back only its conclusion. Use this when answering needs reading many files (\"where is X handled?\", \"how does Y flow through the code?\") so the exploration stays out of your context. The subagent cannot edit files or run commands. Give it one clear question."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The single, well-scoped question or investigation for the subagent."
                },
                "context": {
                    "type": "string",
                    "description": "Optional extra context (paths already known, constraints) to seed the subagent."
                }
            },
            "required": ["task"]
        })
    }
    async fn execute(&self, args: Value) -> Value {
        self.execute_cancelable(args, None).await
    }
    async fn execute_cancelable(&self, args: Value, cancel: Option<CancellationToken>) -> Value {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return json!({ "error": format!("invalid args: {e}") }),
        };
        if args.task.trim().is_empty() {
            return json!({ "error": "task must not be empty" });
        }
        self.run(args, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> SubagentTool {
        SubagentTool {
            http: reqwest::Client::new(),
            backend: crate::backends::backend(crate::backends::BackendName::Ollama),
            model: "test".into(),
            config: AgentConfig::default(),
            runtime: None,
        }
    }

    #[test]
    fn subagent_tools_are_read_only_and_exclude_self() {
        let names = tool().subagent_tool_names();
        assert!(names.contains(&"file_read".to_string()));
        assert!(names.contains(&"grep".to_string()));
        assert!(!names.contains(&"task".to_string()));
        assert!(!names.contains(&"shell".to_string()));
        assert!(!names.contains(&"file_edit".to_string()));
    }

    #[tokio::test]
    async fn empty_task_is_an_error() {
        let out = tool().execute(json!({ "task": "   " })).await;
        assert!(out.get("error").is_some());
    }
}
