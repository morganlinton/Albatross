//! Stable, in-process API for embedding the Albatross agent.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::agent::{run_agent, RunResult};
use crate::backends::{backend, default_model, validate, BackendDescriptor};
use crate::config::{AgentConfig, ApprovalPolicy, ToolSelection};
use crate::openai::build_http_client;
use crate::skills::SkillRegistry;
use crate::tools::build_tools_for_names;

pub use crate::agent::{AgentEvent, ApprovalProvider};
pub use crate::backends::BackendName;
pub use crate::cancel::CancellationToken;
pub use crate::model_system::EffortLevel;
pub use crate::openai::{ChatMessage as Message, ImageUrl, UserContent, UserContentPart};
pub use crate::tools::{Tool, ToolPreview};
pub use async_trait::async_trait;

/// Built-in tool set used by a default SDK session.
pub const DEFAULT_TOOLS: &[&str] = &[
    "file_read",
    "grep",
    "list_dir",
    "file_edit",
    "file_write",
    "shell",
    "update_plan",
    "task",
];

/// Session-level events. Subscribe before calling [`AgentSession::prompt`] to
/// drive a custom terminal, desktop, web, or automation interface.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    TurnStarted { prompt: String },
    Agent(AgentEvent),
    TurnCompleted(TurnStats),
    TurnAborted(TurnStats),
    TurnFailed { error: String },
}

/// Usage and termination information for a completed turn.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnStats {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_input_tokens: u32,
    pub cache_creation_input_tokens: u32,
    pub reported_cost_usd: Option<f64>,
    pub actual_model: Option<String>,
    pub provider: Option<String>,
    pub hit_step_limit: bool,
    pub cancelled: bool,
}

/// Result returned after an agent turn reaches a natural stop or its step
/// limit.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnResult {
    pub response: String,
    pub stats: TurnStats,
}

/// Serializable in-memory conversation state. Provider-private reasoning
/// blocks are intentionally omitted by the underlying message format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub messages: Vec<Message>,
}

/// Metadata discovered eagerly for an Agent Skill. Full instructions remain
/// progressively loaded by the skill activation tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSkill {
    pub name: String,
    pub description: String,
    pub source: String,
    pub path: PathBuf,
}

/// A cloneable handle that can stop the currently streaming prompt from a
/// different task or UI callback.
#[derive(Debug, Clone, Default)]
pub struct AbortHandle {
    current: Arc<Mutex<Option<CancellationToken>>>,
}

impl AbortHandle {
    pub fn abort(&self) -> bool {
        let token = self.current.lock().ok().and_then(|guard| guard.clone());
        if let Some(token) = token {
            token.cancel();
            true
        } else {
            false
        }
    }

    pub fn is_running(&self) -> bool {
        self.current
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
    }
}

struct RejectApprovals;

#[async_trait::async_trait]
impl ApprovalProvider for RejectApprovals {
    async fn approve(
        &mut self,
        _name: &str,
        _args: &serde_json::Value,
        _preview: Option<&ToolPreview>,
    ) -> bool {
        false
    }
}

/// Builder for a self-contained, in-memory agent session.
pub struct AgentBuilder {
    workspace_root: PathBuf,
    backend: BackendName,
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    effort: Option<EffortLevel>,
    system_prompt: Option<String>,
    max_steps: usize,
    builtin_tools: Vec<String>,
    custom_tools: Vec<Arc<dyn Tool>>,
    approval: Box<dyn ApprovalProvider>,
    discover_skills: bool,
    snapshot: Option<SessionSnapshot>,
    event_capacity: usize,
}

impl Default for AgentBuilder {
    fn default() -> Self {
        Self {
            workspace_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            backend: BackendName::Ollama,
            base_url: None,
            api_key: None,
            model: None,
            effort: None,
            system_prompt: None,
            max_steps: 20,
            builtin_tools: DEFAULT_TOOLS.iter().map(|name| (*name).into()).collect(),
            custom_tools: Vec::new(),
            approval: Box::new(RejectApprovals),
            discover_skills: true,
            snapshot: None,
            event_capacity: 256,
        }
    }
}

