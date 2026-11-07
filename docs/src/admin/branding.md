# Branding: your own logos

Administrators can replace the two TeamCraft logos under **Admin → Branding**
(`/admin/branding`):

| Slot | Where it appears | Suggested size |
|------|------------------|----------------|
| **Wide logo** | top left of the web header, the sign-in page, the title bar of the desktop app | about 5:1, for example 600 × 120 px, transparent background |
| **Square logo** | bottom left of the web page (footer), the browser tab icon, the navigation rail of the desktop app | 1:1, for example 256 × 256 px |

Without an upload the built-in TeamCraft logos stay. **Use the built-in logo**
removes an upload again.

## Rules for the files

* PNG, JPEG, WebP or SVG – the type is decided from the content, not from the
  file name.
* At most 512 KiB; bitmaps at most 4096 pixels per side.
* SVG files must not contain scripts, event handlers, animations,
  `foreignObject` or links to other files; they are shown only as images and
  served with a sandboxing `Content-Security-Policy`.

## What else to know

* The logos are stored in the database, so a backup contains them and a
  restore brings them back.
* Every upload and reset is written to the audit log (`branding.set`,
  `branding.reset`).
* The web pages show a new logo at once. The desktop app loads the logos when
  it signs in or starts; sign out and in again to see a change.
* The image routes `GET /branding/wide` and `GET /branding/square` need no
  sign-in (the sign-in page shows the logo); they answer 404 while the built-in
  logo is in use. The REST API reports the state at `GET /api/v1/branding`.
