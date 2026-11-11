# Crash reports from the desktop app

Question: how the desktop app (Tauri 2, Rust + system web view) can notice
crashes and get the reports to the developers. It must do this without
sending anything the user has not seen, and without a hosted service that an
open-source project would have to run. Date: 2026-11-10.

## What can crash

| Where                          | How it shows                                                                    | Can the app catch it?                                                                                            |
|--------------------------------|---------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------|
| Rust (commands, workers)       | a `panic!`; in an async command the task dies, in the main thread the app exits | yes: `std::panic::set_hook` sees every panic before unwinding, with message, location and a backtrace            |
| the web view's JavaScript      | an uncaught exception or unhandled promise rejection; the UI breaks silently    | yes: `window` `error` and `unhandledrejection` events                                                            |
| native code (web view, GTK, …) | the process dies (segfault, abort), the OS may show its own dialog              | not in-process; only afterwards, by noticing that the last session did not end normally                          |
| killed or power loss           | same as a native crash                                                          | same; cannot be told apart from a native crash                                                                   |

## Options

| Option                                                                                  | Pros                                                                     | Cons                                                                                                                                                                                                                                    |
|-----------------------------------------------------------------------------------------|--------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| **Sentry** (SaaS or self-hosted; `sentry` crate, MIT)                                   | full product: grouping, symbolication, minidumps via the Crashpad handler | needs a DSN baked into the app and a service someone pays for or runs; uploads in the background by default (consent has to be bolted on); the self-hosted stack is large; an open-source build would carry the project's DSN to everyone |
| **Crashpad / Breakpad minidumps** (`minidumper`, `crash-handler` crates, MIT/Apache-2.0) | catches native crashes too (out-of-process handler)                      | needs symbol files per release and a server to process dumps; minidumps contain memory (data the user did not choose to share); a second process to ship and sign                                                                      |
| **Upload to the user's own TeamCraft server**                                           | no third party                                                           | goes to the server's administrator, not to the developers; mixes product bugs with tenant data                                                                                                                                          |
| **Panic hook + session marker + UI error events, shown to the user, sent as a GitHub issue they open themselves** | no service, no DSN, nothing leaves the computer unseen; works offline; the report is plain text the user can read and edit | no native stack for web-view crashes (only "the last session ended unexpectedly"); developers depend on the user filing the issue; GitHub URLs cap the prefilled text (the full report goes to the clipboard)                          |

## Decision

Take the last option. It fits a no-release, open-source desktop client with
no hosted backend, and it matches the project's rule that nothing is uploaded
on its own (the update check only reads `api.github.com`).

- **Rust panics.** A panic hook, installed first thing in `main`, writes one
  report per panic to `<config>/crash-reports/`. It records the message,
  location, thread and a backtrace, then calls the previous hook.
- **Abnormal exit.** A session marker is created at start and removed on a
  normal exit (`RunEvent::Exit`). A marker that is still there at the next
  start becomes an "ended unexpectedly" report with the version and start
  time of that session. This is the only trace of native crashes.
- **Uncaught UI errors.** Only real JavaScript exceptions (`Error`
  instances) are recorded, deduplicated and capped per session. Rejected
  command calls (server errors such as 404 or 403) are not crashes and are
  not recorded.
- **Redaction before writing.** The home directory becomes `~`, URLs become
  `[url]`, and TeamCraft tokens (`tc_pat_…`, `tc_sess_…`) become `[token]`.
  The report never contains the server address, user name, token or file
  contents. The store keeps at most 20 reports of at most 64 KiB each.
- **Start-up prompt.** At the next start a dialog shows the whole report and
  offers three choices:
  - *Report on GitHub*: opens a prefilled new issue at
    `github.com/rudalabs/TeamCraft`; the full text is copied to the clipboard
    in case the URL had to cut it.
  - *Copy*.
  - *Don't send*: deletes the reports.

  *Don't ask again* turns the prompt off in Settings. The reports are still
  written locally and can be deleted.
- **Later, if needed.** Minidumps for native crashes (`crash-handler` +
  `minidumper`), only with an explicit opt-in and a place to send them.