impl AgentBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn workspace_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.workspace_root = path.into();
        self
    }

    pub fn backend(mut self, backend: BackendName) -> Self {
        self.backend = backend;
        self
    }

    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn effort(mut self, effort: EffortLevel) -> Self {
        self.effort = Some(effort);
        self
    }

    /// Replace the default system prompt. `{cwd}` and `{tools}` placeholders
    /// are expanded when the session is built.
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps;
        self
    }

    /// Replace the built-in tool allowlist. Custom tools are added separately
    /// with [`Self::custom_tool`].
    pub fn builtin_tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.builtin_tools = tools.into_iter().map(Into::into).collect();
        self
    }

    pub fn custom_tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.custom_tools.push(tool);
        self
    }

    /// Install the callback used for approval-gated built-in and custom tools.
    /// Sessions deny approval requests unless a provider is supplied.
    pub fn approval_provider(mut self, provider: impl ApprovalProvider + 'static) -> Self {
        self.approval = Box::new(provider);
        self
    }

    pub fn discover_skills(mut self, enabled: bool) -> Self {
        self.discover_skills = enabled;
        self
    }

    pub fn resume(mut self, snapshot: SessionSnapshot) -> Self {
        self.snapshot = Some(snapshot);
        self
    }

    pub fn event_capacity(mut self, capacity: usize) -> Self {
        self.event_capacity = capacity;
        self
    }

    pub fn build(self) -> Result<AgentSession> {
        if self.max_steps == 0 {
            bail!("max_steps must be greater than zero");
        }
        if self.event_capacity == 0 {
            bail!("event_capacity must be greater than zero");
        }
        if !self.workspace_root.is_dir() {
            bail!(
                "workspace root is not a directory: {}",
                self.workspace_root.display()
            );
        }

        let workspace_root = absolute_path(&self.workspace_root)?;
        let mut descriptor = backend(self.backend);
        if let Some(base_url) = self.base_url {
            descriptor.base_url = base_url;
        }
        if let Some(api_key) = self.api_key {
            descriptor.api_key = api_key;
        } else if descriptor.api_key.is_empty() {
            if let Some(api_key) = crate::auth::AuthStore::load().get(self.backend.as_str()) {
                descriptor.api_key = api_key.to_string();
            }
        }
        reqwest::Url::parse(&descriptor.base_url)
            .with_context(|| format!("invalid backend base URL: {}", descriptor.base_url))?;
        validate(&descriptor)?;
        let model = self
            .model
            .unwrap_or_else(|| default_model(&descriptor, None));
        if model.trim().is_empty() {
            bail!("model must not be empty");
        }

        let mut config = AgentConfig {
            workspace_root: workspace_root.display().to_string(),
            backend: self.backend,
            model_override: Some(model.clone()),
            max_steps: self.max_steps,
            approval_policy: ApprovalPolicy::Always,
            tool_selection: ToolSelection::Fixed,
            tools: self.builtin_tools.clone(),
            ..AgentConfig::default()
        };
        if let Some(prompt) = self.system_prompt {
            config.system_prompt = prompt;
        }
        validate_tools(&self.builtin_tools, &self.custom_tools)?;
        let mut diagnostics = Vec::new();
        if self.discover_skills {
            let resources = crate::packages::discover_installed();
            diagnostics.extend(resources.diagnostics.iter().cloned());
            config.skills = SkillRegistry::discover(&config.workspace_root, &resources.skills);
            diagnostics.extend(config.skills.diagnostics.iter().cloned());
            config.package_resources = resources;
        }
        let skills = config
            .skills
            .skills()
            .map(|skill| DiscoveredSkill {
                name: skill.name.clone(),
                description: skill.description.clone(),
                source: skill.source.clone(),
                path: skill.path.clone(),
            })
            .collect();

        let mut tool_names = self.builtin_tools;
        let mut tools = build_tools_for_names(&config, &tool_names, None);
        for tool in self.custom_tools {
            tool_names.push(tool.name().to_string());
            tools.push(tool);
        }
        if let Some(tool) = crate::skills::activation_tool(&config.skills) {
            if tool_names.iter().any(|name| name == tool.name()) {
                bail!("duplicate tool name: {}", tool.name());
            }
            tool_names.push(tool.name().to_string());
            tools.push(tool);
        }
        let system_prompt = config.render_system_prompt_for_tools(&tool_names);
        let messages = match self.snapshot {
            Some(snapshot) => validate_snapshot(snapshot, &system_prompt)?,
            None => vec![Message::System {
                content: system_prompt,
            }],
        };
        let (events, _) = broadcast::channel(self.event_capacity);

        Ok(AgentSession {
            http: build_http_client(),
            backend: descriptor,
            model,
            effort: self.effort,
            max_steps: self.max_steps,
            tools,
            tool_names,
            messages,
            approval: self.approval,
            events,
            abort: AbortHandle::default(),
            skills,
            diagnostics,
        })
    }
}

/// An in-memory Albatross conversation with a reusable model/tool context.
pub struct AgentSession {
    http: reqwest::Client,
    backend: BackendDescriptor,
    model: String,
    effort: Option<EffortLevel>,
    max_steps: usize,
    tools: Vec<Arc<dyn Tool>>,
    tool_names: Vec<String>,
    messages: Vec<Message>,
    approval: Box<dyn ApprovalProvider>,
    events: broadcast::Sender<SessionEvent>,
    abort: AbortHandle,
    skills: Vec<DiscoveredSkill>,
    diagnostics: Vec<String>,
}

