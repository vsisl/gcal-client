"""Guests in the details card, and answering an invitation, checked against Google.

Needs the `invite` fixture.
"""
import json, sys, time
from common import *


def own_answer():
    return [g["responseStatus"] for g in named("AT invite")[0]["attendees"] if g.get("self")][0]


def click(role, text):
    call("click_element", elementHandle=[h for h in find(role) if label(h) == text][0]); time.sleep(0.4)


time.sleep(4)
wait("the invitation is shown", lambda: len(items("AT invite")) == 1)
call("click_element", elementHandle=items("AT invite")[0]); time.sleep(0.5)
shot("rsvp-details")
guests = [label(h) for h in find("ListItem") if "answer" in label(h) or ", yes" in label(h)]
print("   guests:", guests)
wait("both guests are listed, unanswered", lambda: len([g for g in guests if g.endswith("no answer yet")]) == 2)
click("Button", "Maybe")
wait("maybe: saved at Google", lambda: own_answer() == "tentative")
time.sleep(3); key("z")
wait("undo: unanswered again", lambda: own_answer() == "needsAction")
time.sleep(2)
call("click_element", elementHandle=items("AT invite")[0]); time.sleep(0.5)
click("Button", "Yes")
wait("yes: saved at Google", lambda: own_answer() == "accepted")
time.sleep(1)
call("click_element", elementHandle=items("AT invite")[0]); time.sleep(0.5)
shot("rsvp-answered")
key(ESC)

# The organiser may change an event with guests; it asks whether to tell them.
guests_before = named("AT invite")[0]["attendees"]
call("click_element", elementHandle=items("AT invite")[0]); time.sleep(0.5)
key("e"); time.sleep(0.5)
key(" "); key("2")
click("Button", "Save"); time.sleep(0.5)
shot("rsvp-edit-ask")
asked = [label(h) for h in find("Checkbox")]
print("   asked:", asked)
wait("the question offers to tell the guests", lambda: "Send e-mails to the guests" in asked)
# Not for a made-up address.
click("Checkbox", "Send e-mails to the guests")
key(ENTER)
wait("renamed at Google, guests as they were", lambda: len(named("AT invite 2")) == 1 and named("AT invite 2")[0]["attendees"] == guests_before)
