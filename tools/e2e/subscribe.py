"""Adding a calendar to the test account's list and taking it off again, checked
against what the app then has from Google. The calendar is a public one of Google's
(holidays), so nobody else's calendar is touched; the list is left as it was found.

Needs no fixture.

  subscribe.py <screenshot folder> <cache.db of the test folder>
"""
import json, sqlite3, sys, time
from common import *

PUBLIC = "en.usa#holiday@group.v.calendar.google.com"
NOBODY = "nobody-at-all@example.com"
HINT = "ann@example.com"


def rows(sql, *args):
    c = sqlite3.connect(f"file:{dbpath}?mode=ro", uri=True)
    found = c.execute(sql, args).fetchall()
    c.close()
    return found


def calendar(cid):
    found = rows("select json from calendars where id = ?", cid)
    return json.loads(found[0][0]) if found else None


def its_events(cid):
    return rows("select count(*) from events where calendar_id = ?", cid)[0][0]


def by_label(role, text):
    return [h for h in find(role) if label(h) == text]


def click(role, text):
    call("click_element", elementHandle=by_label(role, text)[0]); time.sleep(0.4)


def add(what):
    click("Button", f"Add a calendar to {email}")
    click("TextInput", HINT)
    for ch in what: key(ch)
    key(ENTER)


(email,), = rows("select email from accounts")
ok = True
if calendar(PUBLIC):
    print("the calendar is on the list already (an earlier run failed?): only removing it")
else:
    add(PUBLIC)
    ok &= wait("added: the calendar is in the cache", lambda: calendar(PUBLIC) is not None, 60)
    ok &= wait("its events came with it", lambda: its_events(PUBLIC) > 0, 60)
name = (calendar(PUBLIC) or {}).get("summaryOverride") or (calendar(PUBLIC) or {}).get("summary", "")
ok &= wait("it is in the sidebar, ticked or not", lambda: len(by_label("Checkbox", name)) == 1)
shot("subscribed")

# One that the account may not see, or that is not there: said in a dialog.
add(NOBODY)
ok &= wait("a calendar the account may not see is refused in a dialog", notice)
shot("subscribe-refused")
click("Button", "OK")
ok &= wait("and nothing was added", lambda: calendar(NOBODY) is None, 2)

if name:
    click("Button", f"Remove calendar {name}")
    click("Button", f"Remove {name} from the list")
    ok &= wait("removed: the calendar is gone from the cache", lambda: calendar(PUBLIC) is None, 60)
    ok &= wait("and its events with it", lambda: its_events(PUBLIC) == 0, 10)
    ok &= wait("and from the sidebar", lambda: len(by_label("Checkbox", name)) == 0)
shot("unsubscribed")
sys.exit(0 if ok else 1)
