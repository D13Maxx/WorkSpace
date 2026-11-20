"""A minimal MCP server over stdio, for tests: two tools, Serena-style annotations."""
import json
import os
import sys

TOOLS = [
    {"name": "find_symbol", "description": "Find a symbol",
     "inputSchema": {"type": "object", "properties": {"name_path": {"type": "string"}}},
     "annotations": {"readOnlyHint": True, "destructiveHint": False}},
    {"name": "replace_symbol_body", "description": "Replace a symbol body",
     "inputSchema": {"type": "object"},
     "annotations": {"readOnlyHint": False, "destructiveHint": True}},
]

for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue  # notifications need no reply
    method, rid = msg.get("method"), msg["id"]
    if method == "initialize":
        result = {"protocolVersion": msg["params"]["protocolVersion"],
                  "capabilities": {"tools": {}},
                  "serverInfo": {"name": "fake", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": TOOLS}
    elif method == "tools/call":
        p = msg["params"]
        args = p.get("arguments", {})
        # Serena-style editing: writes `body` to `relative_path` under --project.
        if p["name"] == "replace_symbol_body" and "--project" in sys.argv and "body" in args:
            root = sys.argv[sys.argv.index("--project") + 1]
            with open(os.path.join(root, args["relative_path"]), "w") as f:
                f.write(args["body"])
        result = {"content": [{"type": "text",
                               "text": f"{p['name']} {json.dumps(p.get('arguments', {}), sort_keys=True)}"}],
                  "isError": False}
    else:
        reply = {"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": method}}
        print(json.dumps(reply), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": rid, "result": result}), flush=True)
