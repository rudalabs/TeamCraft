# Vault, check-out and check-in

TeamCraft works like an engineering vault: files are never edited in place.
You **check out** a revision (take the exclusive lock), change attributes,
files and the BOM, and **check in** – the new file versions are stored and
the lock is released in **one transaction**. If anything fails, nothing
changes.

## The vault explorer

*Vault* in the top bar opens a three-pane explorer:

| Pane    | Shows                                                                                          |
|---------|------------------------------------------------------------------------------------------------|
| Folders | **My check-outs**, **All check-outs**, every project group (items, locks), items without group |
| Items   | number, revision, name, state and **who holds the lock** – filterable                          |
| Detail  | files with version, size, format and a 3D button; *Check out*, *Check in*, *Cancel check-out*  |

Every action returns to the explorer, so a typical session is: pick the
folder, select the item, *Check out*, download the files, work in your CAD
program, drop the changed files into *Check in*, done.

## Rules

* Only revisions **in work** can be checked out, and only by CAD editors of
  the item's project group.
* A new item and a new revision (*Revise*) are checked out to their author.
* **Check in** with files: files with the same name become the next version (Creo's iteration suffixes `bracket.prt.7`
  are folded into versions);
  identical content is recognised and not stored again.
* **Cancel check-out** releases your lock at once – no dialog, nothing is
  uploaded. **Force unlock** (admins) releases someone else's lock after a
  confirmation and is recorded in the audit log.
* Uploads show a progress bar; **Cancel upload** stops the transfer
  immediately and nothing changes on the server.

## Desktop workspace

The desktop app adds a local workspace: *Check out* copies the files into a
folder per revision, the workspace tab shows which files you changed, and *Check in* uploads exactly those.
See [Desktop app](desktop.md).
