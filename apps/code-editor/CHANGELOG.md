# Changelog — Code Editor

## 1.0.0

- A folder of your RAST files on the left (open any folder, recent folders,
  new file, new folder, rename, move to the recycle bin), files in tabs.
- CodeMirror 6 with syntax highlighting for 45 languages, search and
  replace, go to line, find a file by name (Ctrl+P), comment toggling,
  bracket matching, folding and multiple cursors.
- Changes made elsewhere (by AI Agent, File Station or another device) show
  as they happen; when you have unsaved changes it asks which to keep.
- Unsaved changes stay in the browser until saved, so closing the window or
  reloading loses nothing.
- Keeps each file's indentation and line endings (LF or CRLF).
- Opens a file at a line for other apps: `openApp("code", { path, line })`.
