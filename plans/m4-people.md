# M4 (people) — task plan

From the design (`plans/i-want-to-create-deep-hamster.md`): guests, RSVP, guest
permissions, contact autocomplete, "find a time" from free/busy, Meet links, rooms,
attachment links.

## Done (branch `m4-people`), each tested against Google with the test account

1. **Guests in the details card**: names, answers, organiser, optional; "Join the video
   meeting". (`tools/e2e/rsvp.py`)
2. **Answer an invitation**: Yes / No / Maybe, with undo, in the account's own calendar
   only. For an event of a series it asks "this event / all events"
   (`tools/e2e/rsvp_series.py`); an event already answered or changed on its own is
   answered without the question. Google gives the web a third choice, "this and
   following events"; not offered here.
3. **Change events with guests** (drag, resize, editor), for the account that invited
   them; asks whether to send e-mails. A guest's own copy stays read-only.
4. **Guests in the editor**: a field for addresses and, under it, the list of guests,
   each with "optional" and a remove button; inviting on a new event, adding and
   removing on an existing one; the organiser joins the first guests.
   (`tools/e2e/guests.py`)
5. **Google Meet** from the editor: create, keep, remove. (`tools/e2e/meet.py`)
6. **What guests may do**, in the editor: change the event, invite others, see the
   guest list. Who may change the event may do the other two, as on the web. A new
   event says nothing about them unless they differ from Google's defaults.
   (`tools/e2e/guests.py`)

Sending e-mails has not been tried by the scripts on purpose: their guests are made-up
addresses, and every question about e-mails is answered "no".

7. **Attachments** in the details card: each file a link that opens in the browser.
   Only web addresses are offered, and only web addresses are ever handed to the
   desktop to open (also the event's own link and the video meeting's).
8. **Notifications** in the editor (the web's word; they were "Reminder", one per
   event): the calendar's usual ones, or up to five of the event's own, each with a
   time from a list. Those by e-mail or at another time are kept and shown, but the
   editor cannot make them. (`tools/e2e/ed.py`)

9. **Suggestions for the guests field** from the account's contacts, the people it has
   written to and, if allowed, its organisation's directory (Google's People API).
   Every sign-in asks for the first two (read-only; each can be left unticked on
   Google's page). The directory is asked for only from the settings: it covers every
   profile in the organisation. Tried by the owner by hand (2026-11-16); the test
   accounts have no contacts, so no script covers Google's answers, only the
   mock-server tests do. Not done: choosing a suggestion with the arrow keys (Enter
   takes the first).
10. **Permissions in the settings**: for each account what it allows (Google says so
    with every access token; the cache keeps it), what each permission is for and what
    it exposes, and "Allow" for a missing one: a sign-in for that account which keeps
    what it gave before. A sign-in that comes back as another account changes nothing.
    (`tools/e2e/perms.py` reads the list; pressing "Allow" needs the owner in the
    browser and was not tried by a script.)

## Left

- A note with an answer to an invitation.
- **Find a time**: other people's free/busy (`freebusy.query`) beside the event in the
  editor.
- **Rooms**: adding a room by name or address (a room is a guest with `resource`).
- **Adding attachments** in the editor (Google takes links to Drive files; writes then
  need `supportsAttachments=true`).
- A guest's copy of an event: "propose a new time" has no API and goes to the browser.

## Known rough edges

- A guest's own copy of an event stays read-only here, also when the organiser lets
  guests change it. Tried with the two test accounts (2026-11-16): a guest's change
  through the guest's own calendar is saved in that copy alone and never reaches the
  organiser (not after minutes, not with `sendUpdates=all`); sent to the organiser's
  calendar it is refused as "not found". The web can do it; the public API, as far as
  these tries show, cannot. The details card's link opens the event on the web.
  (Issue 17.)

- After a change to a video meeting the app fetches the event again three seconds
  later, which closes an open details card; within those seconds another change to the
  event may be refused as "changed somewhere else".
- Without the suggestions the guests field takes addresses only; names are shown in
  the details card.
- A notification's time is one of ten in a list, not a number typed in, and it is
  always on the screen, not by e-mail.
