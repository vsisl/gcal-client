"""Makes a data folder with made-up accounts and events, for screenshots.

usage: make_demo_cache.py <new data folder>

Nothing in it is real: two accounts of one invented person (private and work) at
example.com / example.org, a few calendars each, and events in the weeks around today.
No account in it is signed in, so an app started on it shows the events and can change
nothing. Start it without a sign-in key as well, so that it does not even try:

    XDG_DATA_HOME=<folder> XDG_CONFIG_HOME=<an empty folder> gcal-client

The layout written here is the cache's version 4 (crates/store); the app migrates
nothing in it. If the store's layout changes, this script must follow.
"""
import json
import os
import sqlite3
import sys
from datetime import date, datetime, time, timedelta

SCHEMA = """
CREATE TABLE accounts (
    id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE,
    calendar_list_sync_token TEXT, scopes TEXT);
CREATE TABLE calendars (
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    id TEXT NOT NULL, json TEXT NOT NULL, events_sync_token TEXT, shown_override INTEGER,
    PRIMARY KEY (account_id, id));
CREATE TABLE events (
    account_id INTEGER NOT NULL, calendar_id TEXT NOT NULL, id TEXT NOT NULL,
    start_ts INTEGER, end_ts INTEGER, is_master INTEGER NOT NULL,
    recurring_event_id TEXT, json TEXT NOT NULL,
    PRIMARY KEY (account_id, calendar_id, id),
    FOREIGN KEY (account_id, calendar_id)
        REFERENCES calendars(account_id, id) ON DELETE CASCADE);
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE INDEX events_by_time ON events (start_ts, end_ts);
PRAGMA user_version = 4;
"""

HOME, WORK = "anna.rossi@example.com", "anna@studio.example.org"
# (account, calendar id, name, colour, the account's own calendar)
CALENDARS = [
    (HOME, HOME, "Anna Rossi", "#039be5", True),
    (HOME, "family@group.calendar.example.com", "Family", "#33b679", False),
    (HOME, "sport@group.calendar.example.com", "Sport", "#f6bf26", False),
    (WORK, WORK, "Anna Rossi (work)", "#7986cb", True),
    (WORK, "team@group.calendar.example.org", "Design team", "#e67c73", False),
    (WORK, "releases@group.calendar.example.org", "Releases", "#8e24aa", False),
]
FAMILY, SPORT = CALENDARS[1][1], CALENDARS[2][1]
TEAM, RELEASES = CALENDARS[4][1], CALENDARS[5][1]

# One week: (weekday 0-6, start "9:30" or None for all day, hours long or days long,
# calendar, title, place). Repeated for the weeks around today.
WEEK = [
    (0, "9:00", 0.5, TEAM, "Stand-up", ""),
    (0, "10:00", 1.5, WORK, "Sprint planning", "Room Vesuvio"),
    (0, "13:00", 1, WORK, "1:1 with Marco", ""),
    (0, "18:00", 1, SPORT, "Swimming", "City pool"),
    (1, "9:00", 0.5, TEAM, "Stand-up", ""),
    (1, "11:00", 1, WORK, "Design review: onboarding", "Room Etna"),
    (1, "12:30", 1, HOME, "Lunch with Giulia", "Trattoria da Enzo"),
    (1, "15:00", 2, WORK, "Focus: icon set", ""),
    (1, "19:30", 2, FAMILY, "Parents' evening", "School"),
    (2, "9:00", 0.5, TEAM, "Stand-up", ""),
    (2, "10:00", 1, TEAM, "Critique", "Room Vesuvio"),
    (2, "14:00", 1, WORK, "Customer call: Lumen", ""),
    (2, "14:30", 1, HOME, "Dentist", "Via Roma 12"),
    (2, "17:30", 1.5, SPORT, "Climbing", "Boulder hall"),
    (3, "9:00", 0.5, TEAM, "Stand-up", ""),
    (3, "9:30", 2.5, WORK, "Workshop: navigation", "Room Etna"),
    (3, "13:30", 1, WORK, "Interview: product designer", ""),
    (3, "16:00", 1, FAMILY, "Pick up Luca", "Music school"),
    (3, None, 1, RELEASES, "Release 4.2", ""),
    (4, "9:00", 0.5, TEAM, "Stand-up", ""),
    (4, "11:00", 1, TEAM, "Demo", "Room Vesuvio"),
    (4, "12:00", 1, WORK, "Team lunch", ""),
    (4, "15:30", 1, WORK, "Retrospective", "Room Etna"),
    (4, "20:00", 2.5, HOME, "Cinema", "Odeon"),
    (5, "10:00", 2, FAMILY, "Market and brunch", ""),
    (5, None, 2, FAMILY, "Weekend at the lake", "Lago di Garda"),
    (6, "17:00", 1, SPORT, "Run", "River path"),
]