impl AgentSession {
    pub fn builder() -> AgentBuilder {
        AgentBuilder::new()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.events.subscribe()
    }

    pub fn abort_handle(&self) -> AbortHandle {
        self.abort.clone()
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn backend(&self) -> BackendName {
        self.backend.name
    }

    pub fn tool_names(&self) -> &[String] {
        &self.tool_names
    }

    pub fn skills(&self) -> &[DiscoveredSkill] {
        &self.skills
    }

    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    pub fn set_model(&mut self, model: impl Into<String>) {
        self.model = model.into();
    }

    pub fn effort(&self) -> Option<EffortLevel> {
        self.effort
    }

    pub fn set_effort(&mut self, effort: Option<EffortLevel>) {
        self.effort = effort;
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            messages: self.messages.clone(),
        }
    }

    pub fn clear(&mut self) {
        self.messages.truncate(1);
    }

    pub async fn prompt(&mut self, prompt: impl Into<String>) -> Result<TurnResult> {
        self.prompt_content(UserContent::Text(prompt.into())).await
    }

    /// Run a turn with text or multipart text/image content.
    pub async fn prompt_content(&mut self, content: UserContent) -> Result<TurnResult> {
        let prompt_preview = content.as_text().into_owned();
        if prompt_preview.trim().is_empty() {
            bail!("prompt must not be empty");
        }
        let _ = self.events.send(SessionEvent::TurnStarted {
            prompt: prompt_preview,
        });

        let mut initial_messages = self.messages.clone();
        initial_messages.push(Message::User { content });
        let cancel = CancellationToken::new();
        if let Ok(mut current) = self.abort.current.lock() {
            *current = Some(cancel.clone());
        }
        let events = self.events.clone();
        let result = run_agent(
            &self.http,
            &self.backend,
            &self.model,
            self.effort,
            initial_messages,
            self.tools.clone(),
            self.max_steps,
            move |event| {
                let _ = events.send(SessionEvent::Agent(event));
            },
            Some(self.approval.as_mut()),
            Some(cancel),
            None,
            None,
            None,
            0,
            None,
        )
        .await;
        if let Ok(mut current) = self.abort.current.lock() {
            *current = None;
        }

        match result {
            Ok(result) => {
                self.messages = result.messages.clone();
                let turn = turn_result(result);
                let lifecycle = if turn.stats.cancelled {
                    SessionEvent::TurnAborted(turn.stats.clone())
                } else {
                    SessionEvent::TurnCompleted(turn.stats.clone())
                };
                let _ = self.events.send(lifecycle);
                Ok(turn)
            }
            Err(error) => {
                let _ = self.events.send(SessionEvent::TurnFailed {
                    error: error.to_string(),
                });
                Err(error)
            }
        }
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("resolving workspace root {}", path.display()))
}

fn validate_tools(builtin: &[String], custom: &[Arc<dyn Tool>]) -> Result<()> {
    let mut names = BTreeSet::new();
    for name in builtin {
        if !crate::config::is_tool_name(name) {
            bail!("unknown built-in tool: {name}");
        }
        if !names.insert(name.clone()) {
            bail!("duplicate tool name: {name}");
        }
    }
    for tool in custom {
        let name = tool.name().trim();
        if name.is_empty()
            || name.len() > 64
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        {
            bail!(
                "custom tool name must contain 1-64 ASCII letters, numbers, underscores, or hyphens"
            );
        }
        if !names.insert(name.to_string()) {
            bail!("duplicate tool name: {name}");
        }
        if !tool.input_schema().is_object() {
            bail!("custom tool `{name}` input schema must be a JSON object");
        }
    }
    Ok(())
}

fn validate_snapshot(snapshot: SessionSnapshot, system_prompt: &str) -> Result<Vec<Message>> {
    if snapshot.messages.is_empty() {
        bail!("session snapshot contains no system message");
    }
    if !matches!(snapshot.messages.first(), Some(Message::System { .. })) {
        bail!("session snapshot must start with a system message");
    }
    let mut messages = snapshot.messages;
    if let Some(Message::System { content }) = messages.first_mut() {
        *content = system_prompt.to_string();
    }
    crate::context_guard::validate_transcript(&messages)?;
    Ok(messages)
}

