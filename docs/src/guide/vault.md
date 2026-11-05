# Vault, check-out and check-in

TeamCraft works like an engineering vault: files are never edited in place.
You **check out** a revision (take the exclusive lock), change attributes,
files and the BOM, and **check in** – the new file versions are stored and
the lock is released in **one transaction**. If anything fails, nothing
changes.

## The vault explorer

*Vault* in the top bar opens a three-pane explorer:

| Pane    | Shows                                                                                                            |
|---------|------------------------------------------------------------------------------------------------------------------|
| Folders | **My check-outs**, **★ Favorites**, **All check-outs**, every project group (items, locks), items without group |
| Items   | number, revision, name, state and **who holds the lock** – filterable                                            |
| Detail  | files with version, size, format and a 3D button; *Check out*, *Check in*, *Cancel check-out*                    |

Every action returns to the explorer, so a typical session is: pick the
folder, select the item, *Check out*, download the files, work in your CAD
program, drop the changed files into *Check in*, done.

> **Browser downloads are for single files and for viewing.** A browser
> saves files one by one into *Downloads* – without the folder structure an
> assembly needs, without knowing which version a file is and without
> read-only protection. For CAD work (assemblies, drawings, anything you
> will change and check in) use the **desktop working folder**: one folder
> per project group with the original folder structure, *Get* for whole
> assemblies, read-only files until you check out – see
> [How files appear on your computer](desktop.md#how-files-appear-on-your-computer).

## Rules

* Only revisions **in work** can be checked out, and only by CAD editors of
  the item's project group.
* A new item and a new revision (*Revise*) are checked out to their author.
* **Check in** with files: files with the same name become the next version (Creo's iteration suffixes `bracket.prt.7`
  are folded into versions);
  identical content is recognised and not stored again.
* Every file has a **folder** inside its project group (`chassis/brackets`;
  empty = the top of the group). It is the file's place in the desktop
  working folder, so assemblies find their parts. The folder import sets it
  from the scanned tree, uploads and check-ins may send it (`folder`,
  `folders` in the API), new versions keep it, and while the revision is
  checked out the *Folder* column of the revision page moves a file. Folders
  are relative: `..`, drive letters and names that are invalid on Windows
  are refused.
* **Cancel check-out** releases your lock at once – no dialog, nothing is
  uploaded. **Force unlock** (admins) releases someone else's lock after a
  confirmation, is recorded in the audit log and **notifies the owner**
  (notification kind *unlock*).
* The revision page and the dashboard show **how long** a lock has been
  held (*🔒 Max for 3 days*), so forgotten check-outs stand out.
* **Check in all** (dashboard, *Checked out by me*) checks in every revision
  you hold **without new files** – the locks are released, the revisions keep
  their current files. Each revision is checked in on its own (own
  transaction, own audit record), so one failure does not block the others;
  the message lists what failed and why. Upload changed files with the
  revision's own *Check in* first.

## Concurrency: nothing is overwritten silently

The lock keeps two people from changing a revision at the same time. When a
lock was broken (*Force unlock*) and someone else checked in meanwhile, the
original owner's files are out of date – checking them in would throw away
the newer version. TeamCraft therefore checks every upload against the
version it was **based on** (optimistic concurrency, as Vault and Teamcenter
clients do):

* The forms of the revision page send the versions the page showed; the
  desktop app sends the SHA-256 of the version each file was copied from;
  API clients send `bases` (see the [REST API](../reference/api.md)).
* If a file moved on since, the check-in or upload is **refused with 409**
  and a message such as *hub.step changed on the server since you got it:
  your change is based on v1, the server has v2 by Erika (…). Nothing was
  stored* – the whole check-in is rolled back, the lock stays yours. Get the
  current version, merge your changes and check in again.
* Identical content is never a conflict, and uploads without a base keep the
  plain last-writer-wins behaviour.

Per file, a workspace is in one of these states (desktop Workspace tab,
`…/file-status` in the API):

| State        | Meaning                                                                          |
|--------------|----------------------------------------------------------------------------------|
| *up to date* | the local file is the current version                                            |
| *modified*   | changed locally, the server still has the version it was based on – check it in |
| *outdated*   | unchanged locally, the server has a newer version – get the current files       |
| *conflict*   | changed locally **and** on the server since – a check-in would be refused        |
| *new*        | only local                                                                       |
| *missing*    | only on the server                                                               |
* Uploads show a progress bar; **Cancel upload** stops the transfer
  immediately and nothing changes on the server.

## Large files: chunked, resumable uploads

CAD assemblies and simulation results reach several gigabytes. Sending them
in one request means starting over after every network hiccup, so API
clients – and the desktop app and the web UI for files above 32 MiB – upload
them in **chunks**:

1. `POST /api/v1/uploads` announces the file (revision, name, size,
   optionally its SHA-256, the base version and its folder in the project
   group – the desktop app sends the folder of the working folder, so the
   file keeps its place there). The server refuses at once
   what it would refuse later: a file larger than `TEAMCRAFT_MAX_UPLOAD_MB`,
   a revision you have not checked out, or a base the server file has moved
   on from.
2. `PATCH /api/v1/uploads/{id}` appends one chunk (at most
   `TEAMCRAFT_UPLOAD_CHUNK_MB`) at the offset the server confirmed last. A
   chunk at any other offset is refused with **409** and the server's offset,
   so after a broken connection the client asks (`HEAD`/`GET`) and resumes
   there – bytes of an interrupted chunk that were never confirmed are cut
   off. Each chunk is on disk before its offset is confirmed.
3. `POST /api/v1/uploads/{id}/finish` – or the upload id in the `uploads`
   field of a multipart check-in – hashes the received file, checks size and
   SHA-256 (a mismatch resets the upload to offset 0), moves it into the
   content-addressed vault and stores the version through the same code as
   every other upload: check-out required, base-version guard, audit log,
   notifications, one transaction with the other files of the check-in.

The chunks are collected in `uploads/` below the vault directory (the same
file system as `objects/`, so the finished file is moved, not copied); only
one chunk per request is held in memory, which is why
`TEAMCRAFT_MAX_UPLOAD_MB` can be raised for large files without more memory.
Uploads are private to the user who started them. An unfinished upload
expires 24 hours after its last chunk; the server removes expired uploads and
their data at start-up and every hour. Each user may have at most 16
unfinished uploads at a time, so the space they reserve stays bounded
(16 × `TEAMCRAFT_MAX_UPLOAD_MB` per user). `DELETE /api/v1/uploads/{id}`
cancels one at once.

**In the browser** the upload and check-in forms of the revision page and
the vault explorer do the same on their own: files above 32 MiB go first in
chunks (to `/uploads`, the same sessions, signed in with the browser
session), the progress bar counts all files, and the form then stores them
together with the small files and the comment in one step. A dropped chunk
is sent again after a short wait (up to six times); if the connection stays
away, or the page was closed or reloaded, choose the same files again and
submit – the upload continues where the server stopped, the browser
remembers the unfinished uploads per revision and file. **Cancel upload**
removes them on the server. The *New item* form still sends its files in one
request, within `TEAMCRAFT_MAX_UPLOAD_MB`; attach large files to the new
revision afterwards.

## Favorites, saved searches, Save As

* The **star** next to the title of a revision page keeps the item in **Vault → Favorites**
  (like Teamcenter's Favorites). Favorites are personal.
* **Saved searches:** filter the item list, then *Save this search* – the
  named search appears above the list for one-click reuse (like Teamcenter's
  Saved Queries).
* **More → Save as…** on a revision page creates a new item from it: the attributes
  and, optionally, the current files and the BOM. The new item starts with
  revision A, checked out to you; the source does not change.

## Desktop working folder

The desktop app keeps a local working folder: **one folder per project
group that mirrors the folder structure of the files**, one copy of each
file (`TeamCraft/ROVER/chassis/brackets/bracket.prt`); the revision is
metadata, not a folder level.

* *Get* copies an item with its BOM (revision rule *latest* or *released*)
  and the parts its CAD files reference; only missing and outdated files
  are downloaded, checked by SHA-256, with progress and cancel.
* Files you have not checked out are **read-only**; *Check out* makes them
  writable, *Check in* uploads the changed ones and makes them read-only
  again.
* The workspace tab shows which files you changed, which are outdated or in
  conflict, name clashes (two items with the same folder and file name –
  never overwritten) and who holds the lock; *Check in* uploads exactly the
  changed files – based on the versions they were copied from. Opening an
  outdated file or one somebody else has checked out asks first.
* *Check in all* in the worklist checks in every check-out without local
  changes and lists the ones that still have changes.

See [How files appear on your computer](desktop.md#how-files-appear-on-your-computer).
