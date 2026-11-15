"""Makes a data folder for the end-to-end scripts that holds one account only.

usage: make_test_cache.py <test account's e-mail> <new data folder>

Copies the app's real cache and deletes every other account's rows from the copy, so
that an app started with XDG_DATA_HOME pointing at the new folder knows the test account
and nothing else: whatever a script or a bug does, it cannot reach another calendar.
The real cache is only read. The account must have been signed in with the app before
(its sign-in stays in the system keyring, which the copy shares).
"""
import os
import sqlite3
import sys

email, folder = sys.argv[1], sys.argv[2]
source = os.path.join(
    os.environ.get("XDG_DATA_HOME", os.path.expanduser("~/.local/share")),
    "gcal-client", "cache.db")
target_dir = os.path.join(folder, "gcal-client")
target = os.path.join(target_dir, "cache.db")
if os.path.exists(target):
    sys.exit(f"{target} exists already; choose a new folder")
os.makedirs(target_dir)

real = sqlite3.connect(f"file:{source}?mode=ro", uri=True)
copy = sqlite3.connect(target)
real.backup(copy)
row = copy.execute("SELECT id FROM accounts WHERE email = ?", (email,)).fetchone()
if row is None:
    copy.close()
    os.remove(target)
    sys.exit(f"{email} is not signed in to the app; add it there first")
copy.execute("DELETE FROM events WHERE account_id != ?", row)
copy.execute("DELETE FROM calendars WHERE account_id != ?", row)
copy.execute("DELETE FROM accounts WHERE id != ?", row)
copy.execute("DELETE FROM settings")
copy.commit()
copy.execute("VACUUM")
accounts = [email for (email,) in copy.execute("SELECT email FROM accounts")]
print(f"{target}: accounts {accounts}")
