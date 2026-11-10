# Desktop app

TeamCraft Desktop (`desktop/`, Tauri 2) is a rich client for Linux, macOS and
Windows: a navigator tree of the project groups, object tabs (overview,
files, structure, where used, references, graph, changes, history, 3D viewer), a worklist with
check-outs and reviews, and a **local working folder**:

* *New* (`Ctrl/Cmd+N`) creates an item together with its files: choose the
  CAD files first, name and type follow from the first one;
* *Check out* takes the lock, copies the files into the working folder and
  makes them writable. For an assembly it also downloads every part of its
  structure that is missing (latest version, read-only) – only the assembly is
  locked; parts you changed locally, or whose place is taken by another file,
  are never overwritten and are listed in the report (*Settings → Workspace*
  switches this off);
* *All items / Structure* in the navigator head: *Structure* shows, per project
  group, the assemblies and the parts no BOM uses; an assembly expands into its
  parts one level at a time with quantity, state, lock owner and ⧉ N for a part
  used in several assemblies;
* *Get…* on a revision copies the item **with everything it consists of** –
  its BOM resolved with a revision rule and the parts its CAD files
  reference – so the assembly opens in the CAD program (see below);
* the download icon next to *Projects* in the navigator (*Tools → Get Project Groups…*) copies the files of every item
  of
  chosen project groups into the working folder – for new team members;
* the folder icon next to it (*Tools → Import CAD Folder…*) brings an existing CAD folder in:
  scan first (nothing is uploaded), then one item per part or assembly with
  its drawings and exports; every file keeps its folder, so the working
  folder later looks like the original tree. An interrupted import continues
  where it stopped – an item whose files did not arrive is completed, none is
  created twice; the app remembers per project group what it imported, so
  the same folder can also be imported into another group. BOM lines come
  from the references in the assembly files – for NX also from `.prt` files (such an assembly is imported as a part
  item, NX does not tell parts and
  assemblies apart by the file type). *Help → How to
  Import a CAD Folder* is the step-by-step tutorial (Pack and Go, unique file
  names, check and release);
* *Import BOM…* (in the *Structure* tab and the *More* menu of a revision you
  have checked out, or *Tools → Import BOM as New Assembly…*) reads a BOM
  exported by the CAD program – CSV, Excel, ODS or a STEP assembly – with the
  same column recognition as the web import: the preview shows the lines,
  how many items are new or existing and the notes (for example a quantity
  that is not a number); *Import* adds the lines in one step, creating the
  missing items. *Part numbers from the file* or *New numbers* (the group's
  numbering scheme), and whether items with the same number are reused, are
  chosen in the dialog;
* the *Graph* tab draws a navigable reference and BOM graph around the
  revision: the focused item in the middle, its BOM children and references
  below, the assemblies that use it above. Node colour is the lifecycle state,
  node size (and the ⚙ badge) the item's own BOM complexity. Click a node to
  follow it – an assembly re-centres the graph, anything else opens its
  overview – and a back control stays in the same place (left of the tab row)
  so the graph is one click away even from a part's page; the mouse wheel
  zooms on the cursor, dragging pans and a double-click on the background
  fits the graph again. Large assemblies are laid out in rows, and only the
  first 48 neighbours get a preview and their complexity;
* the *Workspace* tab shows per file where it is, whether it is up to date,
  modified, outdated, in conflict, new, missing or a name clash, whether it
  is read-only, and who holds the lock for how long; opening an outdated
  file or one somebody else has checked out asks first;
