# Privacy policy

*gcal-client, a desktop client for Google Calendar. Last changed: 19 November 2026.*

gcal-client is a program that runs on your own computer. It has no server of its own and
no account system. Its author never receives any of your data.

## What the app reads and writes at Google

You sign in to your Google accounts in your browser, on Google's own page. The app never
sees your password. For each account it then uses only what you allowed on that page:

| Permission | What the app does with it |
|---|---|
| Calendars and events (`calendar`) | Shows your calendars and events, and saves the changes you make: creating, editing, moving and deleting events, answering invitations, inviting guests, adding a calendar to your list or taking it off. |
| Contacts (`contacts.readonly`) | While you type a name in a guests field or in "Add a calendar", asks Google for the names, e-mail addresses and pictures of matching contacts, to suggest them. For the guests of an event you open, asks for the picture of each guest who is among your contacts, to show it by their name. |
| Other contacts (`contacts.other.readonly`) | Suggestions as above, for people you have exchanged e-mails with but not saved as contacts: their names and e-mail addresses (Google gives no pictures for them). |
| Your organisation's directory (`directory.readonly`) | Suggestions and pictures as for contacts, for colleagues in a company's or school's directory. Never asked for at sign-in; only if you allow it in the settings. |
| Your profile picture (`userinfo.profile`) | Asks Google for the account's own profile picture, to show it by the account. |

The contact and profile permissions are optional: the app works without them, and **Settings →
Permissions…** shows what each account allows.

## What is kept, and where

- **On your computer, in `~/.local/share/gcal-client/cache.db`:** a copy of your calendars
  and events, so that the app starts quickly and can show them without a connection, and
  the app's settings. The file is not encrypted; it is protected like your other files.
- **In your system's keyring:** each account's sign-in (a token from Google), never in a
  plain file.
- **Nowhere on disk:** names, addresses and pictures from your contacts or directory, and
  your accounts' pictures. Names and addresses are shown as suggestions and forgotten;
  only an address you choose becomes part of the event you are editing, at Google.
  Pictures are kept in the app's memory while it runs, so that each is fetched once, and
  are gone when it is closed.

## What is sent, and to whom

The app talks to Google's servers and to nothing else; profile pictures, too, are fetched
from Google's own picture servers. It sends no statistics, no crash
reports and no update checks, and shares nothing with the author or with any third party.
Your data is not used for advertising, not sold, and not used to train any model.

The app's use of information received from Google APIs adheres to the
[Google API Services User Data Policy](https://developers.google.com/terms/api-services-user-data-policy),
including the Limited Use requirements.

## Removing your data

- **Remove account** in the app deletes that account's sign-in from the keyring and its
  copy of your calendars from the computer.
- Deleting `~/.local/share/gcal-client` afterwards removes what is left: the settings
  and the cache's file. Remove the accounts first: the folder alone does not hold the
  sign-ins (they are in the keyring), and without it the app no longer knows which
  ones to delete.
- Your own sign-in key, if you made one, is in `~/.config/gcal-client/client.toml`.
- You can take back the app's access at any time in your Google Account, under
  "Third-party apps & services".

## Questions

Open an issue at <https://github.com/vsisl/gcal-client/issues>.