fn turn_result(result: RunResult) -> TurnResult {
    let response = result
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant {
                content: Some(content),
                ..
            } => Some(content.clone()),
            _ => None,
        })
        .unwrap_or_default();
    TurnResult {
        response,
        stats: TurnStats {
            input_tokens: result.input_tokens,
            output_tokens: result.output_tokens,
            cached_input_tokens: result.cached_input_tokens,
            cache_creation_input_tokens: result.cache_creation_input_tokens,
            reported_cost_usd: result.reported_cost_usd,
            actual_model: result.actual_model,
            provider: result.provider,
            hit_step_limit: result.hit_step_limit,
            cancelled: result.cancelled,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;
    use serde_json::{json, Value};

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }

        fn description(&self) -> &str {
            "Echo a value"
        }

        fn input_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"]
            })
        }

        async fn execute(&self, args: Value) -> Value {
            json!({"echoed": args["value"]})
        }
    }

    #[test]
    fn builder_rejects_unknown_and_duplicate_tools() {
        let unknown = AgentBuilder::new().builtin_tools(["nope"]).build();
        assert!(unknown
            .err()
            .unwrap()
            .to_string()
            .contains("unknown built-in"));
        let duplicate = AgentBuilder::new()
            .builtin_tools(["file_read", "file_read"])
            .build();
        assert!(duplicate.err().unwrap().to_string().contains("duplicate"));
    }

    #[test]
    fn snapshots_require_and_refresh_the_system_message() {
        let missing = SessionSnapshot {
            messages: vec![Message::User {
                content: "hello".into(),
            }],
        };
        assert!(AgentBuilder::new().resume(missing).build().is_err());

        let snapshot = SessionSnapshot {
            messages: vec![Message::System {
                content: "old".into(),
            }],
        };
        let session = AgentBuilder::new()
            .system_prompt("new")
            .resume(snapshot)
            .build()
            .unwrap();
        assert!(matches!(
            session.messages().first(),
            Some(Message::System { content }) if content == "new"
        ));
    }

    #[test]
    fn builder_uses_the_configured_workspace_in_the_system_prompt() {
        let temp = tempfile::tempdir().unwrap();
        let session = AgentBuilder::new()
            .workspace_root(temp.path())
            .builtin_tools(Vec::<String>::new())
            .discover_skills(false)
            .build()
            .unwrap();
        assert!(matches!(
            session.messages().first(),
            Some(Message::System { content }) if content.contains(&temp.path().display().to_string())
        ));
    }

    #[test]
    fn abort_handle_cancels_the_active_token() {
        let handle = AbortHandle::default();
        assert!(!handle.abort());
        let token = CancellationToken::new();
        *handle.current.lock().unwrap() = Some(token.clone());
        assert!(handle.is_running());
        assert!(handle.abort());
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn prompt_streams_events_and_retains_conversation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello from SDK.\"}}]}\n\n",
            "data: [DONE]\n\n"
        );
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut session = AgentBuilder::new()
            .backend(BackendName::Ollama)
            .base_url(format!("http://{address}/v1"))
            .model("mock")
            .builtin_tools(Vec::<String>::new())
            .discover_skills(false)
            .build()
            .unwrap();
        let mut events = session.subscribe();
        let turn = session.prompt("hello").await.unwrap();
        server.join().unwrap();

        assert_eq!(turn.response, "Hello from SDK.");
        assert_eq!(session.messages().len(), 3);
        assert!(matches!(
            events.try_recv().unwrap(),
            SessionEvent::TurnStarted { prompt } if prompt == "hello"
        ));
        assert!(matches!(
            events.try_recv().unwrap(),
            SessionEvent::Agent(AgentEvent::Text { delta }) if delta == "Hello from SDK."
        ));
        assert!(matches!(
            events.try_recv().unwrap(),
            SessionEvent::TurnCompleted(_)
        ));
    }

    #[tokio::test]
    async fn custom_tools_execute_inside_the_agent_loop() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let tool_call = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"echo\",\"arguments\":\"{\\\"value\\\":\\\"hello\\\"}\"}}]}}]}\n\n",
            "data: [DONE]\n\n"
        );
        let answer = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Tool finished.\"}}]}\n\n",
            "data: [DONE]\n\n"
        );
        let server = std::thread::spawn(move || {
            for body in [tool_call, answer] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 8192];
                let _ = stream.read(&mut request);
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });

        let mut session = AgentBuilder::new()
            .backend(BackendName::Ollama)
            .base_url(format!("http://{address}/v1"))
            .model("mock")
            .builtin_tools(Vec::<String>::new())
            .custom_tool(Arc::new(EchoTool))
            .discover_skills(false)
            .build()
            .unwrap();
        let mut events = session.subscribe();
        let turn = session.prompt("echo hello").await.unwrap();
        server.join().unwrap();

        assert_eq!(turn.response, "Tool finished.");
        let emitted = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert!(emitted.iter().any(|event| matches!(
            event,
            SessionEvent::Agent(AgentEvent::ToolResult { name, output, .. })
                if name == "echo" && output.contains("hello")
        )));
    }
}
