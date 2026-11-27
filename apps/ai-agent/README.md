# AI Agent

RAST's agent harness as an App Center app package (`ai-agent`, app id
`agent`): chat with the models an administrator connects, with tools, files,
a web browser, artifacts, a viewer for the pictures, videos and PDFs a chat
names, plan sign-ins with several accounts per provider and a quota tracker. RAST installs it from App Center, runs its server program
next to itself and opens its window with its window SDK, so AI Agent updates
without a RAST release.

## Layout

| Path | What |
| --- | --- |
| `src/` | The server program (`rast-ai-agent`) and its library (`rast_ai_agent`): providers, sign-ins, runs, tools, the sandboxed browser, files, prices; `src/api/ai.rs` is its HTTP API (`/api/ai/*`, [docs/API.md](docs/API.md)) |
| `migrations/` | Its database (`agent.db` in the package's data folder) |
| `ui/` | The window, written against RAST's frontend (`src/core` is the window SDK) |
| `ui-tests/` | The window's unit tests |
| `tests/agent_e2e.rs` | End to end: the API beside a real RAST in one program, and the program run by RAST's app host |
| `scripts/build-ui.sh` | Builds `app.js` and `app.css` with a RAST checkout's frontend |

The crate builds against the RAST commit pinned in `Cargo.toml`
(`rast = { git, rev }`): RAST's library gives it accounts, access rules, the
audit log, the sandbox, events and the host API. `src/lib.rs` re-exports
those modules, so the code reads as it did inside RAST (`crate::db`,
`crate::auth`, `ai::…`).

## Develop

```sh
cargo build
cargo test --all-targets
scripts/build-ui.sh <RAST checkout>          # ui-build/app.js + app.css
scripts/build-ui.sh <RAST checkout> --check  # type-check and test only
```

The RAST checkout for the window needs `npm ci` in its `frontend/` and should
be at the pinned commit. To work on AI Agent and RAST together, point the
crate at a local RAST in a git-ignored `.cargo/config.toml`:

```toml
[patch."https://github.com/Enricko/rast"]
rast = { path = "/path/to/rast" }
```

(Cargo then records the path in `Cargo.lock`; drop the patch and pin the
pushed RAST commit before committing.)

## Release

1. Bump `VERSION` in `src/app.rs` and `version` in `Cargo.toml`.
2. Add the version's notes to `ui/whats-new.ts` (shown once per version in the
   window) and to [CHANGELOG.md](CHANGELOG.md).
3. Push the tag `ai-agent-v<version>`. The `ai-agent` workflow tests it,
   builds the window and the program for x86_64 and aarch64, attests them and
   publishes the release.
4. The catalog workflow lists it (`packages.ai-agent.releaseRepos` in
   `catalog.config.json`); servers then offer the update in App Center and in
   AI Agent's own window.

Third-party notices: [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
