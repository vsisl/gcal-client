"""An invitation between two test accounts, seen from both sides and checked against
Google: the organiser invites, the guest answers, the organiser changes and deletes.

usage: pair.py <screenshot folder> <organiser's cache.db> <guest's cache.db> [--email]

Two test apps must be running, each on a one-account data folder of its own: the
organiser's with SLINT_MCP_PORT=8766, the guest's with SLINT_MCP_PORT=8767. Both
accounts must be ones set aside for testing. No e-mail is sent: every question about
e-mails is unticked, and seen to be unticked, before it is confirmed. Needs no fixture.

With --email the three questions are left ticked instead, so that Google e-mails the
guest an invitation, an update and a cancellation. The owner allowed this between the
two test accounts only: before each e-mailed change the script stops unless the
guests it sees (in the editor, and in the cache) are those two and no other. It
cannot rule out a guest added at Google in the seconds between that look and the
change. The app cannot read mail: that the e-mails arrived is for the owner
to see in the guest's inbox. They carry the name the script prints.

Before anything is written the script makes sure that each app really is the one
working on the cache it was given. Whatever goes wrong after the event was created, the
event is deleted again (without e-mails), and the script ends with an error.
"""
import json, sqlite3, sys, time
import common
from common import *

ORGANISER = (8766, sys.argv[2])
GUEST = (8767, sys.argv[3])
HINT = "ann@example.com, bob@example.com"
ASK = "Send e-mails to the guests"
ASK_CANCEL = "Send cancellation e-mails to guests"
# A name of this run's own: an event left by an earlier run must never be taken for it.
NAME = "AT pair " + time.strftime("%H%M%S")
RENAMED = NAME + " 2"
EMAIL = "--email" in sys.argv[4:]


class Failed(Exception):
    pass


def do(tool, **args):
    """A call to the test server that must work: a click that did not happen must not
    be followed by the next key."""
    result = call(tool, **args)
    if result.get("isError"):
        raise Failed(f"{tool}: {result['content'][0]['text']}")
    return result


def properties(handle):
    return json.loads(do("get_element_properties", elementHandle=handle)["content"][0]["text"])


def one(role, text):
    found = [h for h in find(role) if label(h) == text]
    if len(found) != 1:
        raise Failed(f"expected one {role} '{text}', found {len(found)}")
    return found[0]


def click(role, text):
    do("click_element", elementHandle=one(role, text)); time.sleep(0.4)


def open_event(name):
    """Opens the details of the one event on screen with exactly this name."""
    found = [h for h in items(name) if name == RENAMED or not label(h).startswith(RENAMED)]
    if len(found) != 1:
        raise Failed(f"expected one event '{name}' on screen, found {len(found)}: "
                     f"{[label(h) for h in items(name)]}")
    do("click_element", elementHandle=found[0]); time.sleep(0.5)


def confirm(question, email=False):
    """Sets the question about e-mails (unticked unless e-mails are wanted), makes sure
    it is set that way, and only then confirms."""
    if bool(properties(one("Checkbox", question)).get("accessibleChecked")) != email:
        click("Checkbox", question)
    if bool(properties(one("Checkbox", question)).get("accessibleChecked")) != email:
        raise Failed(f"'{question}' could not be set; nothing was confirmed")
    key(ENTER)


def typed_guests():
    """The guests the editor will send: those in its list, and what is still in the
    field for adding them."""
    text = properties(one("TextInput", HINT)).get("accessibleValue", "")
    typed = {part.strip().lower() for part in text.replace(";", ",").split(",") if part.strip()}
    rows = {label(h)[len("Guest: "):].split(",")[0].lower() for h in find("ListItem") if label(h).startswith("Guest: ")}
    return typed | rows


def only_the_two(shown):
    """With --email, stops unless the e-mails can only go to the two test accounts:
    the guests in the editor and the guests the cache has for the event."""
    if not EMAIL:
        return
    if not shown or not shown <= {organiser, guest}:
        raise Failed(f"the editor lists {sorted(shown)} as guests; no e-mail is sent")
    if created and set(answers(mine())) != {organiser, guest}:
        raise Failed("the event has other guests; no e-mail is sent")


