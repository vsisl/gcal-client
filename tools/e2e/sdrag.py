"""Drag occurrences of a repeating event in the test app and check Google."""
import base64, json, sqlite3, sys, time, urllib.request
PORT = 8766
W = {"index": "1", "generation": "1"}
out, dbpath = sys.argv[1], sys.argv[2]
def call(tool, **args):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": {"name": tool, "arguments": args}}).encode()
    req = urllib.request.Request(f"http://127.0.0.1:{PORT}/mcp", data=body,
                                 headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req))["result"]
def shot(name):
    for part in call("take_screenshot", windowHandle=W)["content"]:
        if part["type"] == "image":
            open(f"{out}/{name}.png", "wb").write(base64.b64decode(part["data"]))
def key(text):
    call("dispatch_key_event", windowHandle=W, text=text); time.sleep(0.3)
def find(role):
    r = call("query_element_descendants", elementHandle=W, findAll=True,
             queryStack=[{"matchDescendants": True}, {"matchElementAccessibleRole": role}])
    h = json.loads(r["content"][0]["text"])
    h = h.get("elementHandles", h) if isinstance(h, dict) else h
    # The query lists an element once per path to it.
    seen, unique = set(), []
    for handle in h:
        k = json.dumps(handle, sort_keys=True)
        if k not in seen:
            seen.add(k); unique.append(handle)
    return unique
def label(h):
    return json.loads(call("get_element_properties", elementHandle=h)["content"][0]["text"]).get("accessibleLabel", "")
def items(prefix):
    return [h for h in find("ListItem") if label(h).startswith(prefix)]
def events():
    c = sqlite3.connect(f"file:{dbpath}?mode=ro", uri=True)
    rows = [json.loads(r[0]) for r in c.execute("select json from events")]
    c.close()
    return rows
def named(name):
    return [e for e in events() if e.get("summary") == name]
def wait(what, cond, timeout=25):
    end = time.time() + timeout
    while time.time() < end:
        try:
            if cond():
                print("PASS", what); return True
        except Exception as e:
            pass
        time.sleep(0.5)
    print("FAIL", what); shot("fail-" + what.replace(" ", "-")[:40]); return False
DEL, DOWN, ENTER, ESC = "\u007f", "\uf701", "\n", "\u001b"
def notice():
    return "OK" in [label(b) for b in find("Button")]
DEL, DOWN, ENTER, ESC = "\u007f", "", "\n", "\u001b"


def props(h): return json.loads(call("get_element_properties", elementHandle=h)["content"][0]["text"])
def masters(): return sorted([e for e in events() if e.get("summary") == "AT series" and e.get("recurrence")], key=lambda e: e["start"]["dateTime"])
def exceptions(): return [e for e in events() if e.get("recurringEventId") and e.get("status") != "cancelled" and e.get("summary") == "AT series"]
def drag(index, dx_days, dy, downs):
    h = items("AT series")[index]
    p = props(h); pos, size = p["absolutePosition"], p["size"]
    cx, cy = pos["x"] + size["width"] / 2, pos["y"] + size["height"] / 2
    call("drag_element", elementHandle=h, target={"x": cx + dx_days * (size["width"] + 11), "y": cy + dy})
    time.sleep(0.5)
    for _ in range(downs): key(DOWN)
    shot(f"sdrag-ask-{downs}")
    key(ENTER)
time.sleep(4)
wait("six shown", lambda: len(items("AT series")) == 6)
# this: Wednesday half an hour later
drag(2, 0, 24, 0)
wait("this: one changed occurrence at 09:30Z", lambda: [e["start"]["dateTime"] for e in exceptions()] == ["2026-11-11T09:30:00Z"])
time.sleep(1); shot("sdrag-this")
time.sleep(3); print("   undo offered:", "Undo" in [label(b) for b in find("Button")]); key("z")
wait("undo this: back at 09:00Z", lambda: [e["start"]["dateTime"] for e in exceptions()] == ["2026-11-11T09:00:00Z"])
time.sleep(11)
# all: half an hour later
drag(0, 0, 24, 2)
wait("all: series starts 09:30Z", lambda: masters()[0]["start"]["dateTime"] == "2026-11-09T09:30:00Z")
print("   exceptions after all:", [(e["start"]["dateTime"]) for e in exceptions()])
time.sleep(3); print("   undo offered:", "Undo" in [label(b) for b in find("Button")]); key("z")
wait("undo all: series starts 09:00Z", lambda: masters()[0]["start"]["dateTime"] == "2026-11-09T09:00:00Z")
time.sleep(11)
# following: Thursday on, an hour later
drag(3, 0, 48, 1)
wait("following: two series", lambda: len(masters()) == 2)
m = masters()
print("   old:", m[0]["start"]["dateTime"], m[0]["recurrence"], "new:", m[1]["start"]["dateTime"], m[1]["recurrence"])
wait("following: new series from Thursday 10:00Z with three left", lambda: masters()[1]["start"]["dateTime"] == "2026-11-12T10:00:00Z" and masters()[1]["recurrence"] == ["RRULE:FREQ=DAILY;COUNT=3"])
wait("following: old series ends before Thursday", lambda: "UNTIL=20261112T085959Z" in masters()[0]["recurrence"][0])
wait("following: six shown", lambda: len(items("AT series")) == 6)
time.sleep(1); shot("sdrag-following")
