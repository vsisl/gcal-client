"""Guests in the editor: inviting on a new event, adding to an existing one, and the
question about e-mails, checked against Google. No e-mail is ever sent: the guests
are made-up addresses and the box is unticked each time.

Needs no fixture.
"""
import json, sys, time
from common import *

END = ""
HINT = "ann@example.com, bob@example.com"
ASK = "Send e-mails to the guests"


def by_label(role, text):
    return [h for h in find(role) if label(h) == text]


def click(role, text, index=0):
    call("click_element", elementHandle=by_label(role, text)[index]); time.sleep(0.4)


def value(role, text):
    return json.loads(call("get_element_properties", elementHandle=by_label(role, text)[0])["content"][0]["text"]).get("accessibleValue", "")


def type_into(hint, text):
    click("TextInput", hint)
    key(END)
    for ch in text: key(ch)


def guests(name):
    """Each guest's answer by address; Google lists them in an order of its own."""
    return {g["email"]: g.get("responseStatus") for g in named(name)[0].get("attendees", [])}


time.sleep(4)
key("m")
# 1. A new event with a guest. Closing the question brings the editor back as it was.
key("c")
for ch in "AT guests": key(ch)
click("Button", "More options")
type_into(HINT, "nobody@example.com")
click("Button", "Save"); time.sleep(0.5)
wait("a new event with guests asks about e-mails", lambda: bool(by_label("Checkbox", ASK)))
key(ESC); time.sleep(0.5)
wait("cancelled: the editor is back with what was typed", lambda: value("TextInput", "Add title") == "AT guests" and value("TextInput", HINT) == "nobody@example.com")
click("Button", "Save"); time.sleep(0.5)
click("Checkbox", ASK)
key(ENTER)
wait("created with the organiser and the guest", lambda: guests("AT guests") == {named("AT guests")[0]["organizer"]["email"]: "accepted", "nobody@example.com": "needsAction"})
time.sleep(2)

# 2. Another guest added to it, and taken back.
call("click_element", elementHandle=items("AT guests")[0]); time.sleep(0.5)
shot("guests-details")
key("e"); time.sleep(0.5)
type_into(HINT, ", second@example.com")
click("Button", "Save"); time.sleep(0.5)
click("Checkbox", ASK)
key(ENTER)
wait("a third guest at Google", lambda: len(guests("AT guests")) == 3 and "second@example.com" in guests("AT guests"))
time.sleep(3); key("z")
wait("undo: two guests again", lambda: len(guests("AT guests")) == 2)
time.sleep(11)

# 2b. What the guests may do. A new event says nothing about it (Google's defaults).
may = lambda: [named("AT guests")[0].get(name) for name in ("guestsCanModify", "guestsCanInviteOthers", "guestsCanSeeOtherGuests")]
wait("a new event leaves the guests' rights at Google's defaults", lambda: may() in ([None, None, None], [None, True, True], [False, True, True]))
call("click_element", elementHandle=items("AT guests")[0]); time.sleep(0.5)
key("e"); time.sleep(0.5)
shot("guests-rights")
click("Checkbox", "See guests")
click("Button", "Save"); time.sleep(0.5)
click("Checkbox", ASK)
key(ENTER)
wait("guests may not see the guest list", lambda: may()[2] is False and not may()[0])
time.sleep(2)
call("click_element", elementHandle=items("AT guests")[0]); time.sleep(0.5)
key("e"); time.sleep(0.5)
click("Checkbox", "Change it")
wait("who may change may do the rest: the other boxes are ticked", lambda: all(json.loads(call("get_element_properties", elementHandle=by_label("Checkbox", text)[0])["content"][0]["text"]).get("accessibleChecked") for text in ("Change it", "Invite others", "See guests")))
click("Button", "Save"); time.sleep(0.5)
click("Checkbox", ASK)
key(ENTER)
wait("guests may change the event, and with it see the list", lambda: may()[0] is True and may()[2] is not False)
time.sleep(3); key("z")
wait("undo: as before", lambda: not may()[0] and may()[2] is False)
time.sleep(11)

# 3. Deleted, without cancellation e-mails.
call("click_element", elementHandle=items("AT guests")[0]); time.sleep(0.5)
key(DEL); time.sleep(0.5)
click("Checkbox", "Send cancellation e-mails to guests")
key(ENTER)
wait("deleted", lambda: not named("AT guests"))
