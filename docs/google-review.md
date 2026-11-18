# Getting the app reviewed by Google

Why: until Google has verified the app, every user has to create a Google Cloud project
and a sign-in key of their own (README, "Your own Google sign-in key"), and Google ends
each sign-in after seven days. After verification the project ships one key, and a user
only signs in.

This file is the owner's checklist. What is written here about Google's rules comes from
Google's pages as read in November 2026; the Cloud Console says what is asked for at the
time, and it has the last word.

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
- [ ] **Prove the homepage is yours** in Google Search Console (add the address as a
      "URL prefix" property and place the file or tag it gives you in `docs/`).
      If Google does not accept an address under `github.io` as the app's domain, a
      domain of your own is needed, pointed at the same pages.
- [ ] In the Cloud Console, under **Google Auth Platform → Branding**: the app's name,
      a support e-mail address, the icon (`packaging/io.github.vsisl.gcal_client.png`,
      enlarged to what is asked for), the homepage, the privacy policy, and the
      authorised domain.
- [ ] Under **Data access**: the four scopes above, each with its reason.
- [ ] Under **Audience**: publish the app ("In production"). This also ends the
      seven-day limit on sign-ins for the scopes that need no review.
- [ ] **Record a short video** (Google asks for one for sensitive scopes): the sign-in
      from the app with Google's consent page and its address bar in view, then each
      permission in use: events shown and changed; a guest suggested from the contacts;
      one from other contacts; the directory allowed in the settings and a colleague
      suggested. Unlisted on YouTube is enough.
- [ ] **Submit for verification** and answer Google's questions by e-mail. It commonly
      takes some weeks and a few rounds.

After verification:

- [ ] Put the verified key into the releases. Google's terms do not let it be written
      into the public source, so the release workflow is to take it from the
      repository's secrets and build it into the AppImage; a key in
      `~/.config/gcal-client/client.toml` keeps taking precedence, for developers.
      (Not built yet.)
- [ ] Take the "own key" section out of the README's installation steps.
