"""End-to-end run of the window against Google: create, delete and undo, for a plain
event and for a repeating one (this / this and following / all).

It changes a real calendar, so it must only ever see a test account:

1. Build the test copy of the app (it has Slint's test server and the `insert` command):
     SLINT_EMIT_DEBUG_INFO=1 cargo build --features ui-testing -p gcal-client --target-dir target/uitest
2. Make a data folder whose cache.db holds the test account alone (a copy of the real
   cache with every other account's rows deleted), and point XDG_DATA_HOME at it for
   every command below. The app can then reach no other account.
3. Seed the repeating event:
     gcal-client insert <email> <calendar> tools/e2e/series.json
4. Start the app with SLINT_MCP_PORT=8766, then:
     python3 tools/e2e/e2e.py <screenshot-dir> <data folder>/gcal-client/cache.db
   A third argument "reset" instead deletes what an interrupted run left behind.

Each step prints PASS or FAIL; what Google holds is read from the cache after the app's
own sync. Run it with the week of the seeded series (2026-11-09) on screen.
"""
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

time.sleep(4)
shot("e2e-start")
if len(sys.argv) > 3 and sys.argv[3] == "reset":
    for name in ("AT series", "AT plain"):
        while items(name):
            n = len(items(name))
            call("click_element", elementHandle=items(name)[0]); time.sleep(0.4)
            key(DEL); time.sleep(0.3); key(DOWN); key(DOWN); key(ENTER)
            wait("reset " + name, lambda: len(items(name)) < n)
    print("left:", [(e.get("summary"), e.get("status")) for e in events()])
    sys.exit(0)
# 1 create
key("m")  # the month shows events at any hour
if not named("AT plain"):
    key("c")
    for ch in "AT plain": key(ch)
    key(ENTER)
wait("create: event exists at Google", lambda: len(named("AT plain")) == 1 and named("AT plain")[0].get("id"))
wait("create: shown in the window", lambda: len(items("AT plain")) == 1)
# 2 delete + undo
def delete_first(prefix, downs=0, ask=False, index=0):
    call("click_element", elementHandle=items(prefix)[index]); time.sleep(0.4)
    key(DEL)
    if ask:
        time.sleep(0.3)
        for _ in range(downs): key(DOWN)
        shot(f"e2e-ask-{downs}")
        key(ENTER)
old_id = named("AT plain")[0]["id"]
delete_first("AT plain")
wait("delete: gone", lambda: not named("AT plain") and not items("AT plain"))
key("z")
wait("undo delete: back as a new event", lambda: len(named("AT plain")) == 1 and named("AT plain")[0]["id"] != old_id and len(items("AT plain")) == 1)
time.sleep(1)
delete_first("AT plain")
wait("cleanup plain", lambda: not named("AT plain"))
time.sleep(11)  # let the toast go
key("w")

def series(): return [e for e in events() if e.get("summary") == "AT series" and e.get("recurrence")]
def cancelled():
    ids = {e["id"] for e in series()}
    return [e for e in events() if e.get("recurringEventId") in ids and e.get("status") == "cancelled"]
# 3 series: all, while nothing in it was changed: undo brings it back
wait("series: six occurrences shown", lambda: len(items("AT series")) == 6)
delete_first("AT series", downs=2, ask=True, index=1)
wait("all: series gone", lambda: not series() and not items("AT series"))
time.sleep(3); key("z")
wait("undo all: series back", lambda: len(series()) == 1 and len(items("AT series")) == 6)
time.sleep(11)

# 3 series: this
wait("series: six occurrences shown", lambda: len(items("AT series")) == 6)
delete_first("AT series", ask=True, index=2)
wait("this: one occurrence cancelled at Google", lambda: len(cancelled()) == 1)
wait("this: five shown", lambda: len(items("AT series")) == 5)
print("   cancelled:", [e.get("originalStartTime") for e in cancelled()], "notice:", notice())
key("z")
wait("undo this: none cancelled", lambda: not cancelled())
wait("undo this: six shown", lambda: len(items("AT series")) == 6)
shot("e2e-after-undo-this")
time.sleep(11)

# 4 series: this and following
delete_first("AT series", downs=1, ask=True, index=3)
wait("following: rule ends with UNTIL", lambda: "UNTIL=" in series()[0]["recurrence"][0])
print("   rule:", series()[0]["recurrence"])
wait("following: fewer shown", lambda: len(items("AT series")) < 6)
print("   shown:", len(items("AT series")), [e.get("status") for e in events() if e.get("recurringEventId")])
key("z")
wait("undo following: rule back", lambda: series()[0]["recurrence"] == ["RRULE:FREQ=DAILY;COUNT=6"])
wait("undo following: six shown", lambda: len(items("AT series")) == 6)
time.sleep(11)

# 6 series: all, after occurrences were changed: no undo, which would bring back
# another schedule than the one deleted
delete_first("AT series", downs=2, ask=True, index=0)
wait("all (with changed occurrences): series gone", lambda: not series())
time.sleep(3)
print("PASS no undo offered" if "Undo" not in [label(b) for b in find("Button")] else "FAIL undo offered")
print("left in calendar:", [e.get("summary") for e in events() if e.get("status") != "cancelled"])
shot("e2e-end")
