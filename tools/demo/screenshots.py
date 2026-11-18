"""Takes the README's screenshots from the made-up data of make_demo_cache.py.

usage: screenshots.py <folder for the pictures> <the demo folder's cache.db>

Needs a test build (AGENTS.md, "Driving the window") running on the demo folder, without
a sign-in key and without its first sync, so that nothing complains that the made-up
accounts are signed in nowhere:

    GCAL_UITEST_NO_SYNC=1 SLINT_MCP_PORT=8766 XDG_DATA_HOME=<demo folder> \
        XDG_CONFIG_HOME=<an empty folder> <scratch folder>/gcal-uitest

Changes nothing: what it opens it closes with Escape. The README's pictures are in
docs/screenshots.
"""
import sys
import time

sys.path.insert(0, __file__.rsplit("/", 2)[0] + "/e2e")
from common import *  # noqa: E402


def press(role, text):
    found = [h for h in find(role) if label(h).startswith(text)]
    if not found:
        sys.exit(f"screenshots: nothing on screen is called {text!r}")
    call("click_element", elementHandle=found[0])
    time.sleep(0.8)


time.sleep(1)
key("w")
shot("week")
key("m")
shot("month")
key("w")
press("ListItem", "Design review")
shot("details")
key("e")
time.sleep(0.8)
shot("editor")
key(ESC)
key(ESC)
print("week, month, details, editor")
