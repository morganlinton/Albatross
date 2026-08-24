# Albatross Extensions

Albatross extensions are trusted executable programs that add model tools,
interactive slash commands, and lifecycle event listeners without being linked
into the Albatross binary. The protocol is language-neutral JSON-RPC 2.0 over
newline-delimited stdin/stdout.

Extensions run with the user's operating-system permissions. Review an
extension before trusting it. Albatross hashes each extension's command,
arguments, environment, and enabled state; any change revokes trust.

## Configure and trust an extension

Add an `extensions` map to `agent.config.json`:

```json
{
  "extensions": {
    "hello": {
      "command": "python3",
      "args": ["examples/extensions/hello.py"],
      "env": {},
      "enabled": true
    }
  }
}
```

Extension configuration is project-local. A new or changed extension is not
started until it is trusted:

```text
/extensions
/extensions trust hello
```

Use `/extensions trust-all` to trust and start every enabled extension in the
current configuration. Trust is stored per canonical workspace and
configuration hash in `~/.config/albatross/extensions-trust.json` (or the
platform/XDG equivalent).

Extension child processes start with a cleared environment. Albatross passes a
small platform environment allowlist plus only the literal values in the
extension's `env` object. Provider credentials are not inherited unless they
are explicitly placed in `env`.

Trusted extension tools are available in interactive sessions and `--print`
mode. Extension slash commands are interactive-only.

## Transport

Every request, response, and notification is one JSON object followed by `\n`.
Extensions must reserve stdout for protocol frames. Diagnostics should go to
stderr, although Albatross currently suppresses extension stderr during normal
operation.

Requests and responses use JSON-RPC 2.0:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
{"jsonrpc":"2.0","id":1,"result":{}}
```

The current Albatross extension protocol version is `1`. Requests time out
after 30 seconds; initialization has a 10-second timeout. Extension processes
are terminated when their session registry is dropped.

## Initialize

Albatross's first request is `initialize`:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "initialize",
  "params": {
    "protocolVersion": "1",
    "clientInfo": { "name": "albatross", "version": "2.3.0" },
    "workspaceRoot": "/absolute/project/path"
  }
}
```

The extension returns its registrations:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "name": "Hello extension",
    "version": "0.1.0",
    "tools": [
      {
        "name": "greet",
        "description": "Return a greeting for a person",
        "inputSchema": {
          "type": "object",
          "properties": { "name": { "type": "string" } },
          "required": ["name"]
        },
        "requiresApproval": false
      }
    ],
    "commands": [
      { "name": "hello", "description": "Ask the agent to greet someone" }
    ],
    "events": ["session_start", "user_prompt_submit", "stop"]
  }
}
```

Names may contain ASCII letters, digits, `-`, and `_`, with a maximum length of
64 characters. Model tools are namespaced as
`ext__<configuration-name>__<tool-name>`. Slash commands are presented as
`/<command-name>` and cannot replace built-in or previously registered
commands.

`requiresApproval` defaults to `true`. This is a user-interaction guard, not a
sandbox: the trusted extension process is already running with the user's OS
permissions.

## Tool calls

When the model invokes an extension tool, Albatross sends:

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "tools/call",
  "params": {
    "name": "greet",
    "arguments": { "name": "Morgan" }
  }
}
```

The `result` may be any JSON value and is returned directly to the model:

```json
{"jsonrpc":"2.0","id":2,"result":{"greeting":"Hello, Morgan!"}}
```

Return a JSON-RPC `error` response for protocol or execution failures.

## Slash commands

For `/hello Morgan`, Albatross sends:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "commands/execute",
  "params": { "name": "hello", "arguments": "Morgan" }
}
```

The command may show a message, submit a prompt to the current agent session,
or do both:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "message": "Preparing a greeting…",
    "prompt": "Use the greet tool for Morgan"
  }
}
```

An omitted or empty `prompt` does not start an agent turn. Command arguments are
passed as one unparsed string so each extension can define its own syntax.

## Events

An extension subscribes using the event keys returned from `initialize`. Use
`"*"` to receive every event. Notifications have no request id and require no
response:

```json
{
  "jsonrpc": "2.0",
  "method": "events/emit",
  "params": {
    "event": "user_prompt_submit",
    "payload": {
      "hook_event_name": "UserPromptSubmit",
      "session_id": "2026-08-24T12-00-00Z",
      "prompt": "Review this code"
    }
  }
}
```

Supported event keys mirror Albatross hooks:

- `session_start`
- `user_prompt_submit`
- `pre_tool_use`
- `permission_request`
- `post_tool_use`
- `pre_compact`
- `post_compact`
- `plan_updated`
- `subagent_start`
- `subagent_stop`
- `stop`
- `session_end`

Event subscriptions are observational in protocol v1. They cannot block or
rewrite operations; use trusted Albatross hooks for policy enforcement.
Payloads may contain raw user prompts, tool arguments, tool results, and local
paths. Treat subscribed event data as sensitive and avoid logging it unless the
extension performs its own redaction.

## Version 1 boundaries

Protocol v1 intentionally does not support custom model providers, custom TUI
components, prompts/skills/themes packaging, or extension-to-host requests.
Those capabilities can be added without changing the tool, command, and event
contracts above.
