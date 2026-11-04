# REST API

The JSON API lives under `/api/v1` and authenticates with **personal access
tokens** (`Authorization: Bearer tc_pat_…`), created on the account page
with an optional expiry. Cookies are never accepted by the API.
`GET /api/v1` on a running server lists every endpoint.

```sh
TOKEN=…   # from Account → API tokens
curl -s -H "Authorization: Bearer $TOKEN" http://localhost:8080/api/v1/me
curl -s -H "Authorization: Bearer $TOKEN" "http://localhost:8080/api/v1/items?q=bracket&system=solidworks"
```

## The check-out / check-in cycle

```sh
API=http://localhost:8080/api/v1; H="Authorization: Bearer $TOKEN"
curl -s -X POST -H "$H" $API/items/PRT-000001/revisions/A/checkout
curl -s -H "$H" -o hub.step $API/files/<file id>/content
# … edit …
curl -s -X POST -H "$H" -F file=@hub.step -F comment="wall 2 → 2.5 mm" \
     $API/items/PRT-000001/revisions/A/checkin          # stores and releases in one step
curl -s -X POST -H "$H" $API/items/PRT-000001/revisions/A/cancel-checkout
```

### Concurrency: base versions and file status

A check-in or upload can name the version each file is **based on** in the
multipart field `bases`, a JSON object from file name to the version number
(`0` = a new file that must not exist yet) or the SHA-256 of the content the
client checked out. If a file moved on since (e.g. after an administrator
broke the lock and someone else checked in), the request is refused with
**409 Conflict** and nothing is stored; identical content is never a
conflict, and files without a base are stored as before. Several uploads of
one name in a request (Creo iterations `housing.prt.6`, `housing.prt.7`)
share the base: the first is checked against it, the others build on it.

```sh
curl -s -X POST -H "$H" -F file=@hub.step -F 'bases={"hub.step": 3}' \
     $API/items/PRT-000001/revisions/A/checkin
# 409 {"error": {"code": "conflict", "message": "hub.step changed on the server since you got it: …"}}
```

`GET …/revisions/{code}/file-status` returns every file with its current
`version`, `sha256`, uploader and the lock (`locked_by`, `locked_by_name`,
`locked_at`, `locked_by_me`, `locked_by_other`). `POST` the same path with the
client's files to get the sync state of each local copy:

```sh
curl -s -X POST -H "$H" -H 'Content-Type: application/json' \
     -d '{"files": [{"filename": "hub.step", "sha256": "9f86d0…", "base_version": 3}]}' \
     $API/items/PRT-000001/revisions/A/file-status
```

`state` is `up_to_date`, `modified` (the server still has the base),
`outdated` (the local content is an older version – `local_version` says
which), `conflict` (changed locally and on the server since the base), `new`
(only local) or `missing` (only on the server). Without a base, unknown local
content counts as `modified`.

### Working folder: folders and the working set

