"""The settings' list of permissions says, for the test account, what the cache knows
Google to have said. Nothing is written to Google, and "Allow" is not pressed: it
opens the browser, where only the owner can answer.

  perms.py <screenshot folder> <cache.db of the test folder>
"""
import sqlite3, sys, time
from common import *

NAMES = ["Calendars and events", "Contacts", "Other contacts", "Your organisation's directory"]
SCOPES = ["calendar", "contacts.readonly", "contacts.other.readonly", "directory.readonly"]


def account():
    c = sqlite3.connect(f"file:{dbpath}?mode=ro", uri=True)
    rows = c.execute("select email, scopes from accounts").fetchall()
    c.close()
    assert len(rows) == 1, "a one-account folder is needed"
    return rows[0]


def expected():
    email, scopes = account()
    granted = None if scopes is None else {s.rsplit("/", 1)[-1] for s in scopes.split()}
    lines = []
    for name, scope in zip(NAMES, SCOPES):
        state = "Not known yet" if granted is None else "Allowed" if scope in granted else "Not allowed"
        lines.append(f"{name}, {email}: {state}")
    return lines


def shown():
    return [l for l in (label(h) for h in find("Text")) if any(l.startswith(n + ", ") for n in NAMES)]


ok = True
# The first sync after the start tells what the account allows.
ok &= wait("the cache knows what the account allows", lambda: account()[1] is not None, 60)
key("s")
page = [b for b in find("Button") if label(b) == "Permissions…"]
ok &= wait("the settings offer the permissions", lambda: len(page) == 1, 1)
if page:
    call("click_element", elementHandle=page[0]); time.sleep(0.4)
ok &= wait("the settings list each permission's state", lambda: shown() == expected())
print("  ", shown())
allow = [label(b) for b in find("Button") if label(b).startswith("Allow ")]
missing = [l for l in expected() if l.endswith("Not allowed")]
ok &= wait("a missing permission can be allowed, a given one not", lambda: len(allow) == len(missing), 1)
shot("permissions")
key(ESC)
sys.exit(0 if ok else 1)
