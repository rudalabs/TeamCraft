# Manual test tour

A 20-minute walk through every feature on a local server (`docker compose up -d db && cargo run`,
then <http://localhost:8080>).
Use two browsers (or a private window) for the two-person steps.

## 1. Accounts and project groups

1. Create the administrator on the login page (first start only).
2. *Users → New user*: `erika` (CAD editor) and `rolf` (CAD editor), `vera` (viewer).
3. Sign in as **erika**. *Projects → New project group* `ROVER` – erika is its
   manager. Add `rolf` as member. (The tour builds a rover with wheel hubs;
   any product works the same way – a pump, a gearbox, a conveyor, a machine.)
4. *Account → Appearance* (or the menu under your name): *System* follows the light or dark appearance of your operating
   system, *Light* and *Dark* stay fixed; the distribution skins (Arch, KDE, Ubuntu, …) are dark. The choice is
   remembered for the login page as well.

## 2. Items, check-out and check-in

1. *Items → New item*: choose `hub.step` and `hub.stl` as files – the name *hub* and the type *part* are filled in;
   change the name to *Wheel hub*,
   project group `ROVER`. The item is created with both files; revision A is
   in work and checked out to erika (lock badge).
2. Open **3D view** for each file. The STEP model is
   tessellated on the server – try *Export STL*.
3. *Vault → My check-outs*: the item is listed; select it, **Check in** a
   changed `hub.step` with a comment. The upload shows a progress bar; try **Cancel upload** on a large file – nothing
   changes.
4. As **rolf**: the item is read-only while erika holds the lock; after the
   check-in rolf can *Check out*. **Cancel check-out** releases it at once.

## 3. Versions and rollback

1. On the revision page, open the version history of `hub.step`, download v1.
2. *Restore* v1: it becomes v3 – history is never rewritten.

## 4. Structure

1. Create an assembly *Rover* and add the hub to its BOM (find number 10,
   quantity 4). Open the BOM page; switch the revision rules.
2. Upload an assembly file that references `hub.step` (e.g. a STEP assembly):
   the references are listed and matched.
3. *Pack and Go (ZIP)* on the BOM page.

## 5. Change and release

1. As erika: *Release via new change…*, add rolf as reviewer, *Submit*.
2. As rolf: the bell shows the review request; **Approve**. The hub is
   released; dependents get *Check needed*.
3. *Revise* the released hub: revision B is in work and checked out.

## 6. Viewer, API, audit

1. As **vera**: everything is readable, nothing editable.
2. *Account → API tokens*: create one and call
   `curl -H "Authorization: Bearer <token>" http://localhost:8080/api/v1/me`.
3. As the administrator: *Audit log* lists every step of this tour.

## 7. Operations

`curl -s localhost:8080/healthz` → `ok`, `/readyz` → database and vault
status, `teamcraft vault check --verify-hashes`.
