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

| Resource | Endpoints |
|---|---|
| Items | `GET/POST /items`, `GET /items/{number}`, where-used, history, impacts, watch |
| Revisions | `GET/PATCH /items/{number}/revisions/{code\|latest}`, `checkout`, `checkin`, `cancel-checkout`, `revise`, `obsolete` |
| Files | `POST …/files` (multipart), `GET /files/{id}` (metadata), `/content`, `/versions`, `/restore`, `/mesh` (3D preview) |
| BOM | `GET/POST …/bom` (`rule=latest\|released\|as_released`, `expand=true`), `PATCH/DELETE /bom-lines/{id}`, `…/package` (Pack and Go ZIP) |
| References | `GET/POST …/references`, `DELETE /references/{id}`, `GET /impacts` |
| Changes | `GET/POST /changes`, items, reviewers, `submit`, `review`, `withdraw`, `impact` |
| Projects | `GET/POST /projects`, `GET/POST /projects/{code}/members` |
| Notifications | `GET /notifications`, `POST /notifications/read`, settings, watches |
| Formats | `GET /formats` – the recognised CAD programs and formats |

Errors are JSON: `{"error": {"code": "conflict", "message": "…"}}` with the
matching HTTP status (400, 401, 403, 404, 409, 413, 500).
