"""What the end-to-end scripts share: talking to the app's test server, reading what the
app has cached from Google, and the dates of the week the fixtures are made for.

Every script is started as  <script> <screenshot folder> <cache.db of the test folder>.
"""
import base64, json, sqlite3, sys, time, urllib.request
from datetime import date, timedelta

PORT = 8766
W = {"index": "1", "generation": "1"}
out, dbpath = sys.argv[1], sys.argv[2]
def call(tool, **args):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": {"name": tool, "arguments": args}}).encode()
    req = urllib.request.Request(f"http://127.0.0.1:{PORT}/mcp", data=body,
                                 headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req))["result"]
def use(port, cache):
    """Turns every helper to another running app: its test server and its cache.
    For scripts that drive two apps (pair.py)."""
    global PORT, dbpath
    PORT, dbpath = port, cache
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
    # An element found a moment ago may be gone when it is asked about: the window
    # redraws from the cache whenever a sync brings something. Such an element has no
    # label.
    try:
        return json.loads(call("get_element_properties", elementHandle=h)["content"][0]["text"]).get("accessibleLabel", "")
    except (ValueError, KeyError, IndexError):
        return ""
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


# The fixtures (fixture.py) and the checks are for the week the app opens on: the one
# with today in it, from its Monday. Day 0 is that Monday.
_today = date.today()
MONDAY = _today - timedelta(days=_today.weekday())


def at(day, clock):
    """An instant of this week as Google writes it: at(2, "09:30:00") is Wednesday."""
    return f"{MONDAY + timedelta(days=day)}T{clock}Z"


def compact(day, clock):
    """The same as a repeat rule writes it: compact(3, "085959")."""
    return f"{(MONDAY + timedelta(days=day)).strftime('%Y%m%d')}T{clock}Z"