* *Check in…* uploads the selected files as new versions and releases the
  lock – based on the versions they were copied from, so a version somebody
  else checked in meanwhile is never overwritten (see
  [Concurrency](vault.md#concurrency-nothing-is-overwritten-silently)) –
  and makes the files read-only again; files above 32 MiB are sent in
  resumable chunks – a dropped connection resumes where it stopped (see
  [Large files](vault.md#large-files-chunked-resumable-uploads)). The dialog
  stays open with the progress while the check-in runs, and **Cancel** stops
  it at any point until the last byte is sent: before the next STEP export,
  between two chunks of a large file (the chunks on the server are
  discarded – a cancelled upload starts over next time) or half-way through
  the request with the small files. Nothing is stored then, the revision
  stays checked out to you and your local files are untouched. Once every
  byte is sent the server finishes the check-in on its own; the dialog says
  so (*too late to cancel*);
* *Check in all* in the worklist checks in every check-out without local
  changes and lists those that still have changes;
* *Dependents to check* (worklist and home) shows one line per dependent
  revision with the number of changes it has to be checked against; the
  arrow lists them (a click opens the item that changed). On the home page
  every dependent has **Open**, **Revise** and **Acknowledge all…** (one note
  for all of them, see [Change propagation](structure.md#change-propagation))
  and an **Acknowledge…** per change;
* *Request change…* on a revision tab and *Request…* on a file (Files and
  Workspace tabs) raise a [change request](changes.md) with the item,
  revision and file name filled in and an optional screenshot or PDF; the
  worklist lists the requests assigned to you and your open ones;
* on Windows with Inventor or Solid Edge installed, *STEP* (workspace) and
  the check-in option create STEP exports for the 3D preview with that
  program; the Files tab shows the preview image stored in native files.
* *Agent* (rail, *View → Agent*, `Ctrl/Cmd+4`, or *Agent* in a revision or
  change tab) is the app's one place for AI. It runs on one of two
  backends, decided by the server's policy and your choice:
    * **Company AI** – the server's assistant, configured by the
      administrator (the key stays on the server). It is the
      [Ask TeamCraft](assistant.md) conversation of the web UI, inline in the
      Agent tab: it knows the last revision (and its section, BOM with
      revision rule), change order or change request tab you opened, offers
      quick actions that fit it (*Draft release notes*, *Check before
      release*, *Brief me for this review* …), has the *Allow drafting*
      checkbox, *Cancel* and *New conversation*, and keeps one conversation on
      the server that survives restarts.
    * **Local agent** – an AI agent on your computer that you set up under *Settings → Agents…*, for example Claude Code
      through its ACP adapter
      or Gemini CLI. None is set up by default. The app starts the agent in
      the workspace folder and gives it TeamCraft's tools with your rights.
      The conversation streams in the tab, with tool calls, the agent's plan,
      permission prompts (*Allow once* / *Reject*), *Cancel* and quick
      prompts (release notes, release check, change review, worklist triage).
      Through the app the agent may only read text files in the workspace
      folder; it gets no write access and no terminal.

  When the server offers both, *Company AI* / *Local agent* in the Agent
  toolbar switches between them (each keeps its own conversation); the app
  remembers the choice per server and user, and starts with the local agent
  if you have set one up, otherwise with the company AI. If the
  administrator turned local agents off (`TEAMCRAFT_LOCAL_AGENTS=off`), the
  Agent view runs on the company AI only, the app ends a running agent and
  refuses to start one; with no company AI configured either, the view
  says that AI is switched off by the administrator. The app asks the
  server again when the Agent view opens, every minute and before every
  message to a local agent, so a changed setting takes effect without a
  restart. See
  [ACP agents](assistant.md#acp-agents-in-the-desktop-app).

Sign-in runs through the browser (single sign-on or password, confirmed on a
TeamCraft page) or with a personal access token. The app keeps the token in
the system keychain (Windows Credential Manager, macOS Keychain, GNOME Keyring
or KWallet on Linux). Where there is none, it uses a file only your user
account can read and says so at the start and in *Settings*. An address with
plain `http://` to another computer gets a warning when you connect: the token
and all data would cross the network unencrypted – use the `https://` address
of the server. The app uses the same skins
and 3D viewer as the web interface; the models are converted by the server. *Settings → Appearance*: *TeamCraft* (the
default) follows the light or dark
appearance of the operating system and switches with it, *TeamCraft Light*
and *TeamCraft Dark* stay fixed, the distribution skins are dark. *Settings → Change password…* changes the password of
a local account (it
asks for the current one; wrong ones count like failed sign-ins, and your web
sessions end); single-sign-on accounts do not see it.

The window is built for daily work: object tabs on top, under them the
toolbar of the open object with its one next step filled (see
[The toolbar](#the-toolbar)) and the rare tools under *More*. The navigator shows a state as its symbol (a check:
released, a ring: in review, a half-filled ring: in work, – obsolete, the
name in the tooltip); the search field is `Ctrl+F` (`⌘F`).

## The toolbar

The toolbar sits right below the tab strip and acts on the active tab. Its
buttons are grouped, and they keep their place whatever state the object is
in; a button that does not apply right now is greyed out, and its tooltip
says why (for example *Check out first* or *Already in change order
CHG-000002*). The **one highlighted button is the most likely next step**:

| Revision                                                                    | Highlighted                                                 |
|-----------------------------------------------------------------------------|-------------------------------------------------------------|
| in work, not checked out                                                    | *Check out*                                                 |
| checked out by you, with local changes (or the workspace could not be read) | *Check in…*                                                 |
| checked out by you, no local changes                                        | *Release…* (when it can be released, otherwise *Check in…*) |
| in review, obsolete, or checked out by somebody else                        | nothing                                                     |
| released                                                                    | *Revise*                                                    |

The working folder of your check-out is looked at again whenever the tab is
shown or the app window gets the focus back – save in the CAD program, switch
back, and *Check in…* is highlighted.

* revision tab: **Lock** (*Check out*, or *Check in…* and *Cancel check-out*
  / *Force unlock* while checked out) · **Work** (*Edit*, *Get…*, *Open
  folder*) · **Lifecycle** (*Release…*, *Revise*) · **Collaborate**
  (*Request change…*, *Watch*, *Agent*);
* change tab: **Review** (*Approve change* highlighted, *Reject* – the comment field
  of *Your review* goes with it, required for a rejection) while your review
  is pending · **Lifecycle** (*Submit for review*, highlighted on your draft, *Withdraw*) · **Collaborate**;
* change request tab: **Triage** (*Start triage* or *Create change order*
  highlighted, *Take up again*) · **Requester** (*Attach…*, *Withdraw*) · **Collaborate**.

Rare actions are in **More ▾** at the end: *Pack and Go…*, *Open in browser*, *Refresh* (`F5`) and *Mark obsolete…*. The
menu works with the keyboard:
`Enter`, `Space` or `↓` opens it, `↑`/`↓`, `Home`/`End` move, `Enter` runs
the action, `Esc` closes it and returns to the button; a click outside closes
it too.

The **Overview** of a revision shows *At a glance* next to the properties:
the preview image stored in an Inventor, Solid Edge, SolidWorks or NX file (with the way into the 3D viewer) and the
number of files, BOM lines, where
used, references, change orders and open change requests – a click opens the
matching sub-tab.

*Home*, *Refresh* and the dialogs that offer project groups (*New*, *Get
Project Groups…*, *Import CAD Folder…*) load the groups and your memberships
from the server again, so a group created or joined in the web UI shows
without restarting the app.

Keyboard: `Ctrl/Cmd+N` new item, `Ctrl/Cmd+F` search, `F5` refresh,
`Ctrl/Cmd+1/2/3/4` Home, Worklist, Changes, Agent, `Ctrl/Cmd+W` close the tab,
`Ctrl/Cmd` + `+` / `-` / `0` (or `Ctrl` + mouse wheel) zoom.

## How files appear on your computer

There is **one working folder per project group**, and inside it the files
sit in the **same folders as in the original CAD tree** – one copy of each
file. The revision is information TeamCraft keeps about the file, not a
folder level:

```text
Documents/TeamCraft/                     ← the workspace (Settings → Workspace)
├── ROVER/                               ← project group
│   ├── chassis/
│   │   ├── chassis.asm                  ← ASM-000001 rev. B, checked out by you (writable)
│   │   └── brackets/
│   │       ├── bracket.prt              ← PRT-000001 rev. A (read-only)
│   │       └── bracket.step             ← new STEP export next to it
│   └── drawings/
│       └── chassis.drw
├── LANDER/
│   └── …
└── .teamcraft/
    ├── state.json                       ← per file: item, revision, file id, version, SHA-256, checked out
    └── migration.log                    ← what the move from the old layout did
```

The folder of a file is stored with it on the server (*Folder* on the
revision page, `folder` in the API). The folder import sets it from the
scanned tree; uploads without a folder put new files at the top of the
group, and a file keeps its folder when new versions arrive. While a
revision is checked out, the revision page can move a file to another
folder.

**Read-only rule.** Files you have **not** checked out are read-only on disk (the read-only attribute on Windows, no
write permission on Linux and macOS).
CAD programs then open them read-only, and nobody edits a file that cannot be
checked in. *Check out* makes the files of the revision writable; *Check in*
uploads the changed ones and makes them read-only again; *Cancel check-out*
does the same without uploading. Files you changed are never made read-only
or overwritten by TeamCraft.

**Check-out of an assembly.** The lock is taken on the assembly only; its
structure is then fetched like *Get* does under the *latest* rule, in the same
progress dialog. Per file: missing – downloaded read-only; up to date –
skipped; an unchanged older copy – updated; changed here, checked out by you,
or a different file at the same place – kept untouched and listed (*kept – your
local changes*, *name clash*). Cancelling leaves no partial file.
**Add files.** *Add files…* (Files section and *More ▾* of a revision) takes
files from a file picker, checks the revision out first if you have not,
and **copies** them into its working folder – the originals are never moved
or changed. New files go into the revision's main folder or a sub-folder of
it. A file with the name of an existing file becomes its next version and
asks first (unticked files are not added). The copies appear as *new*
files in *Check in…* – also files with an unrelated name, which TeamCraft
remembers in `.teamcraft/added.json` until they are checked in – or tick *Check in now*. The Files section explains why
the button is disabled:
checked out by someone else (add after check-in), in review, or released (frozen: *Revise* and add the files to the new
revision). Dropping files
onto the window is not supported yet.

**Get.** *Get…* on a revision resolves its structure on the server – the
multi-level BOM with the revision rule you choose (*latest*: the newest
revision of every part, also in work; *released*: the newest released
revision) plus the items whose files its CAD files reference (an assembly
names its parts, a drawing its model) – and downloads **only what is
missing or outdated**, compared by SHA-256. Every file is checked against
its SHA-256 after the download and recorded at once, so a Get that was
cancelled or interrupted continues where it stopped – also inside a large file:
the bytes already received stay next to the file (`<name>.<sha>.teamcraft-download`, ignored by the working folder), the
next
Get asks the server only for the rest and checks the SHA-256 of the whole
file before it puts it in place. A file that changed on the server
meanwhile is downloaded completely again. The status bar shows
the progress with a *Cancel* button.

**Nothing is overwritten.**

* A file you changed locally is kept (and listed) when the server has a
  newer version – *conflict* in the Workspace tab.
* Two items with a file of the same folder and name (`chassis/bolt.prt` in
  two items) cannot share one place: the second is reported as a **name
  clash** and not downloaded. Give one of them another folder on the
  revision page.
* A file in the way that did not come from TeamCraft is reported the same
  way.

**Why not the browser?** A browser download saves single files into the
Downloads folder: no folder structure, so the CAD program cannot find the
parts of an assembly; no record of which version a file is, so changes
cannot be checked in safely; and no read-only protection. Browser downloads
are fine for one file or for looking at something – CAD work goes through
the desktop working folder. *Pack and Go* (ZIP) is for sending a structure
to someone outside TeamCraft: after the save dialog a progress dialog shows
how much has arrived, and **Cancel** stops it – also while the server is
still packing. The ZIP is written under a temporary name next to the target
and renamed only when it is complete, so a cancelled or failed Pack and Go
never leaves a half ZIP behind.

**Moving an older workspace.** Earlier versions of the app kept one folder
per revision, `<group>/<item>/<revision>/` (and before that
`<item>/<revision>/`). After the sign-in the app finds these folders and
offers to move the files to their places in the new layout. Files whose
place is already taken by a different file, and older revisions of an item
whose newer revision takes the place, stay where they are; files you
changed are moved first. Identical copies are removed, emptied old folders
too, and every step is written to `.teamcraft/migration.log`.

## Run it

```sh
cd desktop
cargo run            # Linux needs the WebKitGTK development packages
```

The platform guides list the system packages. Installers are not published
yet; the app is built and tested from source.

## Update notice

Once a day at most, the app asks GitHub for the latest TeamCraft release. If
it is newer than your version, the status bar shows *TeamCraft x.y.z is
available* – a link to the release page in your browser. The app never
downloads or installs anything itself; install the new version as you
installed this one. *×* hides the notice until the next version; *Settings → Updates* switches the check off (for
example on computers without
internet access).
