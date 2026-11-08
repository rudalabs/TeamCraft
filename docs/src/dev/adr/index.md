# Architecture decision records

Decisions that keep coming back in reviews, written down once: the context,
the decision, its consequences, the alternatives that were turned down, and
where the code and the tests that hold the decision live. Each record only
states what the repository documents (code, tests, research notes, commit
messages, `docs/OPEN_WORK.md`); where the reason for a choice is not written
down anywhere, the record says so instead of guessing.

| No. | Decision | Status |
|-----|----------|--------|
| [0001](0001-access-model.md) | Read access: a deny list on the actor, "not found" for what is hidden, enforced in the services | accepted (#52, #55) |
| [0002](0002-outbox.md) | Notifications and webhooks are an outbox written in the event's transaction | accepted |
| [0003](0003-content-addressed-vault.md) | Files live in a content-addressed vault on the local file system | accepted |
| [0004](0004-no-node.md) | No Node.js and no bundler in the product | accepted |

A new record gets the next number, the same headings and a row here. A
decision that is replaced keeps its record with the status *superseded by
NNNN*.
