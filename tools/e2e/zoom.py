"""Zoom of the day columns in the test app."""
import json, sys, time
from common import *


def click(role, text):
    call("click_element", elementHandle=[h for h in find(role) if label(h) == text][0]); time.sleep(0.3)
time.sleep(4)
key("s")
for _ in range(3): click("Button", "Zoom out")
shot("zoom-settings")
key(ESC); time.sleep(0.3); shot("zoom-out")
key("s")
for _ in range(6): click("Button", "Zoom in")
key(ESC); time.sleep(0.3); shot("zoom-in")
key("s"); click("Button", "Reset"); key(ESC)
