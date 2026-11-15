"""Quick create and its undo in the test app, checked against Google."""
import json, sys, time
from common import *


time.sleep(4)
key("c")
for ch in "AT quick": key(ch)
time.sleep(0.3); shot("quick-card")
key(ENTER)
wait("created", lambda: len(named("AT quick")) == 1)
wait("shown in the week (scrolled to it)", lambda: len(items("AT quick")) == 1)
time.sleep(3); shot("quick-created")
print("   undo offered:", "Undo" in [label(b) for b in find("Button")])
key("z")
wait("undo create: gone", lambda: not named("AT quick"))
