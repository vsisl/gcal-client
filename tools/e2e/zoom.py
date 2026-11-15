"""Zoom of the day columns in the test app."""
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


def click(role, text):
    call("click_element", elementHandle=[h for h in find(role) if label(h) == text][0]); time.sleep(0.3)
time.sleep(4)
key("s")
for _ in range(3): click("Button", "Zoom out")
shot("zoom-settings")
key(ESC); time.sleep(0.3); shot("zoom-out")
key("s")
for _ in range(6): click("Button", "Zoom in")
key(ESC); time.sleep(0.3); shot("zoom-in")
key("s"); click("Button", "Reset"); key(ESC)
