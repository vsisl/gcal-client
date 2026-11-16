"""The editor in the test app, checked against Google."""
import json, sys, time
from common import *


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
            # Never cancellation e-mails for test events: their guests are made up.
            for box in [h for h in find("Checkbox") if label(h) == "Send cancellation e-mails to guests"]:
                call("click_element", elementHandle=box); time.sleep(0.3)
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
