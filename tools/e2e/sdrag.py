"""Drag occurrences of a repeating event in the test app and check Google."""
import json, sys, time
from common import *


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
wait("this: one changed occurrence at 09:30Z", lambda: [e["start"]["dateTime"] for e in exceptions()] == [at(2, "09:30:00")])
time.sleep(1); shot("sdrag-this")
time.sleep(3); print("   undo offered:", "Undo" in [label(b) for b in find("Button")]); key("z")
wait("undo this: back at 09:00Z", lambda: [e["start"]["dateTime"] for e in exceptions()] == [at(2, "09:00:00")])
time.sleep(11)
# all: half an hour later
drag(0, 0, 24, 2)
wait("all: series starts 09:30Z", lambda: masters()[0]["start"]["dateTime"] == at(0, "09:30:00"))
print("   exceptions after all:", [(e["start"]["dateTime"]) for e in exceptions()])
time.sleep(3); print("   undo offered:", "Undo" in [label(b) for b in find("Button")]); key("z")
wait("undo all: series starts 09:00Z", lambda: masters()[0]["start"]["dateTime"] == at(0, "09:00:00"))
time.sleep(11)
# following: Thursday on, an hour later
drag(3, 0, 48, 1)
wait("following: two series", lambda: len(masters()) == 2)
m = masters()
print("   old:", m[0]["start"]["dateTime"], m[0]["recurrence"], "new:", m[1]["start"]["dateTime"], m[1]["recurrence"])
wait("following: new series from Thursday 10:00Z with three left", lambda: masters()[1]["start"]["dateTime"] == at(3, "10:00:00") and masters()[1]["recurrence"] == ["RRULE:FREQ=DAILY;COUNT=3"])
wait("following: old series ends before Thursday", lambda: "UNTIL=" + compact(3, "085959") in masters()[0]["recurrence"][0])
wait("following: six shown", lambda: len(items("AT series")) == 6)
time.sleep(1); shot("sdrag-following")