Every file has a `folder` inside its project group (`""` = the top), the
place it gets in the desktop working folder
`<workspace>/<group>/<folder>/<file>`. Uploads and check-ins set it with the
multipart field `folder` (one folder for all files of the request) or
`folders` (a JSON object from file name to folder, wins over `folder`); files
sent without one keep their folder (new files: the top). Folders are
normalised (`\` → `/`, no leading or trailing `/`) and refused with 400 if
they contain `..`, a drive letter or names invalid on Windows.
`PATCH /files/{id}` with `{"folder": "chassis/brackets"}` moves a file while
you hold the check-out (no new version; recorded in the history). File lists
and `file-status` return the `folder`.

```sh
curl -s -X POST -H "$H" -F file=@bracket.prt -F file=@chassis.asm \
     -F 'folders={"bracket.prt": "chassis/brackets", "chassis.asm": "chassis"}' \
     $API/items/ASM-000001/revisions/A/checkin
```

`GET …/revisions/{code}/workset?rule=latest|released|as_released` is what
the desktop *Get* downloads: the revision, its multi-level BOM under the
rule and the items holding the CAD files those files reference
(`references=false` leaves them out), each revision with `via`
(`root`/`bom`/`reference`), `project_code`, lock (`locked_by_name`,
`locked_by_me`) and its files (`file_id`, `filename`, `folder`, `version`,
`sha256`, `size_bytes`); plus `unresolved` (items without a revision for the
rule), `missing_references` (names not in the vault), `file_count` and
`total_bytes`.

`POST /worklist/checkin-all` (body optional: `{"revisions": [id, …]}`) checks
in all of my check-outs – or the listed ones – **without new files**, one
transaction and one audit record per revision. The answer lists every
revision with `ok` and `error`, plus `checked_in`, `failed` and a `summary`.

## Main resources

| Resource         | Endpoints                                                                                                                             |
|------------------|---------------------------------------------------------------------------------------------------------------------------------------|
| Items            | `GET/POST /items`, `GET /items/{number}`, where-used, history, impacts, watch                                                         |
| Revisions        | `GET/PATCH /items/{number}/revisions/{code\|latest}`, `checkout`, `checkin`, `cancel-checkout`, `file-status`, `revise`, `obsolete`   |
| Worklist         | `GET /worklist` (check-outs, reviews, impacts, statistics), `POST /worklist/checkin-all`                                              |
| Files            | `POST …/files` (multipart, `folder`/`folders`), `GET/PATCH /files/{id}` (metadata, folder), `/content`, `/versions`, `/restore`, `/mesh`, `/thumbnail`, `…/workset` |
| BOM              | `GET/POST …/bom` (`rule=latest\|released\|as_released`, `expand=true`), `PATCH/DELETE /bom-lines/{id}`, `…/package` (Pack and Go ZIP) |
| BOM import       | `POST /bom-imports` (new assembly), `POST …/bom/import` (into a checked-out revision); multipart `file` = CSV/Excel table or STEP assembly, `dry_run=true` returns the plan |
| References       | `GET/POST …/references`, `DELETE /references/{id}`, `GET /impacts`                                                                    |
| Change requests  | `GET/POST /change-requests`, `GET/PATCH /change-requests/{number}`, `comments`, `items`, `triage`, `assign`, `decide`, `reopen`, `withdraw`, `create-change`, `link-change`, `attachments`, `watch` |
| Simulations      | `GET …/simulation`, `GET/POST …/simulation/bases`, `DELETE /simulation-bases/{id}`, `GET/POST/PUT …/simulation/results`, `…/results/import`, `PATCH/DELETE /simulation-results/{id}`, `…/simulation/compare`, `GET /items/{number}/simulations` |
| Changes          | `GET/POST /changes`, items, reviewers, `readiness`, `dependencies` (add all unreleased), `submit`, `review`, `withdraw`, `impact`      |
| Projects         | `GET/POST /projects`, `GET/POST /projects/{code}/members`, `GET/PUT /projects/{code}/numbering` (`?scheme=` previews)                 |
| Notifications    | `GET /notifications`, `POST /notifications/read`, settings, watches                                                                   |
| Personal         | `GET /favorites`, `POST/DELETE /items/{number}/favorite`, `GET/POST /searches`, `DELETE /searches/{id}`                               |
| Save As, compare | `POST …/revisions/{code}/save-as` `{number?, name?, project?, files?, bom?}`, `GET …/revisions/{code}/bom/compare?with={code}`        |
| Formats          | `GET /formats` – the recognised CAD programs and formats                                                                              |
| MCP              | `POST /mcp` – Model Context Protocol for AI assistants, see [MCP server](mcp.md)                                                      |
| Assistant        | `GET/POST /assistant` – the built-in assistant (optional), see [Ask TeamCraft](../guide/assistant.md#api)                              |

The BOM comparison is also available as an Excel workbook of the same lines
(same `with` query): `GET …/revisions/{code}/bom/compare.xlsx?with={code}`.
Both compare the direct lines; `depth=all` compares every level, with the
children resolved by `rule=latest|released|as_released` (each line then has
its `level` and the resolved `revision` on both sides). The expanded BOM
itself downloads as Excel workbook: `GET …/revisions/{code}/bom.xlsx?rule=`.

## Change requests (ECR)

Every signed-in user – viewers included – raises change requests and
comments on them; CAD editors of the project groups of the affected items
(and admins) triage and decide. `GET /change-requests/{number}` returns the
request with its `items`, `comments`, `attachments`, linked `changes`,
`history` and `can` (the actions the caller may take). See
[Change requests](../guide/changes.md) for the flow.

```sh
curl -s -X POST -H "$H" -H "Content-Type: application/json" $API/change-requests -d '{
  "title": "Weld seam cracks", "reason": "manufacturing", "priority": "high",
  "description": "Seen on 3 of 10 parts. Proposal: 4 mm fillet.", "due_date": "2026-12-01",
  "items": [{"item": "PRT-000001", "revision": "A", "note": "File: bracket.step"}]}'