def stamp(moment):
    return moment.astimezone().isoformat(timespec="seconds")


def main():
    if len(sys.argv) != 2:
        sys.exit("usage: make_demo_cache.py <new data folder>")
    folder = os.path.join(os.path.abspath(sys.argv[1]), "gcal-client")
    if os.path.exists(folder):
        sys.exit(f"make_demo_cache: {folder} exists already; choose a new folder")
    os.makedirs(folder)
    db = sqlite3.connect(os.path.join(folder, "cache.db"))
    db.executescript(SCHEMA)
    accounts = {}
    for number, email in enumerate((HOME, WORK), 1):
        db.execute("INSERT INTO accounts (id, email) VALUES (?, ?)", (number, email))
        accounts[email] = number
    owner = {}
    for email, calendar, name, colour, own in CALENDARS:
        owner[calendar] = accounts[email]
        entry = {
            "kind": "calendar#calendarListEntry", "id": calendar, "summary": name,
            "backgroundColor": colour, "foregroundColor": "#000000",
            "accessRole": "owner", "selected": True, "timeZone": "Europe/Rome",
            "defaultReminders": [{"method": "popup", "minutes": 10}],
        }
        if own:
            entry["primary"] = True
        db.execute("INSERT INTO calendars (account_id, id, json) VALUES (?, ?, ?)",
                   (accounts[email], calendar, json.dumps(entry)))

    today = date.today()
    monday = today - timedelta(days=today.weekday())
    count = 0
    for week in range(-5, 6):
        for weekday, clock, length, calendar, title, place in WEEK:
            day = monday + timedelta(weeks=week, days=weekday)
            count += 1
            event = {"kind": "calendar#event", "id": f"demo{count}", "status": "confirmed",
                     "summary": title, "etag": f'"{count}"'}
            if place:
                event["location"] = place
            if clock is None:
                last = day + timedelta(days=length)
                event["start"], event["end"] = {"date": str(day)}, {"date": str(last)}
                start_ts = int(datetime.combine(day, time.min).replace(
                    tzinfo=__import__("datetime").timezone.utc).timestamp())
                end_ts = start_ts + 86400 * length
            else:
                hour, minute = map(int, clock.split(":"))
                start = datetime.combine(day, time(hour, minute)).astimezone()
                end = start + timedelta(hours=length)
                event["start"] = {"dateTime": stamp(start)}
                event["end"] = {"dateTime": stamp(end)}
                start_ts, end_ts = int(start.timestamp()), int(end.timestamp())
            db.execute(
                "INSERT INTO events (account_id, calendar_id, id, start_ts, end_ts,"
                " is_master, json) VALUES (?, ?, ?, ?, ?, 0, ?)",
                (owner[calendar], calendar, event["id"], start_ts, end_ts, json.dumps(event)))
    db.commit()
    db.close()
    print(f"{count} made-up events for {HOME} and {WORK} in {folder}")


if __name__ == "__main__":
    main()
