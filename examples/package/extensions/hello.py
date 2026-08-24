#!/usr/bin/env python3
"""Small JSON-RPC extension shipped inside the example package."""

import json
import sys

for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        result = {
            "name": "Package hello",
            "version": "1.0.0",
            "tools": [{
                "name": "hello",
                "description": "Return a greeting from an installed package",
                "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}},
            }],
        }
    elif method == "tools/call":
        name = request.get("params", {}).get("arguments", {}).get("name", "world")
        result = {"content": f"Hello, {name}!"}
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