def must(passed):
    if not passed:
        raise Failed("a check failed; the steps after it depend on it")


def synced(what, cond):
    """Waits for a change made in the other account to reach this app's cache,
    refreshing every ten seconds: a refresh right after the change brings nothing."""
    key(ESC)
    # Back to the month with today in it: the windows are on the owner's screen, and a
    # turn of the mouse wheel over one shows another month.
    key("t")
    start = time.time()
    while time.time() - start < 120:
        key("r")
        end = time.time() + 10
        while time.time() < end:
            try:
                if cond():
                    print("PASS", what, f"({time.time() - start:.0f} s)"); return
            except Exception:
                pass
            time.sleep(0.5)
    print("FAIL", what); shot("fail-" + what.replace(" ", "-")[:40])
    raise Failed(what)


def setting(cache, name):
    c = sqlite3.connect(f"file:{cache}?mode=ro", uri=True)
    row = c.execute("select value from settings where key = ?", (name,)).fetchone()
    c.close()
    return row and row[0]


def account(cache):
    c = sqlite3.connect(f"file:{cache}?mode=ro", uri=True)
    rows = [r[0] for r in c.execute("select email from accounts")]
    c.close()
    if len(rows) != 1:
        sys.exit(f"pair: {cache} must hold exactly one account, it holds {len(rows)}")
    return rows[0]


def is_on(app):
    """Whether the app behind this port works on this cache: it writes the view it is
    switched to into its own cache, and nowhere else. Nothing goes to Google."""
    common.use(*app)
    for view, number in (("d", "0"), ("m", "2")):
        key(view)
        end = time.time() + 5
        while setting(app[1], "last_view") != number:
            if time.time() > end:
                return False
            time.sleep(0.2)
    return True


def event(cache, **where):
    """This run's event in a cache, found by its id (organiser) or iCalUID (guest)."""
    c = sqlite3.connect(f"file:{cache}?mode=ro", uri=True)
    rows = [json.loads(r[0]) for r in c.execute("select json from events")]
    c.close()
    (field, value), = where.items()
    found = [e for e in rows if e.get(field) == value and e.get("status") != "cancelled"]
    return found[0] if found else None


def answers(found):
    return {g["email"]: g.get("responseStatus") for g in found.get("attendees", [])}


organiser, guest = account(ORGANISER[1]), account(GUEST[1])
if organiser == guest:
    sys.exit("pair: the two caches hold the same account")
time.sleep(4)
for app in (ORGANISER, GUEST):
    if not is_on(app):
        sys.exit(f"pair: the app on port {app[0]} does not work on {app[1]}; nothing was written")
# New events go to the calendar used last. It must be the organiser's own.
target = setting(ORGANISER[1], "new_event_calendar")
if target not in (None, f"{organiser}\t{organiser}"):
    sys.exit(f"pair: the organiser's app would create the event in {target!r}; nothing was written")

created = None   # the event's id at the organiser, once it exists
uid = None


def mine():
    return event(ORGANISER[1], id=created)


def theirs():
    return event(GUEST[1], iCalUID=uid)


