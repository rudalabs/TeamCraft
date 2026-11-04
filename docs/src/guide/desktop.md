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
* the *Workspace* tab shows new and modified files;
* *Check in…* uploads the selected files as new versions and releases the lock.
* on Windows with Inventor or Solid Edge installed, *STEP* (workspace) and
  the check-in option create STEP exports for the 3D preview with that
  program; the Files tab shows the preview image stored in native files.
* *Ask…* in a revision or change tab opens [Ask TeamCraft](assistant.md)
  (release notes, release check, review briefing, free questions) when the
  server has the assistant enabled.

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