curl -s -X POST -H "$H" -F file=@crack.png $API/change-requests/ECR-000001/attachments
curl -s -X POST -H "$H" $API/change-requests/ECR-000001/triage                     # new → in triage
curl -s -X POST -H "$H" -H "Content-Type: application/json" \
     $API/change-requests/ECR-000001/decide -d '{"decision": "accept"}'           # or reject / defer + comment
curl -s -X POST -H "$H" $API/change-requests/ECR-000001/create-change              # draft CHG with the in-work revisions
```

`create-change` answers with the new `change`, the revisions it `added` and
the affected items it `skipped` (no revision in work, already in another
open change, no access). `link-change` (`{"change": "CHG-000004"}`) links an
existing open change order instead. When a linked change order is released,
the request becomes `implemented`. Lists filter with
`?status=open|all|new|in_triage|accepted|rejected|deferred|implemented|withdrawn`,
`scope=mine|assigned` and `item={number}`; `GET /worklist` contains
`requests_assigned` and `my_requests`, `GET …/revisions/{code}` the
`open_requests` of the item.

## Simulations

A simulation revision (item type `simulation`) links the revisions it was
computed with and stores its key result values (see
[Simulations](../guide/simulation.md)). Writes need the simulation revision
checked out by the caller.

| Endpoint                                                      | Body / result                                                                                                                  |
|---------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------|
| `GET …/revisions/{code}/simulation`                           | `{revision, out_of_date, summary, bases, results}`; every basis has `out_of_date` (reason or `null`), every result `status`    |
| `GET/POST …/revisions/{code}/simulation/bases`                | `{item, revision?, file? \| file_version_id?, role: geometry\|mesh\|material\|load_case\|other, note?}` – default: latest revision |
| `DELETE /simulation-bases/{id}`                               | removes a basis link                                                                                                           |
| `GET/POST …/revisions/{code}/simulation/results`              | `{name, value, unit?, lower_limit?, upper_limit?, load_case?, solver?, solver_version?, note?}` (`limit_low`/`limit_high` too) |
| `PUT …/revisions/{code}/simulation/results`                   | `{results: [...], replace?: true}` – the result set of a solver run in one transaction; same name and load case = update      |
| `POST …/revisions/{code}/simulation/results/import`           | multipart `file` (CSV/TSV/Excel `name,value,unit,limit_low,limit_high,load_case`), `replace=true` optional                     |
| `PATCH/DELETE /simulation-results/{id}`                       | change any field (`null` clears a limit) or remove                                                                             |
| `GET …/revisions/{code}/simulation/compare?with={code}`       | `{before, after, summary, diffs}` – matched by name and load case, with `delta` and `delta_percent`                           |
| `GET /items/{number}/simulations`                             | simulations whose latest revision is based on the item, with `out_of_date` and `open_impact_id`                               |

A solver script after a run:

```sh
curl -s -X POST -H "$H" $API/items/SIM-000003/revisions/B/checkout
curl -s -X PUT -H "$H" -H 'Content-Type: application/json' \
     -d '{"results": [{"name": "Max. von Mises stress", "value": 182.5, "unit": "MPa",
                       "upper_limit": 250, "load_case": "LC1", "solver": "CalculiX", "solver_version": "2.21"}]}' \
     $API/items/SIM-000003/revisions/B/simulation/results
curl -s -X POST -H "$H" -F file=@job.frd $API/items/SIM-000003/revisions/B/checkin
```

Errors are JSON: `{"error": {"code": "conflict", "message": "…"}}` with the
matching HTTP status (400, 401, 403, 404, 409, 413, 500).
