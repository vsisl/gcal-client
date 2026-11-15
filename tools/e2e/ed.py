"""The editor in the test app, checked against Google."""
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


END, BACK = "\uf72b", "\u0008"
def by_label(role, text):
    return [h for h in find(role) if label(h) == text]
def click(role, text, index=0):
    call("click_element", elementHandle=by_label(role, text)[index]); time.sleep(0.4)
def type_into(hint, text, erase=0, index=0):
    click("TextInput", hint, index)
    key(END)
    for _ in range(erase): key(BACK)
    for ch in text: key(ch)
def choose(current, option):
    click("Combobox", current); click("ListItem", option)
def ev(name): return named(name)[0]
def masters(name): return [e for e in events() if e.get("summary") == name and e.get("recurrence")]
time.sleep(4)
key("m")
if len(sys.argv) > 3 and sys.argv[3] == "clean":
    # Only what the scripts name: "AT " and a word. Not "ATM", "Attic", ...
    while items("AT "):
        n = len(items("AT "))
        call("click_element", elementHandle=items("AT ")[0]); time.sleep(0.4)
        key(DEL); time.sleep(0.4)
        if "Cancel" in [label(b) for b in find("Button")]:
            key(DOWN); key(DOWN); key(ENTER)
        wait("clean", lambda: len(items("AT ")) < n)
    print("left:", [e.get("summary") for e in events() if e.get("status") != "cancelled"])
    sys.exit(0)

# 1. A new event through the quick card and "More options".
key("c")
for ch in "AT edit": key(ch)
click("Button", "More options")
type_into("Add location", "Room 1")
choose("The calendar's usual reminders", "10 minutes before")
click("ListItem", "Colour 5")
shot("ed-filled")
click("Button", "Save")
wait("new: created with location, colour and reminder", lambda: ev("AT edit")["location"] == "Room 1" and ev("AT edit")["colorId"] == "5" and ev("AT edit")["reminders"]["overrides"][0]["minutes"] == 10)
start = ev("AT edit")["start"]["dateTime"]
time.sleep(2)

# 2. Edit it: title, a later end, repeating daily; then undo.
call("click_element", elementHandle=items("AT edit")[0]); time.sleep(0.4)
key("e"); time.sleep(0.5)
key(" "); key("2")
old_end = ev("AT edit")["end"]["dateTime"]
type_into("10:30", "23:30", erase=5)
choose("Does not repeat", "Daily")
shot("ed-edited")
click("Button", "Save")
wait("edit: title, end and rule changed", lambda: ev("AT edit 2")["end"]["dateTime"] != old_end and ev("AT edit 2")["recurrence"] == ["RRULE:FREQ=DAILY"] and ev("AT edit 2")["start"]["dateTime"] == start)
print("   end:", old_end, "->", ev("AT edit 2")["end"]["dateTime"], "kept:", ev("AT edit 2")["location"], ev("AT edit 2")["colorId"])
time.sleep(3); print("   undo offered:", "Undo" in [label(b) for b in find("Button")]); key("z")
wait("undo edit: as before", lambda: ev("AT edit")["end"]["dateTime"] == old_end and not ev("AT edit").get("recurrence"))
time.sleep(11)

# 2b. All day and back: the other kind of time must go, both ways.
call("click_element", elementHandle=items("AT edit")[0]); time.sleep(0.4)
key("e"); time.sleep(0.5)
click("Checkbox", "All day")
click("Button", "Save")
wait("all day: has a date and no time", lambda: ev("AT edit")["start"].get("date") and not ev("AT edit")["start"].get("dateTime"))
time.sleep(3); key("z")
wait("undo all day: timed again", lambda: ev("AT edit")["start"].get("dateTime") == start and not ev("AT edit")["start"].get("date"))
time.sleep(11)

# 3. A wrong date is refused in words and nothing is sent.
call("click_element", elementHandle=items("AT edit")[0]); time.sleep(0.4)
key("e"); time.sleep(0.5)
type_into("2026-11-17", "x", index=0)
click("Button", "Save"); time.sleep(0.5)
shot("ed-error")
print("   editor still open:", bool(by_label("Button", "Save")))
key(ESC)

# 4. A repeating event: all, then this.
key("w")
wait("series shown", lambda: len(items("AT series")) >= 3)
first = items("AT series")[0]
call("click_element", elementHandle=first); time.sleep(0.4)
key("e"); time.sleep(0.5)
key(" "); key("X")
click("Button", "Save"); time.sleep(0.5)
key(DOWN); key(DOWN); shot("ed-ask"); key(ENTER)
wait("all: the series is renamed", lambda: len(masters("AT series X")) == 1)
time.sleep(4)
call("click_element", elementHandle=items("AT series X")[1]); time.sleep(0.4)
key("e"); time.sleep(0.5)
type_into("Add location", "Here")
click("Button", "Save"); time.sleep(0.5)
key(ENTER)
wait("this: one occurrence has the location", lambda: [e.get("location") for e in events() if e.get("recurringEventId") == masters("AT series X")[0]["id"] and e.get("location")] == ["Here"])
print("   series itself has no location:", "location" not in masters("AT series X")[0])
time.sleep(1); shot("ed-end")
