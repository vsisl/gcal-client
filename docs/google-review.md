# Getting the app reviewed by Google

Why: until Google has verified the app, every user has to create a Google Cloud project
and a sign-in key of their own (README, "Your own Google sign-in key"), and Google ends
each sign-in after seven days. After verification the project ships one key, and a user
only signs in.

This file is the owner's checklist. What is written here about Google's rules comes from
Google's pages as read in November 2026; the Cloud Console says what is asked for at the
time, and it has the last word.

## Where it stands (2026-11-18, evening)

Everything is prepared; nothing has been sent to Google's reviewers yet. One automatic
check stands in the way, and it has a waiting time.

**Next, from the afternoon of 2026-11-19:**

1. Open the Cloud Console, project `gcal-client-dev`: **Google Auth Platform →
   Branding**, the "Information and summary" icon at the top right, **Verify branding**.
   It takes up to five minutes.
   - It passed: go on with step 2.
   - "The website of your home page URL … is not registered to you" again: the waiting
     time is not over, or Google does not take a `github.io` address. Wait another day
     once; if it still fails, choose "I believe the issues found are incorrect" in the
     issues dialog and say that both `https://vsisl.github.io/` and
     `https://vsisl.github.io/gcal-client/` are verified properties of this Google
     account in Search Console. The last resort is a domain of the owner's own, pointed
     at the same pages.
2. **Verification Center → Prepare for verification** (greyed out until step 1 has
   passed). It asks for:
   - the reason for each scope: the texts in the table below;
   - for `directory.readonly`, add: "It only has an effect for Google Workspace
     accounts and is off unless the user turns it on in Settings → Permissions. The
     video shows that page; the account in the video is not a Workspace account.";
   - the demo video: the unlisted YouTube link, which the owner has. It is not written
     here: this file is public, and the video shows a test account's address.
3. **Submit**, and answer Google's questions by e-mail.

If Google asks to see the directory in use, record a second clip with a Workspace
account (see "The video" below for how the first was made).

## What Google will look at

The app asks for four permissions, all of the kind Google calls **sensitive** (none is
"restricted", so no paid security assessment is needed):

| Scope | Why the app needs it (the text to give Google) |
|---|---|
| `https://www.googleapis.com/auth/calendar` | The app is a full calendar client: it shows the user's calendars and events and lets them create, edit, move and delete events, answer invitations, invite guests and add calendars to their list. Read-only or events-only scopes would not allow this. |
| `https://www.googleapis.com/auth/contacts.readonly` | When the user types a name in an event's guests field, the app suggests matching contacts (name and e-mail address). Nothing is stored. |
| `https://www.googleapis.com/auth/contacts.other.readonly` | The same suggestions for people the user has corresponded with but not saved as contacts, as Google Calendar's own guests field offers. Nothing is stored. |
| `https://www.googleapis.com/auth/directory.readonly` | The same suggestions for colleagues in a Workspace organisation's directory. Asked for only when the user chooses it in the settings. Nothing is stored. |

## Checklist

Done in the repository:

- [x] A privacy policy: [privacy.md](privacy.md).
- [x] A homepage: [index.md](index.md) (to be served by GitHub Pages, below).
- [x] The app shows what each permission is for (Settings → Permissions…), and works
      without the optional ones.
- [x] An icon of the app's own, without anything of Google's.

For the owner:

- [x] **The app's name**: `gcal-client` stays, decided by the owner on 2026-11-18,
      unless Google refuses it. Google's branding rules do not allow a name that
      contains a Google product name or could be taken for one, and "gcal" may be
      judged too close; if so, the name changes then (it is on the consent page, in the
      launcher and in the AppImage's file name) and the app is submitted again.
- [x] **The repository is public** (2026-11-18) and GitHub Pages serves the `docs`
      folder: the homepage is `https://vsisl.github.io/gcal-client/` and the policy
      `https://vsisl.github.io/gcal-client/privacy`. Until the going-public pull request
      is merged, Pages is set to the branch `release-appimage`; after the merge it must
      be switched to `main` (Settings → Pages), or the site goes away with the branch.
- [x] **The homepage is proved to be the owner's** in Google Search Console
      (2026-11-18): the address is a "URL prefix" property, verified by the file
      `docs/google7d20fb683ac7b8d5.html`, which must stay. The whole site
      `https://vsisl.github.io/` is verified too, by the same file in the repository
      `vsisl.github.io`, because the authorised domain is the site and not the path.
- [ ] **Verify the branding** (Branding → the summary panel → "Verify branding").
      Tried on 2026-11-18 two hours after the proof of ownership: refused with "the
      website of your home page URL is not registered to you … wait 24 hours". To be
      tried again from 2026-11-19 afternoon. The scopes cannot be submitted before it
      passes. See "Where it stands" above for what to do with either answer.
- [x] In the Cloud Console (project `gcal-client-dev`), under **Google Auth Platform →
      Branding**: the app's name, the support e-mail address, the icon (120 px), the
      homepage, the privacy policy, and the authorised domain `vsisl.github.io`.
- [x] Under **Data access**: the four scopes above are registered. Their reasons are
      asked for when the app is submitted.
- [x] Under **Audience**: the app is published ("In production", 2026-11-18). Sign-ins
      no longer end after seven days. Until Google has verified the app, a sign-in
      shows an "unverified app" warning and at most 100 users can ever sign in.
- [x] **The video** is recorded and uploaded to YouTube, unlisted (2026-11-18). It
      shows: the sign-in from the app with Google's consent page in English and its
      address bar in view; an event created, moved and deleted; a guest suggested from
      the contacts; one from the other contacts; the Settings → Permissions page. The
      directory is on that page only: it is not shown in use.
- [ ] **Submit for verification** and answer Google's questions by e-mail. It commonly
      takes some weeks and a few rounds.

## The video

Made with one of the owner's test accounts, so that nothing real is on screen. For a
retake:

- The account needs a saved contact and an entry under "Other contacts" whose first
  letters the script can type, and its consent page must come up in English (one of the
  two test accounts shows it in Czech whatever its language setting says).
- A test build (AGENTS.md, "Driving the window") is started on a new, empty data folder,
  so that the window opens without an account and the sign-in is part of the video.
- A script presses "Add account"; the owner answers in the browser, pausing on the
  consent page, and then clicks the app window's title bar once (the app takes keys only
  while its window is the active one). The script then creates an event named
  "AT demo event", moves it, deletes it, types the contacts' letters into the guests
  field of a new event, closes it with Cancel, and opens Settings → Permissions.
- The owner records the screen (Ctrl+Shift+Alt+R) and uploads the file.

The script is not in the repository: it was written for the one take and types letters
that fit one account's contacts.

After verification:

- [ ] Put the verified key into the releases. Google's terms do not let it be written
      into the public source, so the release workflow is to take it from the
      repository's secrets and build it into the AppImage; a key in
      `~/.config/gcal-client/client.toml` keeps taking precedence, for developers.
      (Not built yet.)
- [ ] Take the "own key" section out of the README's installation steps.
