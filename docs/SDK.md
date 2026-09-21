# Embedding Albatross

The `albatross-cli` crate includes a Rust SDK for running the same agent loop in-process. It is intended for custom terminal, desktop, web, automation, and agent-orchestration interfaces that need typed state and events rather than a CLI subprocess.

## Add the library

Until the SDK lands in a tagged release, depend on the repository branch or commit:

```toml
[dependencies]
albatross-cli = { git = "https://github.com/morganlinton/Albatross" }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
anyhow = "1"
```

The Cargo package is named `albatross-cli`; Rust imports it as `albatross_cli`.

## Minimal session

```rust
use albatross_cli::sdk::{AgentBuilder, AgentEvent, BackendName, SessionEvent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut session = AgentBuilder::new()
        .backend(BackendName::OpenAi)
        .model("gpt-5-mini")
        .builtin_tools(["file_read", "grep", "list_dir"])
        .build()?;

    let mut events = session.subscribe();
    let printer = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                SessionEvent::Agent(AgentEvent::Text { delta }) => print!("{delta}"),
                SessionEvent::TurnCompleted(_)
                | SessionEvent::TurnAborted(_)
                | SessionEvent::TurnFailed { .. } => break,
                _ => {}
            }
        }
    });

    let result = session.prompt("Summarize this repository.").await?;
    printer.await?;
    println!("\n{} output tokens", result.stats.output_tokens);
    Ok(())
}
```

Provider credentials use the same environment variables and Albatross auth store as the CLI, without mutating the host process environment. Default base URLs also match the CLI. Use `.api_key(...)` or `.base_url(...)` when the embedding application owns those values directly.

## Session API

`AgentBuilder` configures:

- workspace, backend, base URL, API key, model, and reasoning effort
- system prompt and maximum tool-call steps
- built-in and custom tools
- approval handling
- Agent Skills discovery
- event buffer capacity and snapshot restoration

`AgentSession` provides:

- `prompt()` to run a turn and wait for completion
- `prompt_content()` for multipart text and image turns
- `subscribe()` for structured streaming and lifecycle events
- `messages()` and `snapshot()` for conversation state
- `skills()`, `tool_names()`, and `diagnostics()` for resolved runtime state
- `set_model()` and `set_effort()` for subsequent turns
- `clear()` to retain only the system prompt
- `abort_handle()` for cancellation from another task or UI callback

Sessions are in-memory by default. Persist a `SessionSnapshot` with any Serde-compatible format and restore it with `AgentBuilder::resume`. Snapshots intentionally omit provider-private reasoning blocks; they preserve the portable conversation and tool transcript.

## Approvals

SDK sessions deny approval-gated actions by default. Supply an approval provider when the host application has an explicit policy or user confirmation UI:

```rust
use albatross_cli::sdk::{async_trait, ApprovalProvider, ToolPreview};
use serde_json::Value;

struct HostApprovals;

#[async_trait]
impl ApprovalProvider for HostApprovals {
    async fn approve(
        &mut self,
        name: &str,
        args: &Value,
        preview: Option<&ToolPreview>,
    ) -> bool {
        host_ui_confirm(name, args, preview).await
    }
}
```

Install it with `.approval_provider(HostApprovals)`. The SDK never silently turns approval-required tools into allow-all operations.

## Custom tools

Implement `Tool` and add an `Arc` with `.custom_tool(...)`:

```rust
use std::sync::Arc;
use albatross_cli::sdk::{async_trait, Tool};
use serde_json::{json, Value};

struct Lookup;

#[async_trait]
impl Tool for Lookup {
    fn name(&self) -> &str { "lookup" }
    fn description(&self) -> &str { "Look up a value in the host application" }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "key": { "type": "string" } },
            "required": ["key"]
        })
    }
    async fn execute(&self, args: Value) -> Value {
        json!({ "value": host_lookup(&args["key"]).await })
    }
}

let session = AgentBuilder::new().custom_tool(Arc::new(Lookup)).build()?;
```

Tool names must be unique across built-in tools, custom tools, and the dynamically discovered Agent Skills activation tool.

## Events and backpressure

Each subscriber receives `TurnStarted`, all low-level `AgentEvent` values, then `TurnCompleted`, `TurnAborted`, or `TurnFailed`. Subscriptions use a bounded Tokio broadcast channel, so a slow consumer receives `RecvError::Lagged` instead of blocking model streaming. Increase `.event_capacity(...)` or handle lag explicitly for high-volume UIs.

## Current boundary

The first SDK release intentionally focuses on the reusable in-process session core. CLI presentation, session-tree navigation, live extension/MCP reload, routing menus, and terminal history remain CLI concerns. Applications that need process isolation or a language-neutral boundary should continue to invoke the CLI; a dedicated JSON-RPC mode is a separate follow-up.

See [`examples/sdk_minimal.rs`](../examples/sdk_minimal.rs) for a compilable example.

## Jev-first routing

Use `AgentBuilder::jev(JevConfig)` to opt into bounded direct answers before the
main model. `TurnStats::jev` reports routing usage separately from main-model
tokens. See [Jev Mode](JEV_HARNESS.md) for examples, thresholds, and data handling.
