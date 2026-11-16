"""An invitation between two test accounts, seen from both sides and checked against
Google: the organiser invites, the guest answers, the organiser changes and deletes.

usage: pair.py <screenshot folder> <organiser's cache.db> <guest's cache.db>

Two test apps must be running, each on a one-account data folder of its own: the
organiser's with SLINT_MCP_PORT=8766, the guest's with SLINT_MCP_PORT=8767. Both
accounts must be ones set aside for testing. No e-mail is sent: the box is unticked
each time. Needs no fixture.
"""
import json, sqlite3, sys, time
import common
from common import *

ORGANISER = (8766, sys.argv[2])
GUEST = (8767, sys.argv[3])
HINT = "ann@example.com, bob@example.com"
ASK = "Send e-mails to the guests"
NAME = "AT pair"


def account(cache):
    c = sqlite3.connect(f"file:{cache}?mode=ro", uri=True)
    rows = [r[0] for r in c.execute("select email from accounts")]
    c.close()
    if len(rows) != 1:
        sys.exit(f"pair: {cache} must hold exactly one account, it holds {len(rows)}")
    return rows[0]


def click(role, text):
    call("click_element", elementHandle=[h for h in find(role) if label(h) == text][0]); time.sleep(0.4)


def answers(name):
    return {g["email"]: g.get("responseStatus") for g in named(name)[0].get("attendees", [])}


def refresh():
    key(ESC); key("r")


organiser, guest = account(ORGANISER[1]), account(GUEST[1])
if organiser == guest:
    sys.exit("pair: the two caches hold the same account")

time.sleep(4)
for app in (ORGANISER, GUEST):
    common.use(*app); key("m")

# 1. The organiser invites the guest.
common.use(*ORGANISER)
key("c")
for ch in NAME: key(ch)
click("Button", "More options")
click("TextInput", HINT)
for ch in guest: key(ch)
click("Button", "Save"); time.sleep(0.5)
click("Checkbox", ASK)
key(ENTER)
wait("organiser: created with the guest unanswered", lambda: answers(NAME) == {organiser: "accepted", guest: "needsAction"})

# 2. It arrives in the guest's calendar, and the guest says yes.
common.use(*GUEST)
refresh()
wait("guest: the invitation arrived", lambda: named(NAME)[0]["organizer"]["email"] == organiser and answers(NAME)[guest] == "needsAction", timeout=40)
wait("guest: it is on screen", lambda: len(items(NAME)) == 1)
call("click_element", elementHandle=items(NAME)[0]); time.sleep(0.5)
shot("pair-guest-invited")
click("Button", "Yes")
wait("guest: yes saved at Google", lambda: answers(NAME)[guest] == "accepted")

# 3. The organiser sees the answer.
common.use(*ORGANISER)
refresh()
wait("organiser: sees the guest's yes", lambda: answers(NAME)[guest] == "accepted", timeout=40)
time.sleep(1)
call("click_element", elementHandle=items(NAME)[0]); time.sleep(0.5)
shot("pair-organiser-answered")

# 4. The organiser renames it; the guest's copy follows and keeps the answer.
key("e"); time.sleep(0.5)
key(" "); key("2")
click("Button", "Save"); time.sleep(0.5)
click("Checkbox", ASK)
key(ENTER)
wait("organiser: renamed at Google", lambda: len(named(NAME + " 2")) == 1)
common.use(*GUEST)
refresh()
wait("guest: sees the new name, still yes", lambda: answers(NAME + " 2")[guest] == "accepted" and not named(NAME), timeout=40)

# 5. The guest changes the answer; the organiser sees it.
time.sleep(1)
call("click_element", elementHandle=items(NAME + " 2")[0]); time.sleep(0.5)
click("Button", "No")
wait("guest: no saved at Google", lambda: answers(NAME + " 2")[guest] == "declined")
common.use(*ORGANISER)
refresh()
wait("organiser: sees the guest's no", lambda: answers(NAME + " 2")[guest] == "declined", timeout=40)

# 6. The organiser deletes it, without cancellation e-mails; it leaves the guest's
#    calendar too.
time.sleep(11)
call("click_element", elementHandle=items(NAME + " 2")[0]); time.sleep(0.5)
key(DEL); time.sleep(0.5)
click("Checkbox", "Send cancellation e-mails to guests")
key(ENTER)
wait("organiser: deleted", lambda: not named(NAME + " 2"))
common.use(*GUEST)
refresh()
wait("guest: gone from the guest's calendar", lambda: not named(NAME + " 2"), timeout=40)
