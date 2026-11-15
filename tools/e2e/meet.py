"""A Google Meet video meeting from the editor: asked for on a new event, shown in the
details, removed and brought back, checked against Google.

Needs no fixture.
"""
import json, sys, time
from common import *

BOX = "Google Meet video meeting"


def click(role, text):
    call("click_element", elementHandle=[h for h in find(role) if label(h) == text][0]); time.sleep(0.4)


def link():
    return named("AT meet")[0].get("hangoutLink", "")


time.sleep(4)
key("m")
key("c")
for ch in "AT meet": key(ch)
click("Button", "More options")
click("Checkbox", BOX)
click("Button", "Save")
wait("created with a meeting link", lambda: link().startswith("https://meet.google.com/"))
first = link()
# The app fetches the event once more a few seconds after creating a meeting, which
# closes an open card; wait that out first.
time.sleep(6)
call("click_element", elementHandle=items("AT meet")[0]); time.sleep(0.5)
shot("meet-details")
wait("the details offer to join it", lambda: any(label(b) == "Join the video meeting" for b in find("Button")))
key("e"); time.sleep(0.5)
click("Checkbox", BOX)
click("Button", "Save")
wait("unticked: the link is gone", lambda: link() == "")
time.sleep(5); key("z")
wait("undo: the same link is back", lambda: link() == first)
# Google settles a change to a meeting a moment after answering; the app fetches the
# settled version within a few seconds. Before that, a further change is refused.
time.sleep(11)
call("click_element", elementHandle=items("AT meet")[0]); time.sleep(0.5)
key(DEL)
wait("deleted", lambda: not named("AT meet"))
