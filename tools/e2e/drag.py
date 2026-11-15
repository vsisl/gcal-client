"""Drag an event in the test app and check Google."""
import json, sys, time
from common import *


def props(h): return json.loads(call("get_element_properties", elementHandle=h)["content"][0]["text"])
def start(): return named("AT drag")[0]["start"]["dateTime"], named("AT drag")[0]["end"]["dateTime"]
time.sleep(4)
h = items("AT drag")[0]
p = props(h)
print({k: p[k] for k in p if k in ("absolutePosition", "size", "x", "y", "width", "height")})
pos, size = p["absolutePosition"], p["size"]
cx, cy = pos["x"] + size["width"] / 2, pos["y"] + size["height"] / 2
print("before:", start())
call("drag_element", elementHandle=h, target={"x": cx + size["width"], "y": cy + 24})
time.sleep(0.3); shot("drag-optimistic")
wait("move: a day later and half an hour down, at Google", lambda: start()[0].startswith(at(2, "09:30:00")[:16]))
print("after:", start(), named("AT drag")[0]["start"].get("timeZone"))
time.sleep(1); shot("drag-after")
key("z")
wait("undo move: back", lambda: start()[0].startswith(at(1, "09:00:00")[:16]))
time.sleep(1)
call("click_element", elementHandle=items("AT drag")[0]); time.sleep(0.4)
shot("drag-click-still-opens-details")
key(DEL)
wait("cleanup", lambda: not named("AT drag"))
