"""Writes the event the end-to-end scripts start from, dated in the current week.

usage: fixture.py series|plain <file to write>

The app opens on the week with today in it, so the fixtures are made for that week:
"series" is a daily event from its Monday, six times, 09:00 to 09:30 UTC; "plain" is one
event on its Tuesday, 09:00 to 10:00 UTC. Create it in the test account with the test
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
else:
    sys.exit("usage: fixture.py series|plain <file to write>")
json.dump(event, open(target, "w"), indent=1)
