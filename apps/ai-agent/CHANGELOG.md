# Changelog — AI Agent

AI Agent is RAST's agent harness, an App Center app package (`ai-agent`,
app id `agent`). Versions follow `VERSION` in `src/app.rs`; each release is
the tag `ai-agent-v<version>` in this repository. What each version brings
to people is also in `ui/whats-new.ts`, shown once per version.

## [Unreleased]

### Added

- **The agent works in apps.** A new `use_app` tool works inside an app
  whose window takes commands (the Craft apps that carry ArtCraft's page
  command API: WordCraft, PdfCraft, CADCraft, GridCraft, FigCraft,
  DeckCraft, FilmCraft, VectorCraft): it lists and runs the app's commands,
  reads the open document and the window's state, types, presses keys,
  clicks and takes a picture of the window (shown in the chat). The call
  runs in the person's page: the page that started the reply hands it to
  the app's window (RAST 0.9.20's `ctx.desktop.request`, which opens the
  app when needed) and posts the answer
  (`POST /api/ai/conversations/{id}/tools/{callId}/answer`); the reply
  waits up to 150 seconds. Reading asks nothing; typing, commands and
  clicks ask first, like other tools that act ("Allow files, the web and
  apps" and "Allow everything" include it). The model is offered it when
  RAST's host API marks an app's window as taking commands.
- Which chats a page started is kept for the browser tab, so after a
  reload the page still opens the apps and runs the app work of its
  replies.

### Changed

- `open_app` takes a path only for File Station and apps that open files
  (`opens`); for another app it says the app opens no file from a path
  instead of reporting the file open. The app list tells which apps
  `use_app` works in.

## [1.7.0] - 2026-11-28

### Added

- **Workspaces.** A workspace is a folder of the person's RAST files, or for
  administrators a folder on the server. The chat list shows one
  workspace's chats or all of them, a new chat starts in the one shown,
  and the chip under the message box moves a chat to another. In a
  workspace the file tools reach only inside its folder (a relative path
  is inside it), commands start in it, and the model is told about it: its
  top-level entries and its notes (AGENTS.md, CLAUDE.md or README.md).
  Removing a workspace keeps its chats and the folder. agent.db migration
  0004.
- **The agent opens RAST apps.** A new `open_app` tool opens an app the
  person may open: Code Editor at a file and line, File Station at a
  folder, or any other app. It changes nothing, so it runs without asking;
  the page that sent the message opens the app, and the step's card has
  Open to open it again. The model is told which apps the account may open
  (RAST's host API `GET /v1/users/{id}/apps` from RAST 0.9.19, else the
  built-in apps).
- **Ask AI from other apps.** The window parameter `{ ask }` starts a new
  chat with that text in the message box, for the person to finish and
  send (RAST's apps and Code Editor use it). File cards offer "Open in
  <app>" for the apps that open that kind of file.

### Changed

- **The usage bar shows every provider's limits.** Antigravity and Gemini
  CLI accounts say what each pool of models has left (the catalog's
  `quotaInfo`), OpenRouter keys their limit and credits, and the bar asks
  again for accounts last told over two minutes ago. A provider that
  reports no limits shows what RAST counted for it (last 5 hours, 24
  hours, 7 days). The chat model's pool comes first.

### Fixed

- The window keeps what the events said when a reply (or a refusal) ends
  before the request that started it answers: it stayed on "Thinking"
  before.

## [1.6.0] - 2026-11-28

### Added

- **A bar under the message box.** What the chat used (messages, tool
  steps, the last reply's tokens per second; tokens and the share of input
  read back from the provider's cache), the limits of the provider that
  answers (each window's time left and share used: "Codex 4h 59m 0% · 29m
  79%"; a click lists every provider's with bars), and a ring of how full
  the model's context is (by the model's family, so "about"). Replies keep
  `cacheReadTokens`, `contextTokens` (the last step's input and output) and
  `writingMs` (agent.db migration 0003); `GET /api/ai/quota` gives the
  limits to everyone who chats, account names to administrators and plan
  owners only.
- **Chats named by their first answer.** Once the model's first step is
  written, the same model names the chat in a few words, in a short request
  of its own (counted as the owner's usage, its estimate added to the
  reply's), so the list says what each chat is doing. A name the owner gave
  stays; when naming fails, the first line of the message stays the title.

### Changed

- **One chip for the model and the effort** ("GPT-5 Low"), with a menu:
  Model › opens a list to search, grouped by provider with its logo (the
  provider's name stays on top while its models scroll under it; arrow keys
  and Enter pick); Effort › the levels with what each does. Popovers open
  inside AI Agent's window.

## [1.5.0] - 2026-11-28

### Added

- **The panel beside the chat has tabs.** Artifacts, files and web pages a
  chat names each open in a tab (as in a browser): a type badge (PDF, W,
  XLS, PPT, MD, CSV, …) or the site's icon, × to close, **+** to open a path
  or address, **Fill** and **Hide**. A path bar shows where it is, its kind,
  Preview/Source and Reload, Copy, Download, Open in a new tab and Show in
  File Station. Tabs keep their view while open, and each chat keeps its
  tabs (with the window, and while another chat is in front).
- **Documents as pages.** Word, Excel, PowerPoint, OpenDocument and RTF files
  show as pages: LibreOffice on the server turns them into PDFs, in a locked
  systemd unit (no network, no files of the server), kept a week per account.
  `POST /api/ai/view/convert`, `GET /api/ai/view/converted/{id}`. When
  LibreOffice or a part of it is missing, the tab says which and offers
  administrators **Open App Center** (Server components → LibreOffice).
- **Every other file.** Markdown rendered, CSV/TSV as a table, JSON
  formatted, HTML as a walled-off page (`GET /api/ai/view/html`), text and
  code with line numbers; other files offer Download and File Station.
  Text and documents on the web come through `GET /api/ai/view?as=text|document`.
- **Web pages.** A link opens in a tab: in a sandboxed frame where the site
  allows one (with RAST 0.9.19), else as a card with its picture, title,
  description and Open in a new tab (`GET /api/ai/view/page`).
- **File cards.** Each file a reply wrote gets a card under it: its type,
  name and folder, Open, and Download / Show in File Station.

## [1.4.0] - 2026-11-28

### Added

- **Replies try again after a passing failure.** When the provider times
  out, cannot be reached, breaks off, or answers that it is busy or failing
  (HTTP 408/500/502/503/504/52x/529, `overloaded_error`, `server_error`,
  `UNAVAILABLE`), the reply waits 30 seconds and tries again, up to 10 times
  in a row (5 minutes). The chat shows "Retrying 1/10 in 30s" counting down,
  with what failed and **Retry now**; Stop ends the wait. What a failed try
  had written is taken back before the next one. `POST
  /api/ai/conversations/{id}/retry`; the waiting reply's message carries
  `retry`.
- **A viewer beside the chat.** Pictures, videos and PDFs named in a chat
  open in a panel on the right, in the artifact panel's place, with
  Download, Open in a new tab, Show in File Station and Close: links in an
  answer, paths in the person's files (`/shared/team/q3.pdf`), a finished
  `read_file`/`write_file` call (**View**), and pictures and PDFs sent in
  the chat. Files open with the person's own access; web addresses go
  through `GET /api/ai/view` (public addresses, media types, ranges,
  512 MB). The agent is told to write paths so they open there. PDFs sent in
  the chat open inline (sandboxed).

### Fixed

- Requests to providers and sign-in pages say RAST's version again
  (`RAST/0.9.17`), not AI Agent's, as before the move.

### Changed

- The source moved from RAST's repository to this one (`apps/ai-agent`):
  a crate built against a pinned RAST commit (`rast` in `Cargo.toml`), the
  window in `ui/`, its tests in `tests/` and `ui-tests/`, and its release
  workflow (`.github/workflows/ai-agent.yml`).

## [1.3.0] - 2026-11-27

### Added

- **AI Agent updates from its own window.** When App Center has a newer AI
  Agent, administrators get "AI Agent x is ready · Update" under the chat
  list; Update installs it with its progress as a bar, and the desktop opens
  the window again on the new version, where it was and without a page
  reload. Any Package Center app's open windows now follow an update this
  way, from App Center too.
- **AI Agent: What's new, once per version.** After an update each account
  sees what changed, in a dialog over the window (`POST /api/ai/whats-new`;
  `/api/ai/options` says the version).
- **AI Agent settings → Quota: bars on every card, and limits per account.**
  Accounts whose provider reports nothing show what RAST counted in the last
  5 hours and 7 days as bars against limits set in RAST (Limits on the card
  or in the account dialog, `PUT
  /api/ai/providers/{id}/accounts/{account_id}/limits`, kept in settings);
  at a limit the account rests and the next account answers. Without a limit
  set, the bar measures against about what RAST had counted when the
  provider last said the account was out, else beside the provider's busiest
  account. Per-model use shows as bars too, and ChatGPT Codex plans are asked
  for their limits as Quota opens. Ships as AI Agent 1.3.0.

## [1.2.0] - 2026-11-27

### Added

- **AI Agent: Round robin as a switch, for one provider or all.** A
  provider's dialog has 9router's Round robin switch with Sticky (replies in
  a row per account), applied at once; **Round robin for all** (Providers
  and Quota) turns it on or off for every provider and for the ones added
  later. Ships as AI Agent 1.2.0 with the quota tracker below.
- **AI Agent settings → Quota, a quota tracker like 9router's**: filters by
  provider, each provider's accounts under it with its Round robin switch,
  and a card per account with Refresh (ChatGPT Codex plans are asked for
  their 5-hour and weekly use, as `codex /status` shows it), Edit, Remove
  and an on/off switch. Cards show the limits the provider reports as bars
  coloured by what is left, with the counts and "Resets in …", the
  account's rest with Use now, and for providers that report nothing, the
  tokens RAST counted per model in the last 5 hours and 7 days. Refresh all
  and Auto-refresh (every minute) ask the providers again.

## [1.1.0] - 2026-11-27

### Added

- **AI Agent: one provider, several accounts.** A provider's dialog lists
  its accounts with Add account (sign in with another plan account, or
  paste another API key). Replies take the accounts by the provider's turn
  order: **Fill first** uses the first account and moves to the next when
  it reaches a limit; **Round robin** takes them in turn (a set number of
  replies each), so they reach their limits together. An account whose
  provider answers that it reached a limit rests until the reset time the
  provider gives (one minute when it gives none) and the reply goes on with
  the next account. Each account can be set aside, moved up or down, signed
  in again, woken with Use now, or removed. Sign-ins of one person to the
  same plan become accounts of one provider on upgrade (the two Google
  Antigravity sign-ins become one provider with two accounts); chats,
  agents, limits and usage follow. Database schema v21; ships to App Center
  as AI Agent 1.1.0 (package `ai-agent`, its own database v2).

### Fixed

- AI Agent settings → Providers: a long account email stays inside its card
  (shortened with "…") instead of running into the next card, and the
  models count stays on one line. A provider with several accounts says how
  many.

## [1.0.0] - 2026-11-26

### Added

- AI Agent as an App Center app package: its own server program
  (`rast-ai-agent`) and database (`agent.db`), run by RAST's app host, and
  its window built with RAST's window SDK. On its first start it takes the
  AI Agent data RAST kept. Earlier changes shipped inside RAST (see RAST's
  changelog up to 0.9.17).
