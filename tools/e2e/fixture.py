"""Writes the event the end-to-end scripts start from, dated in the current week.

usage: fixture.py series|plain|invite <file to write> [<account>, for invite]

The app opens on the week with today in it, so the fixtures are made for that week:
"series" is a daily event from its Monday, six times, 09:00 to 09:30 UTC; "plain" is one
event on its Tuesday, 09:00 to 10:00 UTC; "invite" is one on its Wednesday, 13:00 to
14:00 UTC, with the test account and a made-up address as guests. Create it in the test account with the test
build:  gcal-uitest insert <account> <calendar> <file>
"""
import json
import sys

from common import at

kind, target = sys.argv[1], sys.argv[2]
if kind == "series":
    event = {
        "summary": "AT series",
        "start": {"dateTime": at(0, "09:00:00"), "timeZone": "UTC"},
        "end": {"dateTime": at(0, "09:30:00"), "timeZone": "UTC"},
        "recurrence": ["RRULE:FREQ=DAILY;COUNT=6"],
    }
elif kind == "plain":
    event = {
        "summary": "AT drag",
        "start": {"dateTime": at(1, "09:00:00"), "timeZone": "Europe/Zurich"},
        "end": {"dateTime": at(1, "10:00:00"), "timeZone": "Europe/Zurich"},
    }
elif kind == "invite":
    # An event with guests: the test account itself, which has not answered, and an
    # address that cannot receive mail. `insert` sends no invitations.
    account = sys.argv[3]
    event = {
        "summary": "AT invite",
        "start": {"dateTime": at(2, "13:00:00"), "timeZone": "UTC"},
        "end": {"dateTime": at(2, "14:00:00"), "timeZone": "UTC"},
        "attendees": [
            {"email": account, "responseStatus": "needsAction"},
            {"email": "nobody@example.com", "displayName": "Nobody There"},
        ],
    }
else:
    sys.exit("usage: fixture.py series|plain|invite <file to write> [<account> for invite]")
json.dump(event, open(target, "w"), indent=1)
