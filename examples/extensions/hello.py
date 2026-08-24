#!/usr/bin/env python3
"""Minimal Albatross extension implementing one tool, command, and event."""

import json
import sys


def respond(request_id, result):
    print(
        json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}),
        flush=True,
    )


for raw_line in sys.stdin:
    try:
        frame = json.loads(raw_line)
        method = frame.get("method")
        params = frame.get("params", {})

        if method == "initialize":
            respond(
                frame["id"],
                {
                    "name": "Hello extension",
                    "version": "0.1.0",
                    "tools": [
                        {
                            "name": "greet",
                            "description": "Return a greeting for a person",
                            "inputSchema": {
                                "type": "object",
                                "properties": {"name": {"type": "string"}},
                                "required": ["name"],
                            },
                            "requiresApproval": False,
                        }
                    ],
                    "commands": [
                        {
                            "name": "hello",
                            "description": "Ask the agent to greet someone",
                        }
                    ],
                    "events": ["session_start", "stop"],
                },
            )
        elif method == "tools/call":
            name = params.get("arguments", {}).get("name", "world")
            respond(frame["id"], {"greeting": f"Hello, {name}!"})
        elif method == "commands/execute":
            name = params.get("arguments", "").strip() or "world"
            respond(
                frame["id"],
                {
                    "message": f"Greeting {name} through the extension…",
                    "prompt": f"Use ext__hello__greet to greet {name}",
                },
            )
        elif method == "events/emit":
            # Notifications are intentionally one-way. A real extension might
            # update a status file, emit metrics, or notify another process.
            pass
        elif "id" in frame:
            print(
                json.dumps(
                    {
                        "jsonrpc": "2.0",
                        "id": frame["id"],
                        "error": {"code": -32601, "message": "method not found"},
                    }
                ),
                flush=True,
            )
    except Exception as error:  # Protocol stdout must remain valid JSON-RPC.
        print(f"extension error: {error}", file=sys.stderr, flush=True)
