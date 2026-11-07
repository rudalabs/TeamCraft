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

## Token scopes

A token has a scope that narrows what its user may do with it; it never
widens the user's rights. Choose it on the account page, with
`POST /api/v1/tokens` (`{name, expires_in_days?, scope?}`) or
`teamcraft user create-token --scope`.

| Scope       | May                                                                                                   |
|-------------|-------------------------------------------------------------------------------------------------------|
| `full`      | read, write and approve: everything the user may do (default; every older token and the desktop token) |
| `write`     | read and write, but not approve: `POST /changes/{n}/review`, `POST /change-requests/{n}/decide` and `POST /items/{n}/revisions/{r}/obsolete` answer 403 |
| `read_only` | `GET`/`HEAD` requests and the read-only MCP tools                                                     |
| `mcp_only`  | only `POST /api/v1/mcp` (all tools)                                                                   |

A refused request answers `403` and changes nothing. A `write` token cannot
create a `full` token. `GET /api/v1/me` shows the `token_scope`. A token
with an idle lifetime (the desktop app's) expires after that many days
without use.

## Read permissions

Every answer contains only what the token's user may read (see
[Read permissions and restricted groups](../admin/permissions.md)):

* An item, revision, file, file version, change order, change request,
  attachment or project group the user may not read answers **404 with the
  same message as an unknown one** (`{"error": {"code": "not_found", …}}`),
  never 403. Lists and their `total` leave such objects out.
* Rows that link a visible object to a hidden item are **placeholders**:
  `"restricted": true`, the identity fields (number, name, type, revision,
  state) empty and the ids the nil UUID; structural fields (find number,
  quantity, kind of a reference, role of a simulation basis) stay. Notes
  are emptied (they describe the hidden item), a hidden simulation basis
  has `out_of_date: null`, and placeholders come after the visible rows of
  their kind. This applies to BOM lines and trees, item references,
  simulation bases, change order and change request items and readiness
  blockers and warnings; impacts get `"source_restricted": true` with the
  source removed. The releasing change order of an impact
  (`change_id`, `change_number`) is left out, and another open change in
  readiness (`other_change`) reads "another open change order", when the
  user may not see that change.