def run():
    global created, uid
    # 1. The organiser invites the guest.
    common.use(*ORGANISER)
    key("c")
    for ch in NAME: key(ch)
    click("Button", "More options")
    click("TextInput", HINT)
    for ch in guest: key(ch)
    if typed_guests() != {guest}:
        raise Failed(f"the guests field holds {sorted(typed_guests())}, not the guest; nothing was saved")
    click("Button", "Save"); time.sleep(0.5)
    confirm(ASK, EMAIL)
    must(wait("organiser: created", lambda: len(named(NAME)) == 1))
    created, uid = named(NAME)[0]["id"], named(NAME)[0]["iCalUID"]
    must(wait("organiser: with the guest unanswered, in the organiser's own calendar",
              lambda: answers(mine()) == {organiser: "accepted", guest: "needsAction"}
              and mine()["organizer"]["email"] == organiser))

    # 2. It arrives in the guest's calendar, and the guest says yes.
    common.use(*GUEST)
    synced("guest: the invitation arrived", lambda: theirs()["organizer"]["email"] == organiser and answers(theirs())[guest] == "needsAction")
    must(wait("guest: it is on screen", lambda: len(items(NAME)) == 1))
    open_event(NAME)
    shot("pair-guest-invited")
    click("Button", "Yes")
    must(wait("guest: yes saved at Google", lambda: answers(theirs())[guest] == "accepted"))

    # 3. The organiser sees the answer.
    common.use(*ORGANISER)
    synced("organiser: sees the guest's yes", lambda: answers(mine())[guest] == "accepted")
    must(wait("organiser: it is on screen", lambda: len(items(NAME)) == 1))
    open_event(NAME)
    shot("pair-organiser-answered")

    # 4. The organiser renames it; the guest's copy follows and keeps the answer.
    key("e"); time.sleep(0.5)
    key(" "); key("2")
    only_the_two(typed_guests())
    click("Button", "Save"); time.sleep(0.5)
    confirm(ASK, EMAIL)
    must(wait("organiser: renamed at Google", lambda: mine()["summary"] == RENAMED))
    common.use(*GUEST)
    synced("guest: sees the new name, still yes", lambda: theirs()["summary"] == RENAMED and answers(theirs())[guest] == "accepted")

    # 5. The guest changes the answer; the organiser sees it. Not with --email: the
    #    cancellation is to be seen arriving, and after a run in which the guest had
    #    declined none arrived.
    if not EMAIL:
        must(wait("guest: the new name is on screen", lambda: len(items(RENAMED)) == 1))
        open_event(RENAMED)
        click("Button", "No")
        must(wait("guest: no saved at Google", lambda: answers(theirs())[guest] == "declined"))
        common.use(*ORGANISER)
        synced("organiser: sees the guest's no", lambda: answers(mine())[guest] == "declined")

    # 6. The organiser deletes it, without cancellation e-mails; it leaves the guest's
    #    calendar too.
    common.use(*ORGANISER)
    time.sleep(11)
    delete(EMAIL)
    must(wait("organiser: deleted", lambda: mine() is None))
    common.use(*GUEST)
    synced("guest: gone from the guest's calendar", lambda: theirs() is None)


def delete(email=False):
    """Deletes this run's event in the organiser's app; without e-mails in clean-up."""
    common.use(*ORGANISER)
    name = mine()["summary"]
    for _ in range(4): key(ESC)
    key("t")
    must(wait("organiser: the event to delete is on screen", lambda: len(items(name)) >= 1))
    open_event(name)
    key(DEL); time.sleep(0.5)
    # E-mails only to this run's own guest list: the organiser and the guest. The
    # event may have been given other guests at Google since it was created.
    if email and set(answers(mine())) != {organiser, guest}:
        raise Failed("the event has other guests; no e-mail is sent")
    confirm(ASK_CANCEL, email)


if EMAIL:
    print(f"e-mails to {guest} will carry the names '{NAME}' and '{RENAMED}'")
try:
    run()
except Exception as error:
    print("FAIL", f"{type(error).__name__}: {error}")
    # Both copies as the caches have them, to see which side is behind.
    for who, copy in (("organiser", created and mine()), ("guest", uid and theirs())):
        if copy:
            print(f"   {who}:", {name: copy.get(name) for name in ("summary", "location", "updated", "sequence", "guestsCanModify", "etag")})
    try:
        shot("fail-last")
        if created and mine():
            time.sleep(11)   # an Undo offer would take the Delete key's place
            delete()
            if wait("clean-up: the event is deleted again", lambda: mine() is None):
                print("the event was deleted; the guest's copy goes with it at Google")
            else:
                print(f"NOT CLEANED UP: '{mine()['summary']}' is still in {organiser}'s calendar")
        elif created is None and named(NAME):
            print(f"NOT CLEANED UP: '{NAME}' may be in {organiser}'s calendar")
    except Exception as second:
        print(f"NOT CLEANED UP ({type(second).__name__}: {second}): look for '{NAME}' in {organiser}'s calendar")
    sys.exit(1)
