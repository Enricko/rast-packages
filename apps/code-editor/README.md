# Code Editor

RAST's Code Editor, an App Center package (`code-editor`, desktop app id
`code`). It edits the code and text files in a person's RAST files with
[CodeMirror 6](https://codemirror.net/), in a window that follows RAST's look
(light and dark).

It has no server program: it reads and writes files with RAST's files API
(`/api/files/text`, `/api/files/upload`, `/api/files/stat`, …) as the person
who opened it, and follows `file.changed` events, so what AI Agent or anyone
else changes shows as it happens.

Other apps open a file in it with `ctx.openApp("code", { path, line })`.

## Build

```sh
npm ci                                   # CodeMirror, from package-lock.json
scripts/build-ui.sh <RAST checkout>      # → ui-build/app.js + app.css
scripts/build-ui.sh <RAST checkout> --check   # type-check and test only
```

The RAST checkout is the commit in `rast-commit.txt`, with `npm ci` run in
its `frontend/`.

## Release

Bump `version` in `package.json`, add the version's notes to `CHANGELOG.md`,
then push the tag `code-editor-v<that version>`. The `code-editor` workflow
builds the window, publishes `code-editor-<version>-linux-<arch>.tar.gz`
with `SHA256SUMS` and build-provenance attestations, and the catalog
workflow lists it.
