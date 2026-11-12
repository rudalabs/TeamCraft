# Accessibility

What is checked about the web UI against WCAG 2.2 level AA, how, and what is
still open. Contrast is a test of its own (`domain::skins_meet_contrast`:
every skin, text and controls); this page covers the structure of the pages
and the keyboard.

## Checked by a test on every page

`templates::tests::every_rendered_page_is_accessible` runs over every page
that `every_template_renders` renders – every template in the states that
test covers – and fails on (`src/templates/accessibility.rs`):

| Rule                                                                                                                                                                | WCAG         |
|---------------------------------------------------------------------------------------------------------------------------------------------------------------------|--------------|
| every `input` (except hidden, submit, button, reset), `select` and `textarea` has a label: `<label for>`, an enclosing `<label>`, `aria-label` or `aria-labelledby` | 1.3.1, 4.1.2 |
| every button, link and `input type=submit/button/reset/image` has an accessible name: text, `aria-label`, `aria-labelledby`, `title`, or an image with `alt` inside | 2.4.4, 4.1.2 |
| every `img` has an `alt` (empty for decoration)                                                                                                                     | 1.1.1        |
| no `tabindex` above 0                                                                                                                                               | 2.4.3        |
| `<html lang>` on a full page                                                                                                                                        | 3.1.1        |
| headings do not skip a level on the way down (`h2` → `h4`)                                                                                                          | 1.3.1        |
| every `<th>` has a `scope`                                                                                                                                          | 1.3.1        |
| every `<th>` has a name: text (a visually hidden "Actions" for action columns), `aria-label` or `title`                                                             | 1.3.1        |
| a full page with a `<main>` starts with a skip link: its first link points to the id of `<main>`                                                                    | 2.4.1        |

A rule's own test (`the_checks_find_what_they_promise`) feeds it a page with
one violation of each kind and a page without any. When the test was added
it found: `<th>` without `scope` in 21 templates (now `scope="col"`, all of
them in `<thead>`), two headings that skipped a level (the impact groups on
the revision page and the assistant's answer – each got a visually hidden
`<h2>`), and two file inputs without a label (now `aria-label`). None of the
fixes changes how a page looks.

The two later rules found 17 empty column headers (16 action columns, now
a visually hidden "Actions", and the corner of the inertia tensor on the
mass page, now "Axis") and the missing skip link: every page now starts with *Skip to the main content*, visible only
while it has the focus.

## Keyboard: the main flows

Checked by hand on 2026-11-06 in Chromium (the browser pane of the desktop
app) against a throwaway database, with the keyboard only – Tab, Shift+Tab,
Enter, the arrow keys and Escape. The focus was recorded at every step (`focusin`: element, `:focus-visible`, outline or
shadow).

| Flow                   | Result                                                                                                                                                                                                                                                             |
|------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Sign in                | The username has `autofocus`; Tab to the password, Enter signs in.                                                                                                                                                                                                 |
| Dashboard and header   | Tab order: brand, navigation, search, notifications, account menu, then the page. Every stop has a visible focus (2 px outline, form fields 1 px outline plus a border in the focus colour). Landmarks: `header`, `nav` "Main", the search form, `main`, `footer`. |
| Menus (Admin, account) | Enter opens, Tab and ↑/↓ move inside, Escape closes and returns the focus to the menu button.                                                                                                                                                                      |
| New item               | Every field labelled, order as on screen; Enter in a text field submits; a missing required field moves the focus to it (browser validation). Enter on a `<select>` does not submit – the browser's behaviour, not the page's.                                     |
| Check-in               | "Check in…" (`aria-expanded`) opens the panel and moves the focus to its first field; Escape closes it and returns the focus to the button (**fixed**: Escape did nothing before, see below); Enter checks in.                                                     |
| BOM                    | Revision rule, levels and group are labelled `<select>`s; ↑/↓ only change the choice, Enter or leaving the field applies it (WCAG 3.2.2, `data-autosubmit`); collapse/expand, export menu and the lines are reachable.                                             |
| New change request     | Every field labelled, order as on screen, Enter raises the request.                                                                                                                                                                                                |

Every message after a form (`.flash`) has `role="status"`, an error
`role="alert"`, so screen readers announce it.

**Fixed with this check:** the panels opened by a `data-toggle` button (check-in, master data, lifecycle rules, file
versions, the edit forms of
changes, requests and project groups, acknowledging impacts) did not close
with Escape. Now Escape inside an open panel closes
it, sets `aria-expanded="false"` and returns the focus to the button that
opened it (`static/app.js`); a menu inside a panel closes first.

## Open

Each of these is an entry in `docs/OPEN_WORK.md` (*Clients and packaging*):

- **HTML built by JavaScript** – the assistant sidebar, the BOM tree
  controls, the 3D viewer, the upload progress – is not covered by the
  template test, and the desktop app (`desktop/ui`) not at all.
- **No screen reader run.** The checks above are structural; nobody has
  used the main flows with NVDA, JAWS or VoiceOver yet.
- **Focus of form fields** is a 1 px outline plus a coloured border: enough
  for 2.4.7 (AA), below the 2 px of 2.4.13 (AAA).