* Creating an item whose generated number is already held by an item the
  user cannot see answers 409 ("already in use; change the scheme or enter
  a number") instead of skipping to the next number.
* Expanded BOMs (`?expand=true`) carry `restricted_lines` and
  `rollup_incomplete`; a working set carries `restricted_lines`; the Pack and
  Go `manifest.json` carries `restricted_lines`.
* Lists that leave entries out say how many: `where-used`,
  `/items/{number}/simulations` and `/changes/{number}/impact` return the
  usual array plus the header `x-hidden-count`; the revision answer has
  `hidden_changes`, references have `hidden_incoming`, a change request
  has `hidden_changes`. CAD file-name matches (`file_referrers`) in groups
  the user cannot see are left out without a count.
* `GET /api/v1/me` lists the user's own memberships in `projects` and,
  in `restricted_projects`, those of them that are restricted. It never
  names groups the user is not a member of. `auth_source` is `local` or
  `oidc`; only local accounts have a password to change.
  `capabilities` says what the server offers: `assistant` (Ask TeamCraft
  is configured), `mcp` and `local_agents` (both `false` when the
  administrator set `TEAMCRAFT_LOCAL_AGENTS=off`: `POST /api/v1/mcp` then
  refuses every token and clients offer no local agent).
* `GET /api/v1/projects` lists only the groups the user may see; projects
  carry `visibility` (`internal` or `restricted`). `POST /api/v1/projects`
  takes an optional `visibility`; `PUT /api/v1/projects/{code}/visibility`
  `{"visibility": "restricted"}` restricts a group (managers of the group
  and admins). `GET`/`PUT /api/v1/projects/{code}/policy`
  (`{"admin_read": "implicit"|"with_reason"|"none"}`, managers and admins)
  read and set how administrators read a restricted group;
  `POST /api/v1/projects/{code}/elevations` `{"reason"}` (admins, policy
  `with_reason`) opens 60 minutes of access, `DELETE` on it ends it.;
  `GET /api/v1/projects/{code}/access-review` (`?format=csv`) is the access report of a
  restricted group, `POST` confirms the review `{"note"}`, `GET …/access-review/{id}` is the
  member list at that review (managers and admins). See
  [Read permissions](../admin/permissions.md#need-to-know-administrator-access).

## Creating items, safely repeated

`POST /items` takes `{item_type, name, project, number?, unit?, description?,
material?, mass_kg?, unit_cost?, classification?, attributes?}` and answers
**201** with the new revision `A`. A client that lost the answer (timeout,
crash) cannot tell whether the item exists. With the optional
**`idempotency_key`** (1 to 255 printable ASCII characters without spaces,
chosen by the client) the request can be repeated safely:

| Request                                          | Answer                                                         |
|--------------------------------------------------|----------------------------------------------------------------|
| first request with a key                         | **201**, the item is created                                   |
| same key, same user, same fields again           | **200** with the original item (current state) and the header `Idempotent-Replay: true`; nothing is created |
| same key, same user, any field different         | **409**; nothing is created                                    |
| simultaneous identical requests                  | exactly one **201**, all others **200** with that item         |
| same key from another user                       | independent: keys belong to the user                           |
| invalid key                                      | **400**                                                        |

The key is stored in the transaction that creates the item, so a refused
request (403, 409, 400 …) leaves no key behind and may be repeated with the
same key once it is valid. Without a key every request creates an item. The
desktop's *Import CAD Folder…* sends a stable key per source item (group,
folder and item), so a run after a lost answer finds the items it created.

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

### Large files: chunked, resumable uploads

Files of several GB go up in chunks that survive broken connections
(similar to the tus protocol; see [Large files](../guide/vault.md#large-files-chunked-resumable-uploads)):

```sh
SIZE=$(stat -c %s results.op2); SHA=$(sha256sum results.op2 | cut -d' ' -f1)
curl -s -X POST -H "$H" -H 'Content-Type: application/json' \
     -d "{\"item\": \"PRT-000001\", \"revision\": \"A\", \"filename\": \"results.op2\",
          \"size\": $SIZE, \"sha256\": \"$SHA\", \"base\": 3}" $API/uploads
# 201 {"id": "…", "offset": 0, "size": …, "chunk_bytes": 16777216, "location": "/api/v1/uploads/…", …}
split -b 16M -d results.op2 part.; OFFSET=0
for p in part.*; do
  curl -s -X PATCH -H "$H" -H "Upload-Offset: $OFFSET" --data-binary @$p $API/uploads/$ID
  OFFSET=$((OFFSET + $(stat -c %s $p)))
done
curl -sI -H "$H" $API/uploads/$ID                      # Upload-Offset / Upload-Length: where to resume
curl -s -X POST -H "$H" -H 'Content-Type: application/json' \
     -d '{"comment": "run 7", "check_in": true}' $API/uploads/$ID/finish
```

| Request                         | Meaning                                                                                                                                                                          |
|---------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `POST /uploads`                 | `{item, revision, filename, size, sha256?, content_type?, base?, folder?}` → **201** with the session (`folder` = place in the project group, see below; omitted = keep the file's folder); 413 above `TEAMCRAFT_MAX_UPLOAD_MB`, 409 if the revision is not checked out by you or `base` is outdated |
| `PATCH /uploads/{id}`           | header `Upload-Offset`, body = the next chunk (≤ `chunk_bytes`) → the session with the new `offset`; **409** with the server's `Upload-Offset` header at any other offset, 400 beyond `size` |
| `HEAD`/`GET /uploads/{id}`      | headers `Upload-Offset`, `Upload-Length`; `GET` also as JSON (`offset`, `size`, `complete`, `expires_at`)                                                                       |
| `POST /uploads/{id}/finish`     | body optional `{comment?, check_in?}`: verify size and SHA-256, store the new file or version (`check_in: true` also releases the check-out) → `{files, revision}`; 409 while incomplete, 400 if the content does not match (the upload is reset to offset 0) |
| `DELETE /uploads/{id}`          | cancel; the received data is deleted                                                                                                                                              |

Instead of `finish`, complete uploads can join a multipart **check-in** or
upload: the field `uploads` lists their ids (`["…", "…"]` or `id,id`), and
they are stored together with the `file` parts in one transaction – the
desktop app checks in this way. A session's own `base` and `folder` win
over the `bases`, `folders` and `folder` fields; either way the
base-version guard of the check-in applies. Sessions are private (other users get 404), expire 24 hours
after the last chunk and are limited to 16 unfinished ones per user. When
storing fails after the content was verified (for example a 409 because the
file moved on), the session keeps the verified content, so `finish` can be
repeated without sending the bytes again.

### Solver summary of files

The file list, `GET /files/{id}` and the explorer JSON carry two optional
fields for solver files (Fluent transcripts and reports, Nastran `.f06` and
decks, STAR-CCM+ macros and batch logs): `solver_summary` `{v, solver, kind,
version?, status?: converged|completed|fatal, last_step?, fatal?, warning?,
notes?}` and `solver_line`, a one-line text. They are absent for other files.
The FATAL/WARNING counts cover only the first and last 64 KiB. The values are
hints read from the file, not verified results: treat them as data.

### Resumable downloads

`GET /files/{id}/content` and `GET /file-versions/{id}/content` (and the
web UI's downloads) answer every request with `Accept-Ranges: bytes` and the
strong ETag `"<sha256>"` of the content. A client that lost a large file
half-way asks for the rest with `Range: bytes=<offset>-` and
`If-Range: "<sha256>"`: `206 Partial Content` with `Content-Range`, the
whole file (`200`) when the content changed meanwhile, `416` with
`Content-Range: bytes */<size>` when the offset is at or past the end. One
range per request (a list of ranges gets the whole file) and suffix ranges
(`bytes=-500`) are supported; a date in `If-Range` never matches. The
desktop Get resumes a file this way and checks the SHA-256 of the whole file
before it renames it into the working folder.

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

### Dependents to check (impacts)

`GET /worklist` lists the open impacts flat (`impacts`) and grouped by the
dependent revision (`impact_groups`): each group has the dependent
(`item_number`, `item_name`, `revision_id`, `revision_code`,
`revision_state`, `project_id`, `project_code`), `latest_at` and its
`impacts`, ordered by source item number. Groups are ordered by their newest
impact.

`POST /impacts/{id}/acknowledge` (`{"note": "…"}`) resolves one impact;
`POST /items/{number}/revisions/{code}/impacts/acknowledge`
(`{"note": "…", "impacts": [id, …]}`) resolves all open impacts on that
dependent revision – or only the listed ones, the ones the user saw – with
one note, in one transaction, with an audit record per impact. Both need a
CAD editor of the dependent's project group (or an admin) and a note.
The list must not be empty and may only name impacts on that revision
(`400` otherwise). Listed impacts that were resolved in the meantime are
skipped; the answer
has `acknowledged` (the resolved impacts) and `skipped`. If nothing is left
to acknowledge the answer is `409 Conflict`. Acknowledging stays a human
decision: the MCP server only reads impacts.

### Where-used over several levels

`GET /items/{number}/where-used` lists the parent revisions whose BOM uses
the item. With `?levels=N` (1–25) it walks up further – parents of the
parents – and every row also carries `level` (1 = direct parent) and
`child_number` (the item used on that line). Each item is reported on the
lowest level it is reached on. Both reports stop at 2000 rows.

### Custom attributes and classification

Administrators define attributes once; values are set per revision by key,
as text, number, `true`/`false` or one of the choices (`null` or `""`
removes a value, keys not sent keep theirs):

```sh
curl -s -X POST -H "$H" -H 'Content-Type: application/json' \
     -d '{"key": "length_mm", "data_type": "number", "label": "Length", "unit": "mm"}' $API/attributes
curl -s -X PATCH -H "$H" -H 'Content-Type: application/json' \
     -d '{"attributes": {"length_mm": 120.5, "vendor": null}}' $API/items/PRT-000001/revisions/A
curl -s -X PATCH -H "$H" -H 'Content-Type: application/json' \
     -d '{"classification": "purchased"}' $API/items/PRT-000001
curl -s -H "$H" "$API/items?class=purchased&attr=length_mm&value=100..200"
# simulations with a failed value whose name contains "stress"
curl -s -H "$H" "$API/items?type=simulation&result=failed&result_name=stress"
```

`value` matches text and choices as a part (case-insensitive), yes/no
attributes with `yes`/`no`, numbers with `12`, `>=10`, `<=20` or `10..20`; an
empty `value` finds every item that has the attribute. Revisions and the
item search return `attributes` (object by key) and `classification`.

### Alternates, supersession and replace in assemblies

| Request                                            | Body / answer                                                                                     |
|----------------------------------------------------|---------------------------------------------------------------------------------------------------|
| `GET /items/{number}/interchange`                  | `alternates` (with `two_way`), `alternate_for` (one-way), `successor` (or `null`), `predecessors` |
| `POST /items/{number}/alternates`                  | `{item, note?, two_way? = true}` → `201` with the interchange data                                |
| `DELETE /items/{number}/alternates/{alternate}`    | `204`; both directions when you may edit the alternate's group                                    |
| `PUT /items/{number}/supersession`                 | `{item, note?}` – superseded by `item` (replaces an earlier successor; `409` for a circle)        |
| `DELETE /items/{number}/supersession`              | `204`                                                                                             |
| `GET /items/{number}/replace?with=NUMBER`          | preview: `rows` (one per BOM line of the latest parent revisions, with `action`), `replaced`, `hidden` |
| `POST /items/{number}/replace`                     | `{with}` – applies the rows with `action = replace` in one transaction; `409` if there are none   |

`action` is `replace`, `checked_out` (by somebody else), `no_permission`,
`in_review`, `needs_revision` (released) or `cycle`. Linked items in groups
the caller cannot read are left out; a successor there is a placeholder with
`restricted: true`. The preview and the replacement carry `X-Hidden-Count`.

## Main resources

| Resource         | Endpoints                                                                                                                             |
|------------------|---------------------------------------------------------------------------------------------------------------------------------------|
| Items            | `GET/POST /items` (`class=`, `attr=` + `value=`, `result=` + `result_name=` filters), `GET/PATCH /items/{number}` (classification, unit, project), `where-used` (`levels=1…25` for parents of parents), `interchange`, `alternates`, `supersession`, `replace`, history, impacts, watch |
| Attributes       | `GET /attributes` (`all=true`: also inactive), `POST /attributes`, `PATCH /attributes/{key}` (admins); values in `attributes` of a revision |
| Revisions        | `GET/PATCH /items/{number}/revisions/{code\|latest}`, `checkout`, `checkin`, `cancel-checkout`, `file-status`, `revise`, `obsolete`   |
| Worklist         | `GET /worklist` (check-outs, reviews, impacts and `impact_groups`, statistics), `POST /worklist/checkin-all`                          |
| Files            | `POST …/files` (multipart, `folder`/`folders`), `GET/PATCH /files/{id}` (metadata, folder), `/content`, `/versions`, `/restore`, `/mesh`, `/thumbnail`, `…/workset` |
| Uploads          | `POST /uploads`, `PATCH/HEAD/GET/DELETE /uploads/{id}`, `POST /uploads/{id}/finish` – chunked, resumable uploads of large files        |
| BOM              | `GET/POST …/bom` (`rule=latest\|released\|as_released`, `expand=true`), `PATCH/DELETE /bom-lines/{id}`, `…/package` (Pack and Go ZIP); the expanded tree has `project_id` per line (`null` for restricted placeholders) |
| BOM import       | `POST /bom-imports` (new assembly), `POST …/bom/import` (into a checked-out revision); multipart `file` = CSV/Excel table or STEP assembly, `dry_run=true` returns the plan (with `attribute_columns`, `unused_columns`, per item `classification` and `attributes`) |
| References       | `GET/POST …/references`, `DELETE /references/{id}`, `GET /impacts`, `POST /impacts/{id}/acknowledge`, `POST …/impacts/acknowledge`    |
| Change requests  | `GET/POST /change-requests`, `GET/PATCH /change-requests/{number}`, `comments`, `items`, `triage`, `assign`, `decide`, `reopen`, `withdraw`, `create-change`, `link-change`, `attachments`, `watch` |
| Simulations      | `GET/POST /projects/{code}/result-templates`, `PUT/DELETE /result-templates/{id}`, `GET …/simulation`, `GET/POST …/simulation/bases`, `DELETE /simulation-bases/{id}`, `GET/POST/PUT …/simulation/results`, `…/results/import`, `PATCH/DELETE /simulation-results/{id}`, `…/simulation/compare`, `GET /items/{number}/simulations` |
| As-built         | `GET …/as-built`, `PUT …/as-built/record`, `GET/POST …/as-built/deviations`, `PATCH/DELETE /as-built-deviations/{id}`, `GET /items/{number}/built-units`, `POST …/revisions/{code}/built-units` |
| Changes          | `GET/POST /changes`, items, reviewers, `suggested-reviewers`, `readiness`, `dependencies` (add all unreleased), `submit`, `review`, `withdraw`, `impact` |
| Projects         | `GET/POST /projects`, `GET/POST /projects/{code}/members`, `GET/PUT /projects/{code}/numbering` (`?scheme=` previews)                 |
| Notifications    | `GET /notifications`, `POST /notifications/read`, settings, watches                                                                   |
| Personal         | `GET /favorites`, `POST/DELETE /items/{number}/favorite`, `GET/POST /searches`, `DELETE /searches/{id}`                               |
| Save As, compare | `POST …/revisions/{code}/save-as` `{number?, name?, project?, files?, bom?}`, `GET …/revisions/{code}/bom/compare?with={code}`        |
| Similar parts    | `GET …/revisions/{code}/similar` (`limit`, `ignore_scale`, `file`, `wait`) – parts of similar 3D shape, see below                    |
| Formats          | `GET /formats` – the recognised CAD programs and formats                                                                              |
| Password         | `POST /me/password` `{current_password, new_password}` – change the own password (204; throttled; web sessions end, tokens stay) |
| Branding         | `GET /branding` – replaced logos `{wide, square}` (version token or `null`) and `{wide_type, square_type}` (content type); `GET /branding/{wide|square}` – the image (404 = built-in logo) |
| MCP              | `POST /mcp` – Model Context Protocol for AI assistants, see [MCP server](mcp.md)                                                      |
| Assistant        | `GET/POST /assistant` – the built-in assistant (optional), `context` = the page the caller is on, `thread: true` = ask in the caller's conversation; `GET/DELETE /assistant/thread` – the conversation. See [Ask TeamCraft](../guide/assistant.md#api) |

The BOM comparison is also available as an Excel workbook of the same lines
(same `with` query): `GET …/revisions/{code}/bom/compare.xlsx?with={code}`.
Both compare the direct lines; `depth=all` compares every level, with the
children resolved by `rule=latest|released|as_released` (default
`as_released` when one of the revisions is released, else `latest`; each
line then has
its `level` and the resolved `revision` on both sides). The expanded BOM
itself downloads as Excel workbook: `GET …/revisions/{code}/bom.xlsx?rule=`.

### Similar parts

`GET /items/{number}/revisions/{code|latest}/similar` compares the shape of
the revision's 3D model (its STEP file, else its first mesh, or `file=name`)
with the latest revisions of all other items – see
[Finding similar parts](../guide/similar-parts.md) for the method.

| Parameter      | Default | Meaning                                                                     |
|----------------|---------|-----------------------------------------------------------------------------|
| `limit`        | 10      | most hits (1–50)                                                            |
| `ignore_scale` | false   | compare proportions and shape only (a scaled copy scores 100)              |
| `file`         | –       | file name of the 3D model to compare by                                     |
| `wait`         | 30      | seconds to wait (at most 200) when the shape signature is still computed   |

```json
{
  "item": "PRT-000012", "revision": "A",
  "status": "ready",
  "source": { "file_id": "…", "filename": "bracket.step", "sha256": "…", "format": "step" },
  "signature": { "extents": [120.0, 40.0, 8.0], "area": 12160.0, "volume": 38400.0, "closed": true, "triangles": 1234 },
  "ignore_scale": false,
  "candidates": [{
    "item_number": "PRT-000007", "revision_code": "B", "name": "Bracket", "state": "released",
    "revision_id": "…", "file_id": "…", "filename": "bracket-old.stl", "identical": false,
    "score": 91, "shape": 94, "size": 97, "extents": [118.0, 40.0, 8.0],
    "explanation": "extents 118 × 40 × 8 vs 120 × 40 × 8, shape distribution 94 % similar, volume 37760 vs 38400",
    "thumbnail_file_id": null
  }]
}
```

`status` is `ready`, `pending` (call again shortly), `no_shape_file` (no STEP
or mesh file) or `failed` (`message` says why, e.g. the model could not be
read). `volume` is `null` when the mesh is not watertight.

### Suggested reviewers

`GET /changes/{number}/suggested-reviewers?limit=5` (1–20) answers *who
should review this?* from the history of the affected items:
`{"change": "ECO-000004", "suggestions": [{"user_id", "username",
"display_name", "role", "score", "reasons": ["edited PRT-000005 3 times in
the last 90 days", "manager of ROVER"]}]}`, highest score first, ties by
name. Only active CAD editors and admins are suggested; the author and the
assigned reviewers never are. Users who changed the affected revisions come
last with `"cannot_review": true` (four-eyes): show them, do not add them. It only reads – add a suggestion with
`POST /changes/{number}/reviewers`. See
[Suggested reviewers](../guide/changes.md#suggested-reviewers) for the
sources and points.

### Review rounds and absent reviewers

`GET /changes/{number}` returns `review_round`, `earlier_rounds` (reviews of
the rounds before) and, per entry of `reviews`, `absent` (`deactivated`,
`not_editor`, `cannot_see_items` or `null`; set only for a pending reviewer
who cannot decide). `POST /changes/{number}/reviewers/{old_username}/replace`
with `{"username": "<substitute>"}` replaces a pending reviewer while the
change is in review (author or admin; 409 when it is not in review or the
reviewer has decided).

## Change requests (ECR)

Every signed-in user – viewers included – raises change requests and
comments on them; CAD editors of the project groups of the affected items
(and admins) triage and decide. `GET /change-requests/{number}` returns the
request with its `items`, `comments`, `attachments`, linked `changes`,
`history` and `can` (the actions the caller may take). A request names at
most 100 affected `items` (an item named twice is listed once; more distinct items are refused with 400
before any item is looked up). See
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
| `GET/POST /projects/{code}/result-templates` | extraction templates of a group (read: whoever sees the group; write: its managers and admins): `{name, file_pattern, method: regex\|csv\|table, pattern \| csv_column [csv_key_column, csv_key_value], pick?: first\|last, unit?, lower_limit?, upper_limit?, load_case?, solver?, enabled?}` |
| `PUT/DELETE /result-templates/{id}`                          | replace (same fields) or remove a template                                                                                     |
| `GET …/revisions/{code}/simulation/proposals?status=` | values the assistant proposed from reports (`pending` default, `accepted`, `rejected`, `all`): `{proposals: [{name, value, unit, limits, load_case, source_file, source_version, quote, status, proposed_by_name}]}` |
| `POST /simulation-proposals/{id}/accept`, `…/reject`  | CAD editors of the group; accepting makes it a result value (same name and load case = update) and needs the revision checked out |

A solver script after a run:

```sh
curl -s -X POST -H "$H" $API/items/SIM-000003/revisions/B/checkout
curl -s -X PUT -H "$H" -H 'Content-Type: application/json' \
     -d '{"results": [{"name": "Max. von Mises stress", "value": 182.5, "unit": "MPa",
                       "upper_limit": 250, "load_case": "LC1", "solver": "CalculiX", "solver_version": "2.21"}]}' \
     $API/items/SIM-000003/revisions/B/simulation/results
curl -s -X POST -H "$H" -F file=@job.frd $API/items/SIM-000003/revisions/B/checkin
```

## As-built records

An as-built revision (item type `as_built`, numbers `AB-…`) records one
physical unit: the design revision it was built to, serial number, build
date, deviations and – as its regular BOM – what was really installed (see
[As-built records](../guide/as-built.md)). Writes need the as-built revision
checked out by the caller; a revision checked out by somebody else answers
`409`, a viewer or a non-member of the group `403`, and a revision or
deviation the caller may not read `404`.

| Endpoint                                              | Body / result                                                                                                                                  |
|-------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------|
| `GET …/revisions/{code}/as-built`                     | `{revision, record, summary, deviations}`; `record` is `null` until entered, has `newer_design` (text or `null`) and `restricted: true` with a placeholder when the design is hidden |
| `PUT …/revisions/{code}/as-built/record`              | `{design, design_revision?, serial_number?, built_on?: YYYY-MM-DD, note?}` – default: latest design revision; `409` when the serial number is taken for that design item, `400` for an obsolete design revision |
| `GET/POST …/revisions/{code}/as-built/deviations`     | `{kind: dimension\|material\|component\|process\|other, feature, designed?, built?, disposition?: open\|use_as_is\|rework\|repair\|scrap, note?}` → `201`; at most 1000 per revision |
| `PATCH /as-built-deviations/{id}`                     | any of the deviation fields, e.g. `{"disposition": "use_as_is"}`; the others are kept                                                          |
| `DELETE /as-built-deviations/{id}`                    | removes a deviation → `204`                                                                                                                    |
| `GET /items/{number}/built-units`                     | units built to any revision of the design item (latest revision of each unit, at most 500); hidden units are counted in `X-Hidden-Count`        |
| `POST /items/{number}/revisions/{code}/built-units`   | `{serial_number?, built_on?, number?, name?, note?, project?, with_bom?: true}` → `201` with the new as-built revision (A, checked out by the caller) |

A change order with an as-built revision cannot be submitted (`409`) while
that revision has no build record or an `open` deviation.

## Webhooks (administrators)

Outgoing event webhooks – payloads, signature and retries are described in
[Event webhooks](../admin/webhooks.md). Every endpoint needs an
administrator's token; secrets are returned only by `POST /webhooks` and
`rotate-secret`.

| Endpoint                                   | Body / result                                                                                         |
|--------------------------------------------|-------------------------------------------------------------------------------------------------------|
| `GET /webhooks`                            | `{webhooks, events}` – every webhook with `secret_hint`, `pending`, `failed_recently`, `last_status`   |
| `POST /webhooks`                           | `{name, url, events: [...], project?, active?}` → `201 {webhook, secret}`                              |
| `GET/PATCH/DELETE /webhooks/{id}`          | `PATCH` changes the given fields; `"project": null` = all project groups                               |
| `POST /webhooks/{id}/rotate-secret`        | `{webhook, secret}` – the new secret applies at once                                                   |
| `POST /webhooks/{id}/test`                 | sends a `ping` now and returns the delivery (`status`, `last_status_code`, `last_error`)               |
| `GET /webhooks/{id}/deliveries?limit=`     | latest deliveries (default 50, at most 200) with `payload`, `status`, `attempts`, `next_attempt_at`    |
| `POST /webhook-deliveries/{id}/retry`      | schedules a failed delivery again with fresh attempts and tries it at once                            |

```sh
curl -s -X POST -H "$H" -H "Content-Type: application/json" $API/webhooks -d '{
  "name": "ERP BOM export", "url": "https://erp.example.org/teamcraft/events",
  "events": ["revision.released", "change.released"], "project": "ROVER"}'
```

Errors are JSON: `{"error": {"code": "conflict", "message": "…"}}` with the
matching HTTP status (400, 401, 403, 404, 409, 413, 500).
