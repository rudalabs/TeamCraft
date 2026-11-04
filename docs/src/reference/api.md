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
| `POST /uploads`                 | `{item, revision, filename, size, sha256?, content_type?, base?}` → **201** with the session; 413 above `TEAMCRAFT_MAX_UPLOAD_MB`, 409 if the revision is not checked out by you or `base` is outdated |
| `PATCH /uploads/{id}`           | header `Upload-Offset`, body = the next chunk (≤ `chunk_bytes`) → the session with the new `offset`; **409** with the server's `Upload-Offset` header at any other offset, 400 beyond `size` |
| `HEAD`/`GET /uploads/{id}`      | headers `Upload-Offset`, `Upload-Length`; `GET` also as JSON (`offset`, `size`, `complete`, `expires_at`)                                                                       |
| `POST /uploads/{id}/finish`     | body optional `{comment?, check_in?}`: verify size and SHA-256, store the new file or version (`check_in: true` also releases the check-out) → `{files, revision}`; 409 while incomplete, 400 if the content does not match (the upload is reset to offset 0) |
| `DELETE /uploads/{id}`          | cancel; the received data is deleted                                                                                                                                              |

Instead of `finish`, complete uploads can join a multipart **check-in** or
upload: the field `uploads` lists their ids (`["…", "…"]` or `id,id`), and
they are stored together with the `file` parts in one transaction – the
desktop app checks in this way. A session's own `base` wins over the
`bases` field. Sessions are private (other users get 404), expire 24 hours
after the last chunk and are limited to 16 unfinished ones per user. When
storing fails after the content was verified (for example a 409 because the
file moved on), the session keeps the verified content, so `finish` can be
repeated without sending the bytes again.

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
| Files            | `POST …/files` (multipart), `GET /files/{id}` (metadata), `/content`, `/versions`, `/restore`, `/mesh` (3D preview), `/thumbnail`     |
| Uploads          | `POST /uploads`, `PATCH/HEAD/GET/DELETE /uploads/{id}`, `POST /uploads/{id}/finish` – chunked, resumable uploads of large files        |
| BOM              | `GET/POST …/bom` (`rule=latest\|released\|as_released`, `expand=true`), `PATCH/DELETE /bom-lines/{id}`, `…/package` (Pack and Go ZIP) |
| BOM import       | `POST /bom-imports` (new assembly), `POST …/bom/import` (into a checked-out revision); multipart `file` = CSV/Excel table or STEP assembly, `dry_run=true` returns the plan |
| References       | `GET/POST …/references`, `DELETE /references/{id}`, `GET /impacts`                                                                    |
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

Errors are JSON: `{"error": {"code": "conflict", "message": "…"}}` with the
matching HTTP status (400, 401, 403, 404, 409, 413, 500).
