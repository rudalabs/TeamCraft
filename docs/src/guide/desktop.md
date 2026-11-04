# Desktop app

TeamCraft Desktop (`desktop/`, Tauri 2) is a rich client for Linux, macOS and
Windows: a navigator tree of the project groups, object tabs (overview,
files, structure, where used, changes, history, 3D viewer), a worklist with
check-outs and reviews, and a **local workspace**:

* *New* (`Ctrl/Cmd+N`) creates an item together with its files: choose the
  CAD files first, name and type follow from the first one;
* *Check out* takes the lock and copies the files into a folder per project
  group and revision, `<workspace>/<group>/<item>/<revision>/`;
* *Get…* (*Tools → Get Project Groups…*) copies the files of every item of
  chosen project groups into the workspace – for new team members;
* *Import…* (*Tools → Import CAD Folder…*) brings an existing CAD folder in:
  scan first (nothing is uploaded), then one item per part or assembly with
  its drawings and exports; *Help → How to Import a CAD Folder* is the
  step-by-step tutorial (Pack and Go, unique file names, check and release);
* the *Workspace* tab shows up-to-date, modified, outdated, conflicting, new
  and missing files and who holds the lock for how long; opening an outdated
  file or one somebody else has checked out asks first;
* *Check in…* uploads the selected files as new versions and releases the
  lock – based on the versions they were copied from, so a version somebody
  else checked in meanwhile is never overwritten (see
  [Concurrency](vault.md#concurrency-nothing-is-overwritten-silently));
* *Check in all* in the worklist checks in every check-out without local
  changes and lists those that still have changes.
* on Windows with Inventor or Solid Edge installed, *STEP* (workspace) and
  the check-in option create STEP exports for the 3D preview with that
  program; the Files tab shows the preview image stored in native files.
* *Ask…* in a revision or change tab opens [Ask TeamCraft](assistant.md)
  (release notes, release check, review briefing, free questions) when the
  server has the assistant enabled.
* *Agent* (rail, *View → Agent*, `Ctrl/Cmd+4`) works with a local AI agent
  that you set up under *Settings → Agents…*, for example Claude Code
  through its ACP adapter or Gemini CLI. None is set up by default. The app
  starts the agent in the workspace folder and gives it TeamCraft's tools
  with your rights. The conversation streams in a tab of its own, with tool
  calls, the agent's plan, permission prompts (*Allow once* / *Reject*),
  *Cancel* and quick prompts (release notes, release check, change review,
  worklist triage). Through the app the agent may only read text files in
  the workspace folder; it gets no write access and no terminal. See
  [ACP agents](assistant.md#acp-agents-in-the-desktop-app).

Sign-in runs through the browser (single sign-on or password, confirmed on a
TeamCraft page) or with a personal access token. The app uses the same skins
and 3D viewer as the web interface; the models are converted by the server.

## Run it

```sh
cd desktop
cargo run            # Linux needs the WebKitGTK development packages
```

The platform guides list the system packages. Installers are not published
yet; the app is built and tested from source.
