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

## Main resources

| Resource         | Endpoints                                                                                                                             |
|------------------|---------------------------------------------------------------------------------------------------------------------------------------|
| Items            | `GET/POST /items`, `GET /items/{number}`, where-used, history, impacts, watch                                                         |
| Revisions        | `GET/PATCH /items/{number}/revisions/{code\|latest}`, `checkout`, `checkin`, `cancel-checkout`, `revise`, `obsolete`                  |
| Files            | `POST …/files` (multipart), `GET /files/{id}` (metadata), `/content`, `/versions`, `/restore`, `/mesh` (3D preview), `/thumbnail`     |
| BOM              | `GET/POST …/bom` (`rule=latest\|released\|as_released`, `expand=true`), `PATCH/DELETE /bom-lines/{id}`, `…/package` (Pack and Go ZIP) |
| BOM import       | `POST /bom-imports` (new assembly), `POST …/bom/import` (into a checked-out revision); multipart `file` = CSV/Excel table or STEP assembly, `dry_run=true` returns the plan |
| References       | `GET/POST …/references`, `DELETE /references/{id}`, `GET /impacts`                                                                    |
| Change requests  | `GET/POST /change-requests`, `GET/PATCH /change-requests/{number}`, `comments`, `items`, `triage`, `assign`, `decide`, `reopen`, `withdraw`, `create-change`, `link-change`, `attachments`, `watch` |
| Changes          | `GET/POST /changes`, items, reviewers, `submit`, `review`, `withdraw`, `impact`                                                       |
| Projects         | `GET/POST /projects`, `GET/POST /projects/{code}/members`                                                                             |
| Notifications    | `GET /notifications`, `POST /notifications/read`, settings, watches                                                                   |
| Personal         | `GET /favorites`, `POST/DELETE /items/{number}/favorite`, `GET/POST /searches`, `DELETE /searches/{id}`                               |
| Save As, compare | `POST …/revisions/{code}/save-as` `{number?, name?, project?, files?, bom?}`, `GET …/revisions/{code}/bom/compare?with={code}`        |
| Formats          | `GET /formats` – the recognised CAD programs and formats                                                                              |

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

Errors are JSON: `{"error": {"code": "conflict", "message": "…"}}` with the
matching HTTP status (400, 401, 403, 404, 409, 413, 500).
