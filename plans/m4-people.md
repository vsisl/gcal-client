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
4. **Guests in the editor**: a field of e-mail addresses; inviting on a new event,
   adding and removing on an existing one; the organiser joins the first guests.
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

## Left

- **Contact autocomplete** for the guests field: needs Google's People API and a further
  permission at sign-in (every signed-in account has to agree again). Decide with the
  owner before adding a scope.
- **Optional guests**, and a note with an answer.
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
- The guests field takes addresses only; names are shown in the details card.
