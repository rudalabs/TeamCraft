# Event webhooks

Webhooks connect TeamCraft to the rest of the toolchain: *when a revision is
released, export the BOM to the ERP, start the CAM job, post to the
channel*. An administrator registers a URL for a set of events; TeamCraft
then sends each event as a signed JSON `POST`. The format is generic – any
CI server, ERP connector, automation tool or small script can receive it.

Webhooks are managed on the page **Webhooks** (administrators only, next to *Users*) or through
the [REST API](../reference/api.md#webhooks-administrators).
Creating, changing, deleting, rotating the secret, sending a test and
retrying are recorded in the audit log (with the receiver's host, never the
full URL or the secret).

## Events

| Event                     | Sent when                                                              | Objects in the payload           |
|---------------------------|------------------------------------------------------------------------|----------------------------------|
| `revision.released`       | a change is released – one event per released revision                 | `revision`, `change`             |
| `revision.obsoleted`      | a released revision is made obsolete                                   | `revision`                       |
| `change.submitted`        | a change is submitted for review                                       | `change`, `revisions`            |
| `change.approved`         | a reviewer approves (`review.change_status` tells whether it released) | `change`, `review`, `revisions`  |
| `change.rejected`         | a reviewer rejects – the change is rejected                            | `change`, `review`, `revisions`  |
| `change.released`         | the last approval releases the change                                  | `change`, `revisions`            |
| `change_request.created`  | a change request is raised                                             | `change_request`, `items`        |
| `change_request.decided`  | a change request is accepted, rejected or deferred                     | `change_request`, `decision`     |
| `simulation.result_added` | a result value is added, or a result table is imported                 | `revision`, `result` or `import` |
| `ping`                    | *Send test* (cannot be subscribed)                                     | `webhook`, `message`             |

A webhook can be limited to one project group: it then receives only events
about items of that group (for a change: any of its revisions; for a change
request: any affected item). The `revisions` list of a change event then
holds only that group's revisions.

[Restricted project groups](permissions.md): a webhook is treated as a
reader that sees its own group and the internal groups. A webhook for **all
groups receives no events of restricted groups**, and a webhook limited to
one group receives no events that touch another restricted group (a change
with revisions of two restricted groups goes to neither). To forward the
events of a restricted group, limit a webhook to that group. The forms warn
when restricted groups exist. `revision.released` of a revision the webhook
may see, released by a change that also holds restricted items, comes
without the `change` object. A queued delivery whose project group is
restricted before it is sent is not sent: it is marked **failed** with "Not
sent: a project group of the event was restricted after it was queued".

## Payload

Every payload has the same envelope plus the objects of the event. Objects
carry their identifiers, a `web_url` for people and an `api_url` for the
[REST API](../reference/api.md). File contents and secrets are never part of
a payload; fetch files through the API with a token of a dedicated user.

```json
{
  "event": "revision.released",
  "id": "0b7c3c4e-6a55-4b86-9c1e-3a5d2b8f1e07",
  "occurred_at": "2026-11-05T09:41:12.315Z",
  "actor": { "username": "rolf" },
  "revision": {
    "id": "5f0e…", "item_id": "a1c2…", "item_number": "PRT-000042", "item_type": "part",
    "code": "B", "label": "PRT-000042/B", "name": "Bracket", "state": "released", "project": "ROVER",
    "web_url": "https://plm.example.org/revisions/5f0e…",
    "api_url": "https://plm.example.org/api/v1/items/PRT-000042/revisions/B"
  },
  "change": {
    "id": "c9d1…", "number": "CHG-000007", "title": "Thicker flange", "priority": "normal",
    "status": "released", "author": "Erika Muster",
    "web_url": "https://plm.example.org/changes/c9d1…",
    "api_url": "https://plm.example.org/api/v1/changes/CHG-000007"
  }
}
```

`id` identifies the event (the same for every webhook that receives it).
The URLs are built from `TEAMCRAFT_PUBLIC_URL` when the request is sent. A
typical ERP connector reads `revision.api_url`, appends `/bom?rule=released`
and imports the structure.

## Request

```text
POST <your URL>
Content-Type: application/json
User-Agent: TeamCraft-Webhooks/<version>
X-TeamCraft-Event: revision.released
X-TeamCraft-Delivery: 3e1f2a9c-…            (the same for every retry of this delivery)
X-TeamCraft-Signature: sha256=<hex HMAC-SHA256 of the body, keyed with the secret>
```

Answer with any `2xx` status within 10 seconds – queue longer work (a CAM
job) instead of doing it in the request. The response body is ignored and
not stored. Redirects are not followed: configure the final URL.

## Checking the signature

The secret (`tc_whsec_…`) is shown once when the webhook is created or the
secret is rotated; afterwards only a hint (`tc_whsec_…1a2b`) is visible.
Compute HMAC-SHA256 over the **raw request body** with the secret as key (its UTF-8 bytes, prefix included) and compare
it with the header in
constant time. Reject requests whose signature does not match.

Shell (e.g. a CI step that received the body in `$BODY`):

```sh
expected="sha256=$(printf '%s' "$BODY" | openssl dgst -sha256 -hmac "$TEAMCRAFT_WEBHOOK_SECRET" | sed 's/^.*= //')"
# `[ a = b ]` stops at the first differing byte; comparing hashes of both
# values leaks nothing usable about the expected signature.
[ "$(printf '%s' "$expected" | sha256sum)" = "$(printf '%s' "$SIGNATURE_HEADER" | sha256sum)" ] \
  || { echo "bad signature" >&2; exit 1; }
```

A shell variable cannot hold every body byte for byte (command
substitution drops trailing newlines, NUL bytes are lost); for anything
beyond a quick check, verify the raw body in a real program.

Python:

```python
import hashlib, hmac

def verified(secret: str, body: bytes, header: str) -> bool:
    expected = "sha256=" + hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()
    # bytes, not str: compare_digest raises TypeError for non-ASCII text
    return hmac.compare_digest(expected.encode(), header.strip().encode())
```

Rust (crates `hmac`, `sha2` and `hex`):

```rust
use hmac::{Hmac, Mac};

fn verified(secret: &str, body: &[u8], header: &str) -> bool {
    let Some(hex_sig) = header.strip_prefix("sha256=") else { return false };
    let Ok(sig) = hex::decode(hex_sig) else { return false };
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).expect("any key length");
    mac.update(body);
    mac.verify_slice(&sig).is_ok() // constant time
}
```

Replays: remember the `X-TeamCraft-Delivery` ids you processed and skip
repeats; the payload's `occurred_at` lets you drop very old requests.

## Retries and delivery order

A delivery that does not get a `2xx` answer (error status, timeout, refused
connection) is retried after about 1, 2, 4, 8, 16, 32 and 64 minutes – each
delay with up to 25 % jitter – and marked **failed** after 8 attempts (roughly two hours). The webhook's page lists the
latest deliveries with
status, attempts, HTTP status and the last error, shows each payload, and
has **Retry** (a fresh set of attempts, tried at once) and **Send test**.

Deliveries are written in the same database transaction as the event
itself – an action that fails or is rolled back sends nothing, and a
committed one is never lost, even if the server restarts before sending.
With several TeamCraft servers on one database each delivery is claimed by
exactly one of them (`FOR UPDATE SKIP LOCKED` with a lease of two minutes;
a sender that crashes mid-request is retried after the lease). Deliveries
can arrive out of order and, after a crash, a lost response or a *Retry*
while an attempt is still running, more than once – use `occurred_at` and
the delivery id.

Switching a webhook off gives up its waiting deliveries; deleting it removes
its history. Sent and failed deliveries are removed after
`TEAMCRAFT_WEBHOOK_RETENTION_DAYS` (default 30).

## Where webhooks may send to

Only `http://` and `https://` URLs are accepted, without a user name or
password in the URL (the signature authenticates TeamCraft; a token of the
receiver may go into the query string). To keep webhooks from being used to
reach internal services (server-side request forgery), TeamCraft checks
every address it connects to, at connection time and after DNS resolution:

* **Default:** public addresses only. Loopback (`127.0.0.0/8`, `::1`),
  private (`10/8`, `172.16/12`, `192.168/16`, `fc00::/7`), shared (`100.64/10`), link-local (`169.254/16` including
  cloud metadata
  endpoints, `fe80::/10`) and similar ranges are refused, also when they
  are written as IPv4-mapped, NAT64 or 6to4 IPv6 addresses.
* **`TEAMCRAFT_WEBHOOK_ALLOWED_HOSTS`** (recommended for on-premises
  installations): only the listed hosts can be targeted, and they may
  resolve to internal addresses – e.g. `erp.corp.example,*.ci.corp.example`.
* **`TEAMCRAFT_WEBHOOK_ALLOW_PRIVATE=true`:** every host may be internal.
  Use it only when nothing sensitive is reachable from the server.

The unspecified address, broadcast and multicast are always refused. Proxy
variables (`HTTPS_PROXY`, …) are ignored for webhooks so the check applies
to the real target. See [Configuration](configuration.md#event-webhooks).

## How the secret is stored

The server needs the secret itself to sign, so – unlike passwords and API
tokens, which are stored as hashes – it is kept in the `webhooks.secret`
column of the database. It is never shown again in the web UI or returned by
the API, and it is not written to logs or the audit trail. Anyone with read
access to the database or to a [backup](operations.md) can sign payloads, so
protect both like the vault. Rotate the secret on the webhook's page after a
suspected leak; the new secret applies at once, also to deliveries that are
still waiting.
