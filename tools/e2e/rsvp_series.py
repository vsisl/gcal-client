"""Answering an invitation to a repeating event, for one event and for all of them,
checked against Google.

Needs the `invites` fixture.
"""
import json, sys, time
from common import *

NAME = "AT invites"


def own(event):
    return [g["responseStatus"] for g in event["attendees"] if g.get("self")][0]


def series():
    return [e for e in named(NAME) if e.get("recurrence")][0]


def changed():
    """The occurrences that are events of their own, by their original start."""
    return {e["originalStartTime"]["dateTime"]: own(e) for e in named(NAME)
            if e.get("recurringEventId") and e.get("status") != "cancelled"}


def click(role, text):
    call("click_element", elementHandle=[h for h in find(role) if label(h) == text][0]); time.sleep(0.4)


def chosen():
    return [label(h) for h in find("ListItem") if label(h) in ("This event", "All events")]


time.sleep(4)
key("w")
wait("the three events are shown", lambda: len(items(NAME)) == 3)

# 1. Yes to the first one only.
call("click_element", elementHandle=items(NAME)[0]); time.sleep(0.5)
click("Button", "Yes"); time.sleep(0.5)
shot("rsvp-series-ask")
wait("it asks: this event or all, and nothing else", lambda: chosen() == ["This event", "All events"] and "Send e-mails to the guests" not in [label(h) for h in find("Checkbox")])
key(ENTER)
wait("yes for the first event alone", lambda: list(changed().values()) == ["accepted"] and own(series()) == "needsAction")
first = list(changed())[0]
time.sleep(3); key("z")
wait("undo: that event unanswered again", lambda: changed() == {first: "needsAction"} and own(series()) == "needsAction")
time.sleep(2)

# 2. Closing the question sends nothing.
call("click_element", elementHandle=items(NAME)[1]); time.sleep(0.5)
click("Button", "No"); time.sleep(0.5)
key(ESC); time.sleep(3)
wait("cancelled: nothing changed", lambda: changed() == {first: "needsAction"} and own(series()) == "needsAction")

# 3. No to all of them. The event answered on its own keeps its own answer.
call("click_element", elementHandle=items(NAME)[1]); time.sleep(0.5)
click("Button", "No"); time.sleep(0.5)
key(DOWN)
key(ENTER)
wait("no for the series", lambda: own(series()) == "declined")
time.sleep(2)
print("   the one answered on its own:", changed())

# 4. An event that was answered on its own is not asked about again.
call("click_element", elementHandle=items(NAME)[0]); time.sleep(0.5)
click("Button", "Maybe"); time.sleep(0.5)
wait("no question for it; maybe saved", lambda: not chosen() and changed().get(first) == "tentative")
