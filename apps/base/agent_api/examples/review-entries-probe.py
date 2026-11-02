#!/usr/bin/env python3
"""Runtime check for `review_entries` over a running instance's /mcp.

It sends what an AI client sends: initialize, tools/list, then the linked plan
of a business-card task (five new companies, contacts linked to them, one
contact at two companies) with the optional `agent_self_report` argument that
the connect instructions ask every call to carry. Run it against a disposable
database after changing `apps/base/agent_api/src/review.rs` or `mcp.rs`.

  BILIBI_MCP_URL=https://bilibi.localhost/mcp BILIBI_TOKEN=<access token of an
  AI connection that is switched on> python3 review-entries-probe.py

Opening a review writes nothing. Exits 1 when a case does not behave as listed.
The token comes from the environment, never an argument, and is never printed.
`BILIBI_ORIGIN` names the Origin header when the URL is a backend address.
"""
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request

URL = os.environ["BILIBI_MCP_URL"]
TOKEN = os.environ["BILIBI_TOKEN"]
ORIGIN = os.environ.get("BILIBI_ORIGIN") or "{0.scheme}://{0.netloc}".format(urllib.parse.urlparse(URL))
state = {"id": 0, "failed": 0}


def rpc(method, params=None):
    state["id"] += 1
    body = {"jsonrpc": "2.0", "id": state["id"], "method": method}
    if params is not None:
        body["params"] = params
    request = urllib.request.Request(URL, json.dumps(body).encode(), {
        "Authorization": "Bearer " + TOKEN, "Content-Type": "application/json",
        "Accept": "application/json, text/event-stream", "Origin": ORIGIN,
    })
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            return json.loads(response.read())["result"]
    except urllib.error.HTTPError as error:
        sys.exit(f"{method}: HTTP {error.code} {error.read()[:200]!r}")


def check(label, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + label + ("" if ok else f": {detail}"))
    state["failed"] += not ok


def call(arguments):
    result = rpc("tools/call", {"name": "review_entries", "arguments": arguments})
    return result["isError"], result["structuredContent"]


def ref(step):
    return {"$ref": f"{step}.id"}


companies = [("Mono Strategy Sdn Bhd", "MonoStrategy"), ("Bell Hon Sdn Bhd", "BellHon"), ("DES Electronics Sdn Bhd", "DES"),
             ("epd", "epd"), ("Pong Codan Rubber (M) Sdn Bhd", "PongCodan")]
plan = [{"key": "core.company.create", "input": {"name": name, "code": code}} for name, code in companies]
for name, company in [("Sin Jin Cheng", 1), ("Evelyne", 3), ("NG Yee Zhing", 4), ("Kam Pui Yue", 5)]:
    plan.append({"key": "core.affiliation.contact.create", "input": {"name": name, "organisation_id": ref(company), "title": "Manager"}})
# The first contact (step 6) also holds a position at the second company.
plan.append({"key": "core.affiliation.create", "input": {"person_id": ref(6), "organisation_id": ref(2), "title": "Manager"}})
report = "probe/review-entries-default"

rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "review-entries-probe", "version": "1"}})
tool = next(t for t in rpc("tools/list")["tools"] if t["name"] == "review_entries")
check("tools/list points the tool at its card", bool(tool["_meta"]["ui"]["resourceUri"]))
check("review_entries declares agent_self_report", "agent_self_report" in tool["inputSchema"]["properties"])

failed, body = call({"operations": plan, "agent_self_report": report})
check("linked plan with agent_self_report opens the card", not failed and body.get("linked") is True and len(body["operations"]) == len(plan), body)
failed, body = call({"key": "core.company.create", "entries": [{"name": "Acme", "code": "Acme"}], "agent_self_report": report})
check("one key with agent_self_report opens the card", not failed, body)
failed, body = call({"operations": plan})
check("linked plan without agent_self_report opens the card", not failed, body)

steps = [{"key": step["key"], "entries": [step["input"]]} for step in plan]
failed, body = call({"operations": steps, "agent_self_report": report})
text = json.dumps(body)
check("a step with `entries` is refused naming operations[1] and `input`", failed and "operations[1]" in text and "`input`" in text, body)
failed, body = call({"operations": json.dumps(plan)})
check("operations sent as a string is refused saying so", failed and "not a string" in json.dumps(body), body)
failed, body = call({"key": "core.company.create", "operations": plan})
check("key with operations is refused saying to send operations alone", failed and "alone" in json.dumps(body), body)
failed, body = call({"operations": [{"key": "core.affiliation.contact", "input": {"name": "x"}}]})
check("an unknown key is refused naming it", failed and "core.affiliation.contact" in json.dumps(body), body)
sys.exit(1 if state["failed"] else 0)
