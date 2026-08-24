//! General-purpose out-of-process extensions.
//!
//! Extensions are trusted executables that speak line-delimited JSON-RPC 2.0
//! over stdin/stdout. They can register model-callable tools, interactive
//! slash commands, and subscriptions to Albatross lifecycle events without
//! being linked into the Rust binary.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};

use crate::tools::{Tool, ToolPreview};

pub const EXTENSION_PROTOCOL_VERSION: &str = "1";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(10);

type PendingMap = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value>>>>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Optional package-local process directory. Hand-written project config
    /// defaults to the workspace root for backward compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
}

fn default_enabled() -> bool {
    true
}

impl Default for ExtensionConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
            enabled: true,
            working_directory: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExtensionTrustFile {
    #[serde(default)]
    workspaces: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionTrustStatus {
    Trusted,
    Modified,
    Untrusted,
    Disabled,
}

#[derive(Debug, Clone)]
pub struct ExtensionTrust {
    pub name: String,
    pub hash: String,
    pub status: ExtensionTrustStatus,
}

fn trust_file_path() -> Option<PathBuf> {
    crate::auth::config_dir().map(|dir| dir.join("extensions-trust.json"))
}

fn workspace_key(workspace_root: &str) -> String {
    crate::path_security::canonical_root(Path::new(workspace_root))
        .display()
        .to_string()
}

fn config_hash(config: &ExtensionConfig) -> String {
    let bytes = serde_json::to_vec(config).expect("extension configuration serializes");
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2 + 7);
    out.push_str("sha256:");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn load_trust_file() -> ExtensionTrustFile {
    let Some(path) = trust_file_path() else {
        return ExtensionTrustFile::default();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_trust_file(file: &ExtensionTrustFile) -> Result<()> {
    let path = trust_file_path()
        .ok_or_else(|| anyhow!("HOME or XDG_CONFIG_HOME is required to save extension trust"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(file)? + "\n")
        .with_context(|| format!("writing {}", path.display()))
}

pub fn configured_trust(
    extensions: &BTreeMap<String, ExtensionConfig>,
    workspace_root: &str,
) -> Vec<ExtensionTrust> {
    configured_trust_from(extensions, workspace_root, &load_trust_file())
}

fn configured_trust_from(
    extensions: &BTreeMap<String, ExtensionConfig>,
    workspace_root: &str,
    trust: &ExtensionTrustFile,
) -> Vec<ExtensionTrust> {
    let saved = trust.workspaces.get(&workspace_key(workspace_root));
    extensions
        .iter()
        .map(|(name, config)| {
            let hash = config_hash(config);
            let status = if !config.enabled {
                ExtensionTrustStatus::Disabled
            } else {
                match saved.and_then(|entries| entries.get(name)) {
                    Some(saved_hash) if saved_hash == &hash => ExtensionTrustStatus::Trusted,
                    Some(_) => ExtensionTrustStatus::Modified,
                    None => ExtensionTrustStatus::Untrusted,
                }
            };
            ExtensionTrust {
                name: name.clone(),
                hash,
                status,
            }
        })
        .collect()
}

pub fn trusted_configured(
    extensions: &BTreeMap<String, ExtensionConfig>,
    workspace_root: &str,
) -> (BTreeMap<String, ExtensionConfig>, Vec<ExtensionTrust>) {
    let discovery = configured_trust(extensions, workspace_root);
    let trusted = discovery
        .iter()
        .filter(|entry| entry.status == ExtensionTrustStatus::Trusted)
        .filter_map(|entry| {
            extensions
                .get(&entry.name)
                .cloned()
                .map(|config| (entry.name.clone(), config))
        })
        .collect();
    (trusted, discovery)
}

pub fn trust_extension(workspace_root: &str, name: &str, config: &ExtensionConfig) -> Result<()> {
    let mut trust = load_trust_file();
    trust
        .workspaces
        .entry(workspace_key(workspace_root))
        .or_default()
        .insert(name.to_string(), config_hash(config));
    save_trust_file(&trust)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializeResult {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    tools: Vec<ExtensionToolDefinition>,
    #[serde(default)]
    commands: Vec<ExtensionCommandDefinition>,
    #[serde(default)]
    events: BTreeSet<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExtensionToolDefinition {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_input_schema")]
    input_schema: Value,
    #[serde(default = "default_requires_approval")]
    requires_approval: bool,
}

fn default_input_schema() -> Value {
    json!({"type": "object"})
}

fn default_requires_approval() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
struct ExtensionCommandDefinition {
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionCommandResult {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
}

struct ExtensionClient {
    config_name: String,
    next_id: std::sync::atomic::AtomicI64,
    pending: PendingMap,
    stdin: Mutex<ChildStdin>,
    _child: Child,
}

impl ExtensionClient {
    async fn spawn(
        config_name: &str,
        config: &ExtensionConfig,
        workspace_root: &str,
    ) -> Result<(Arc<Self>, InitializeResult)> {
        let mut command = Command::new(&config.command);
        apply_extension_env(&mut command, config);
        command.args(&config.args);
        command
            .current_dir(
                config
                    .working_directory
                    .as_deref()
                    .unwrap_or(workspace_root),
            )
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .with_context(|| format!("spawning extension `{config_name}` ({})", config.command))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("extension `{config_name}` exposed no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("extension `{config_name}` exposed no stdout"))?;

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let pending_for_reader = pending.clone();
        let reader_name = config_name.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    handle_incoming(&reader_name, &line, &pending_for_reader).await;
                }
            }
            let mut guard = pending_for_reader.lock().await;
            for (_, sender) in guard.drain() {
                let _ = sender.send(Err(anyhow!(
                    "extension `{reader_name}` closed before responding"
                )));
            }
        });

        let client = Arc::new(Self {
            config_name: config_name.to_string(),
            next_id: std::sync::atomic::AtomicI64::new(1),
            pending,
            stdin: Mutex::new(stdin),
            _child: child,
        });
        let response = client
            .request_with_timeout(
                "initialize",
                json!({
                    "protocolVersion": EXTENSION_PROTOCOL_VERSION,
                    "clientInfo": {
                        "name": "albatross",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "workspaceRoot": workspace_root,
                }),
                INITIALIZE_TIMEOUT,
            )
            .await
            .with_context(|| format!("initializing extension `{config_name}`"))?;
        let result: InitializeResult = serde_json::from_value(response)
            .with_context(|| format!("invalid initialize result from `{config_name}`"))?;
        Ok((client, result))
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_with_timeout(method, params, REQUEST_TIMEOUT)
            .await
    }

    async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(id, sender);
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(error) = self.write_frame(&frame).await {
            self.pending.lock().await.remove(&id);
            return Err(error);
        }
        match tokio::time::timeout(timeout, receiver).await {
            Ok(response) => response.context("extension response channel dropped")?,
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(anyhow!(
                    "extension `{}` request `{method}` timed out after {}s",
                    self.config_name,
                    timeout.as_secs()
                ))
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.write_frame(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    async fn write_frame(&self, frame: &Value) -> Result<()> {
        let mut line = serde_json::to_string(frame)?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .with_context(|| format!("writing to extension `{}`", self.config_name))?;
        stdin
            .flush()
            .await
            .with_context(|| format!("flushing extension `{}`", self.config_name))
    }
}

async fn handle_incoming(name: &str, line: &str, pending: &PendingMap) {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let Some(id) = value.get("id").and_then(Value::as_i64) else {
        return;
    };
    let Some(sender) = pending.lock().await.remove(&id) else {
        return;
    };
    if let Some(error) = value.get("error") {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("(no message)");
        let _ = sender.send(Err(anyhow!(
            "extension `{name}` returned error {code}: {message}"
        )));
    } else {
        let _ = sender.send(Ok(value.get("result").cloned().unwrap_or(Value::Null)));
    }
}

#[cfg(not(windows))]
fn inherited_env_allowlist() -> &'static [&'static str] {
    &["PATH", "HOME", "TMPDIR", "LANG", "LC_ALL"]
}

#[cfg(windows)]
fn inherited_env_allowlist() -> &'static [&'static str] {
    &[
        "PATH",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        "SYSTEMROOT",
        "TEMP",
        "TMP",
    ]
}

fn apply_extension_env(command: &mut Command, config: &ExtensionConfig) {
    command.env_clear();
    for key in inherited_env_allowlist() {
        if let Ok(value) = std::env::var(key) {
            command.env(key, value);
        }
    }
    for (key, value) in &config.env {
        command.env(key, value);
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn command_name(value: &str) -> Option<String> {
    let bare = value.strip_prefix('/').unwrap_or(value);
    valid_identifier(bare).then(|| format!("/{bare}"))
}

struct ExtensionTool {
    display_name: String,
    remote_name: String,
    description: String,
    schema: Value,
    requires_approval: bool,
    client: Arc<ExtensionClient>,
}

#[async_trait]
impl Tool for ExtensionTool {
    fn name(&self) -> &str {
        &self.display_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        self.schema.clone()
    }

    fn require_approval(&self, _args: &Value) -> bool {
        self.requires_approval
    }

    async fn preview(&self, _args: &Value) -> Option<ToolPreview> {
        self.requires_approval.then(|| ToolPreview {
            summary: format!("invoke extension tool {}", self.display_name),
            diff: None,
            risk: Some("trusted extension process may have external side effects".into()),
        })
    }

    async fn execute(&self, arguments: Value) -> Value {
        match self
            .client
            .request(
                "tools/call",
                json!({"name": self.remote_name, "arguments": arguments}),
            )
            .await
        {
            Ok(result) => result,
            Err(error) => json!({"error": format!("extension tool failed: {error}")}),
        }
    }
}

#[derive(Clone)]
struct ExtensionCommand {
    name: String,
    description: String,
    client: Arc<ExtensionClient>,
}

#[derive(Debug, Clone)]
pub struct LoadedExtension {
    pub config_name: String,
    pub name: String,
    pub version: Option<String>,
    pub tool_count: usize,
    pub command_count: usize,
    pub events: BTreeSet<String>,
}

#[derive(Default)]
pub struct ExtensionRegistry {
    clients: Vec<(Arc<ExtensionClient>, BTreeSet<String>)>,
    tools: Vec<Arc<dyn Tool>>,
    commands: BTreeMap<String, ExtensionCommand>,
    loaded: Vec<LoadedExtension>,
}

#[derive(Clone, Default)]
pub struct ExtensionEventDispatcher {
    clients: Vec<(Arc<ExtensionClient>, BTreeSet<String>)>,
}

impl ExtensionEventDispatcher {
    pub async fn emit(&self, event: &str, payload: Value) -> Vec<String> {
        let mut errors = Vec::new();
        for (client, subscriptions) in &self.clients {
            if subscriptions.contains(event) || subscriptions.contains("*") {
                if let Err(error) = client
                    .notify(
                        "events/emit",
                        json!({"event": event, "payload": payload.clone()}),
                    )
                    .await
                {
                    errors.push(error.to_string());
                }
            }
        }
        errors
    }
}

impl ExtensionRegistry {
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect()
    }

    pub fn command_list(&self) -> Vec<(String, String)> {
        self.commands
            .values()
            .map(|command| (command.name.clone(), command.description.clone()))
            .collect()
    }

    pub fn loaded(&self) -> &[LoadedExtension] {
        &self.loaded
    }

    pub fn event_dispatcher(&self) -> ExtensionEventDispatcher {
        ExtensionEventDispatcher {
            clients: self.clients.clone(),
        }
    }

    pub fn has_command(&self, name: &str) -> bool {
        self.commands.contains_key(name)
    }

    pub async fn execute_command(
        &self,
        name: &str,
        arguments: &str,
    ) -> Result<ExtensionCommandResult> {
        let command = self
            .commands
            .get(name)
            .ok_or_else(|| anyhow!("unknown extension command: {name}"))?;
        let result = command
            .client
            .request(
                "commands/execute",
                json!({"name": name.trim_start_matches('/'), "arguments": arguments}),
            )
            .await?;
        serde_json::from_value(result).context("invalid extension command result")
    }

    pub async fn emit(&self, event: &str, payload: Value) -> Vec<String> {
        self.event_dispatcher().emit(event, payload).await
    }

    pub fn absorb(&mut self, other: ExtensionRegistry) -> Vec<String> {
        let mut errors = Vec::new();
        let existing_tools = self
            .tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect::<BTreeSet<_>>();
        for tool in other.tools {
            if existing_tools.contains(tool.name()) {
                errors.push(format!(
                    "duplicate extension tool `{}` skipped",
                    tool.name()
                ));
            } else {
                self.tools.push(tool);
            }
        }
        for (name, command) in other.commands {
            match self.commands.entry(name) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(command);
                }
                std::collections::btree_map::Entry::Occupied(entry) => {
                    errors.push(format!(
                        "duplicate extension command `{}` skipped",
                        entry.key()
                    ));
                }
            }
        }
        self.clients.extend(other.clients);
        self.loaded.extend(other.loaded);
        errors
    }
}

pub async fn spawn_configured(
    extensions: &BTreeMap<String, ExtensionConfig>,
    workspace_root: &str,
    reserved_commands: &BTreeSet<String>,
) -> (ExtensionRegistry, Vec<String>) {
    let mut registry = ExtensionRegistry::default();
    let mut errors = Vec::new();
    let mut tool_names = BTreeSet::new();

    for (config_name, config) in extensions {
        if !valid_identifier(config_name) {
            errors.push(format!(
                "{config_name}: configuration name must contain only letters, digits, '-' or '_'"
            ));
            continue;
        }
        match ExtensionClient::spawn(config_name, config, workspace_root).await {
            Ok((client, manifest)) => {
                let display_name = manifest.name.clone().unwrap_or_else(|| config_name.clone());
                let mut loaded_tools = 0usize;
                let mut loaded_commands = 0usize;
                for tool in manifest.tools {
                    if !valid_identifier(&tool.name) {
                        errors.push(format!(
                            "{config_name}: invalid tool name `{}` skipped",
                            tool.name
                        ));
                        continue;
                    }
                    let full_name = format!("ext__{config_name}__{}", tool.name);
                    if !tool_names.insert(full_name.clone()) {
                        errors.push(format!(
                            "{config_name}: duplicate tool `{full_name}` skipped"
                        ));
                        continue;
                    }
                    registry.tools.push(Arc::new(ExtensionTool {
                        display_name: full_name,
                        remote_name: tool.name,
                        description: tool.description,
                        schema: tool.input_schema,
                        requires_approval: tool.requires_approval,
                        client: client.clone(),
                    }));
                    loaded_tools += 1;
                }
                for command in manifest.commands {
                    let Some(name) = command_name(&command.name) else {
                        errors.push(format!(
                            "{config_name}: invalid command name `{}` skipped",
                            command.name
                        ));
                        continue;
                    };
                    if reserved_commands.contains(&name) || registry.commands.contains_key(&name) {
                        errors.push(format!(
                            "{config_name}: command `{name}` collides with an existing command and was skipped"
                        ));
                        continue;
                    }
                    registry.commands.insert(
                        name.clone(),
                        ExtensionCommand {
                            name,
                            description: command.description,
                            client: client.clone(),
                        },
                    );
                    loaded_commands += 1;
                }
                registry.clients.push((client, manifest.events.clone()));
                registry.loaded.push(LoadedExtension {
                    config_name: config_name.clone(),
                    name: display_name,
                    version: manifest.version,
                    tool_count: loaded_tools,
                    command_count: loaded_commands,
                    events: manifest.events,
                });
            }
            Err(error) => errors.push(format!("{config_name}: {error}")),
        }
    }
    (registry, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(command: &str) -> ExtensionConfig {
        ExtensionConfig {
            command: command.into(),
            ..Default::default()
        }
    }

    #[test]
    fn trust_changes_when_configuration_changes() {
        let mut entries = BTreeMap::new();
        entries.insert("demo".into(), config("first"));
        let mut trust = ExtensionTrustFile::default();
        trust
            .workspaces
            .entry(workspace_key("/tmp/work"))
            .or_default()
            .insert("demo".into(), config_hash(entries.get("demo").unwrap()));
        assert_eq!(
            configured_trust_from(&entries, "/tmp/work", &trust)[0].status,
            ExtensionTrustStatus::Trusted
        );
        entries.get_mut("demo").unwrap().command = "second".into();
        assert_eq!(
            configured_trust_from(&entries, "/tmp/work", &trust)[0].status,
            ExtensionTrustStatus::Modified
        );
    }

    #[test]
    fn disabled_extensions_do_not_count_as_untrusted() {
        let mut entries = BTreeMap::new();
        let mut disabled = config("demo");
        disabled.enabled = false;
        entries.insert("demo".into(), disabled);
        assert_eq!(
            configured_trust_from(&entries, "/tmp/work", &ExtensionTrustFile::default())[0].status,
            ExtensionTrustStatus::Disabled
        );
    }

    #[test]
    fn names_are_strict_and_commands_are_normalized() {
        assert!(valid_identifier("release-tools_2"));
        assert!(!valid_identifier("release tools"));
        assert_eq!(command_name("deploy").as_deref(), Some("/deploy"));
        assert_eq!(command_name("/deploy").as_deref(), Some("/deploy"));
        assert!(command_name("bad/name").is_none());
    }

    #[test]
    fn approval_defaults_to_true() {
        let definition: ExtensionToolDefinition = serde_json::from_value(json!({
            "name": "deploy"
        }))
        .unwrap();
        assert!(definition.requires_approval);
        assert_eq!(definition.input_schema, json!({"type": "object"}));
    }

    #[test]
    fn minimal_configuration_defaults_to_enabled() {
        let config: ExtensionConfig = serde_json::from_value(json!({
            "command": "demo-extension"
        }))
        .unwrap();
        assert!(config.enabled);
        assert!(config.args.is_empty());
        assert!(config.env.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn extension_process_registers_and_executes_tools_and_commands() {
        let script = r#"
seen_event=0
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"name":"Demo","version":"0.1.0","tools":[{"name":"echo","description":"Echo input","inputSchema":{"type":"object"},"requiresApproval":false}],"commands":[{"name":"hello","description":"Say hello"}],"events":["session_start"]}}'
      ;;
    *'"method":"tools/call"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"echoed":true}}'
      ;;
    *'"method":"commands/execute"'*)
      if [ "$seen_event" -eq 1 ]; then
        printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"message":"event observed","prompt":"inspect the repo"}}'
      else
        printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"message":"event missing","prompt":"inspect the repo"}}'
      fi
      ;;
    *'"method":"events/emit"'*)
      seen_event=1
      ;;
  esac
done
"#;
        let mut configs = BTreeMap::new();
        configs.insert(
            "demo".into(),
            ExtensionConfig {
                command: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                ..Default::default()
            },
        );
        let temp = tempfile::tempdir().unwrap();
        let (registry, errors) =
            spawn_configured(&configs, temp.path().to_str().unwrap(), &BTreeSet::new()).await;
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(registry.loaded().len(), 1);
        assert_eq!(registry.tool_names(), vec!["ext__demo__echo"]);
        assert!(registry.has_command("/hello"));

        let tool_result = registry.tools()[0].execute(json!({"text": "hi"})).await;
        assert_eq!(tool_result, json!({"echoed": true}));
        assert!(registry
            .emit("session_start", json!({"session": "test"}))
            .await
            .is_empty());
        let command_result = registry.execute_command("/hello", "world").await.unwrap();
        assert_eq!(command_result.message.as_deref(), Some("event observed"));
        assert_eq!(command_result.prompt.as_deref(), Some("inspect the repo"));
    }
}
