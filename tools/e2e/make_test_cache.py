"""Makes a data folder for the end-to-end scripts that holds one account only.

usage: make_test_cache.py <test account's e-mail> <new data folder>

Copies the app's real cache and deletes every other account's rows from the copy, so
that an app started with XDG_DATA_HOME pointing at the new folder knows the test account
and no other. The real cache is only read. The account must have been signed in with
the app before (its sign-in stays in the system keyring, which the copy shares).

What this does and does not protect:
- The app in that folder cannot use another account's sign-in.
- It CAN reach every calendar the test account itself has access to. The test account
  must therefore be one made for testing, with no real calendar shared to it with
  write access; the calendars it has are listed at the end, with their access.
- The keyring is shared with the real app: removing an account in the test app (or
  `logout`) removes the real sign-in. Do not test account removal this way.

The copy is prepared in a temporary folder and only moved into place once it is known to
hold the test account alone, so a failure half-way never leaves a usable folder with
other accounts in it. Exits with an error, and without the folder, when anything is not
as expected.
"""
import os
import shutil
import sqlite3
import sys
import tempfile

# The cache's layout this script knows: its version and every table. With another
# version there may be tables holding other accounts' data that would not be cleaned.
KNOWN_VERSION = 3
KNOWN_TABLES = {"accounts", "calendars", "events", "settings"}


def fail(message):
    sys.exit(f"make_test_cache: {message}")


def main():
    if len(sys.argv) != 3:
        fail("usage: make_test_cache.py <test account's e-mail> <new data folder>")
    email, folder = sys.argv[1], os.path.abspath(sys.argv[2])
    source = os.path.join(
        os.environ.get("XDG_DATA_HOME", os.path.expanduser("~/.local/share")),
        "gcal-client", "cache.db")
    if os.path.exists(folder):
        fail(f"{folder} exists already; choose a new folder")
    if not os.path.exists(source):
        fail(f"there is no cache at {source}")

    parent = os.path.dirname(folder)
    os.makedirs(parent, exist_ok=True)
    staging = tempfile.mkdtemp(prefix=".make-test-cache-", dir=parent)
    try:
        os.makedirs(os.path.join(staging, "gcal-client"))
        target = os.path.join(staging, "gcal-client", "cache.db")
        real = sqlite3.connect(f"file:{source}?mode=ro", uri=True)
        copy = sqlite3.connect(target)
        try:
            real.backup(copy)
        finally:
            real.close()

        version = copy.execute("PRAGMA user_version").fetchone()[0]
        tables = {name for (name,) in copy.execute(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")}
        if version != KNOWN_VERSION or tables != KNOWN_TABLES:
            fail(f"the cache is version {version} with tables {sorted(tables)}; this script "
                 f"knows version {KNOWN_VERSION} with {sorted(KNOWN_TABLES)}. Update it "
                 "before using it, so that no table with other accounts' data is missed")
        row = copy.execute("SELECT id FROM accounts WHERE email = ?", (email,)).fetchone()
        if row is None:
            fail(f"{email} is not signed in to the app; add it there first")

        copy.execute("DELETE FROM events WHERE account_id != ?", row)
        copy.execute("DELETE FROM calendars WHERE account_id != ?", row)
        copy.execute("DELETE FROM accounts WHERE id != ?", row)
        copy.execute("DELETE FROM settings")
        copy.commit()
        # Deleted rows stay readable in the file until it is rewritten.
        copy.execute("VACUUM")

        accounts = [address for (address,) in copy.execute("SELECT email FROM accounts")]
        strangers = copy.execute(
            "SELECT (SELECT COUNT(*) FROM events WHERE account_id != ?1)"
            " + (SELECT COUNT(*) FROM calendars WHERE account_id != ?1)", row).fetchone()[0]
        if accounts != [email] or strangers:
            fail("the copy still holds other accounts' data; nothing was created")
        calendars = copy.execute(
            "SELECT id, json_extract(json, '$.accessRole') FROM calendars").fetchall()
        copy.close()
        for leftover in (target + "-wal", target + "-shm"):
            if os.path.exists(leftover):
                os.remove(leftover)
        os.rename(staging, folder)
    finally:
        # Still there only if something failed before the move.
        shutil.rmtree(staging, ignore_errors=True)

    print(f"{folder}: the only account is {email}")
    print("Calendars this account can reach (make sure none of them is a real one it may write to):")
    for calendar, access in calendars:
        print(f"  {access or '?':<16} {calendar}")


main()
