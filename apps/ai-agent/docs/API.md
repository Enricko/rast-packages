# AI Agent API (app id `agent`)

RAST's own agent harness (this crate's [`src`](../src)). The app starts uninstalled:
an administrator installs it in App Center (`POST /api/packages/apps/agent/install`);
until then every `/api/ai/*` request is refused (`403`), administrators'
included. Accounts need the app granted (group or account rule) unless they
are administrators.

Administrators set up **providers** (several of one kind are fine),
**skills** (instructions) and
**agents** (personality, provider and model, skills). API keys are encrypted
with the server secret and never returned: providers show `hasKey` and
`keyHint` (`…` and the last four characters). **Limits** decide which
providers an account may use and how much it may use per day, per week
(Monday to Sunday) and per month (server time), in tokens and in money
(USD): the account's own rule (`user:<id>`), else its groups' rules combined
with the most generous winning, limit by limit (`group:<id>`), else the
`default` rule, else no limits. Administrators are not limited. Usage counts
the tokens the provider reports (about four characters per token when it
reports none) and stays counted when a chat is deleted. **Money** comes from
each model's price, a `Price` of `{input, output, cached?, cacheWrite?,
tiers?, multipliers?}` in USD per million tokens (`cached`: input the
provider reads back from its prompt cache; `cacheWrite`: input written to
it; both the input price when absent; reasoning counts as output; `[input,
output]` is still read). `tiers` are other prices for long prompts,
`[{above, input, output, cached?, cacheWrite?}]` (more than `above` input
tokens; the highest that applies wins). `multipliers` are factors on the
whole price at certain times, all that apply multiplied: `[{factor, days?,
hours?, from?, until?}]` with `days` such as `["mon", …]`, `hours` such as
`[["01:00", "04:00"]]` (UTC, the second excluded, across midnight when it is
earlier) and `from`/`until` dates (`YYYY-MM-DD`, `until` excluded); OpenCode's
`cost.multipliers` and `input`/`output` `overrides` are read into these. A
reply is priced when it is recorded. A
model's price on a provider is, first to last: the one an administrator set
there (`prices`), the one the provider's own model list gives (`listPrices`:
OpenCode's `cost`, OpenRouter's or Vercel's `pricing`; kept when the models
are loaded and asked again daily, ten minutes after start then every half
hour for providers not asked for a day), then the built-in list (9router's
price table with Anthropic's published Claude prices; names compared without
case, a vendor prefix, a variant, a date or an effort, then `*` patterns,
first match wins; `…:free` and `…-free` cost nothing). Local providers
(Ollama, LM Studio, vLLM, llama.cpp) skip the built-in list: free unless
priced. Each reply's cost is stored when it is made (in micro-dollars, with
its cached input), so later price changes do not rewrite it; a model without
any price costs nothing. A new reply is refused (429, with `Retry-After`)
once any limit is reached; a reply already running finishes.

Provider `kind`s are presets of an address and protocol (`kinds` in the
admin overview lists them with `group`, `keyUrl`, `about`, and for the plan
kinds also `signsIn:true`, `signInFlow`, `site`, `callbackHint` and `risk`;
the other kinds have `signsIn:false` and the rest `""`):

| Group | Kinds |
| --- | --- |
| model makers | `anthropic` (Anthropic Messages), `openai`, `gemini` (AI Studio's OpenAI-compatible endpoint), `xai`, `deepseek`, `mistral`, `moonshot`, `zai`, `qwen` (Model Studio), `minimax` |
| plans (signed in, no key) | `codex` (ChatGPT Codex), `claude` (Claude Pro/Max), `grok` (Grok with X Premium), `copilot` (GitHub Copilot), `antigravity` (Google Antigravity), `gemini-cli` (Gemini CLI), `qwen-plan` (Qwen Chat), `iflow` (iFlow), `kimi-plan` (Kimi) |
| gateways | `opencode` (OpenCode Zen, pay as you go), `opencode-go` (the OpenCode Go subscription), `openrouter`, `vercel` (AI Gateway), `huggingface` (Inference Providers) |
| fast open models | `groq`, `cerebras`, `together`, `fireworks`, `nvidia` (NIM) |
| on your network (no key needed) | `ollama`, `lmstudio`, `vllm`, `llamacpp` |
| custom (base URL required) | `custom` (OpenAI chat completions), `custom-anthropic` (Anthropic Messages; the key goes as `x-api-key` and `Bearer`) |

All but the Anthropic ones speak OpenAI chat completions. The OpenCode kinds
serve each model on its own protocol under `{base}/openai/v1` and
`{base}/anthropic/v1`: RAST reads `{base}/v1/models` (every 6 hours) and sends
models whose `recommended_api` is `messages` (Claude, some Qwen and MiniMax)
over Anthropic Messages and the rest over chat completions (which OpenCode
translates for GPT and Gemini); their model list leaves out models that are
not for chat and free ones (OpenCode serves those only to its own apps).
Requests to them carry the chat's id as `x-opencode-session`, which OpenCode Go
requires. A server that refuses `stream_options` is asked again without it.

**Signing in with a subscription plan.** The nine `plan` kinds are a
person's own subscription instead of an API key: they are not added with a
key, only by signing in (Claude can also import the Claude Code login the
server already has). A browser flow (`signInFlow:"browser"`) opens the
provider's page (with PKCE S256 where the provider's flow takes it, and a
`state` that is checked); the person allows it and
pastes back the address the browser lands on (a page that does not open on
the server) or, for Claude, the `code#state` its page shows. A device flow
(`signInFlow:"device"`) shows a `userCode` to type at the provider's own
site, and the page polls RAST for the tokens every `interval` seconds for
up to `expiresIn` seconds. A started sign-in waits 20 minutes in memory and
is answered once, only by the account that started it. The tokens are
stored encrypted with the server secret, never returned or logged, and
refreshed by RAST ahead of expiry, one refresh at a time (most providers
rotate the refresh token, and using an old one ends the session). The
provider it adds (or signs in again) is owned by the signer: `ownerId` and
`signedIn:{email, plan, expiresAt}` in the admin views (never the tokens),
usable by that account alone (administrators included), and its replies are
priced `plan` ("In your plan") rather than from RAST's price list. Its
models are the provider's live catalog read with the new tokens (a short
fallback list when the catalog cannot be read) and are listed by
`POST /api/ai/providers/models {providerId}`. Signing out clears the tokens
and the owner but keeps the provider and its models, so it can be signed in
again. Each provider sets its own line on using a plan outside its own app:
the `risk` note the settings page shows says so. Antigravity, Gemini CLI and
iFlow read their OAuth application client from the server's environment
(`RAST_ANTIGRAVITY_CLIENT_*`, `RAST_GEMINI_CLI_CLIENT_*`, `RAST_IFLOW_CLIENT_*`,
see RAST's [CONFIGURATION.md](https://github.com/Enricko/rast/blob/main/docs/CONFIGURATION.md)); without it, starting that one sign-in
is a `400` naming the variables, and the other plans still work.

**Effort.** A chat's `effort` (`""` = the provider's default, `low`,
`medium`, `high`, `max`) goes out as `output_config.effort` over Anthropic
Messages, `reasoning: {effort}` to OpenRouter and `reasoning_effort` to every
other chat-completions server (`max` is `xhigh` for OpenAI and `high`
elsewhere). A model that refuses the level (a 400/422 naming effort or
reasoning) is asked again with `max` lowered to `high`, then without it.

**Tools.** Agents may act for the account chatting, never with more rights
than it has ([`src/ai/tools.rs`](../src/tools.rs)):

| Tool | What | Who |
| --- | --- | --- |
| `list_files`, `read_file`, `search_files`, `write_file`, `create_folder` | the account's RAST files (`/` = home, `/shared/<name>` = shares it may open), through the same checks and quota as File Station | accounts that may open File Station |
| `list_server_files`, `read_server_file` | a folder or the first 128 KB of a text file on the server itself | administrators: as RAST's service account; others (unless turned off): read-only and guarded, see below |
| `run_command` | one command in a fresh `bash -l` without a terminal (default 60 s, at most 600 s; 64 KB of output per stream) | administrators: root on the server via `rast-terminal.service`; others (unless turned off): read-only on the server, see below; with server access off, accounts granted Terminal: their own sandbox |
| `web_search` | the search service an administrator connects (Brave, Tavily or SearXNG) | everyone, once connected |
| `fetch_url` | a public web page as text; addresses on the server or private networks are refused, redirects included | everyone |
| `browser` | a headless Chrome for the chat: `{action: open\|click\|type\|scroll\|back\|read, url, ref, text, submit, direction}`; each step answers with the page as text (what can be clicked or typed into marked `[n kind "name"]`, `ref` = n) and keeps a JPEG screenshot as a file of the chat (`image` on the tool block, served by `GET /api/ai/files/{id}`) | everyone, when Google Chrome is installed and `rast-terminal.service` runs |
| `artifact` | content shown in a panel beside the chat: `{id, title, kind: html\|svg\|markdown\|code, language?, content}` (≤ 400 KB; the same `id` again = a new version); never asks first, touches nothing | everyone |

**The browser** ([`src/ai/browser.rs`](../src/browser.rs)): Chrome runs
in a systemd unit of its own (`rast-browser-<id>`, started by the terminal
helper) as `nobody`, with Chrome's sandbox on, `ProtectSystem=strict`,
home folders and RAST's data, storage and settings hidden, a private `/tmp`
for its profile, 1 GiB of memory and at most 2 hours. Its only network is
`127.0.0.1` (`IPAddressDeny=any`), where RAST runs a proxy that connects to
public addresses only (the server, private networks and other local
services get `403`; ports 80, 443 and 1024 and up); QUIC is off and WebRTC
goes through the proxy only. RAST speaks DevTools with it over
`--remote-debugging-pipe`, which systemd connects to a socket in
`<data_dir>/browser` only RAST can open. One browser per chat, closed after
10 idle minutes, with its chat, or for another chat (at most 3 at a time).
Downloads are refused, dialogs are accepted, and pages that open a window
open it in the same tab.

**Server access for accounts that are not administrators**
([`src/ai/guard.rs`](../src/guard.rs)), on unless an administrator turns
it off (`PUT /api/ai/server-access`):

- Server files: only paths readable by any account (the file world-readable,
  every folder above it world-searchable; symlinks resolved first), never
  RAST's data, storage and config folders, `/etc/rast`, `/etc/cloudflared`,
  `/etc/netplan`, NetworkManager connections, `/etc/ssh`,
  `/etc/ssl/private`, WireGuard, Let's Encrypt, sudoers, `shadow`/`gshadow`,
  `/root`, `/home`, `/run/user`, Docker and containerd, `/var/backups`,
  cron spools, `/sys/firmware`, nor `/proc` beyond a few system summaries
  (`cpuinfo`, `meminfo`, `loadavg`, `uptime`, `version`, `stat`, `diskstats`,
  `partitions`, `swaps`, `filesystems`). Any path part named like a secret is
  refused: `.env*`, `*.pem`/`.key`/`.p12`/`.pfx`/`.jks`/`.kdbx`/`.gpg`/
  `.asc`/`.ovpn`, databases (`.db`, `.sqlite`), SSH/GnuPG/cloud credential
  folders, `id_rsa`-style keys, `.netrc`, `.pgpass`, `.git-credentials`,
  `.npmrc`, `htpasswd`, `rast.toml`, and names containing `password`,
  `passwd`, `secret`, `token`, `credential`, `private` or `apikey`. Binary
  files and files holding a private key are refused; listings leave private
  entries out.
- Commands: through the Terminal helper, as `systemd-run` with
  `User=nobody`, `ProtectSystem=strict`, `ProtectHome=yes`, a private `/tmp`,
  `/dev` and process namespace, `NoNewPrivileges`, no capabilities,
  `IPAddressDeny=any` (Unix and netlink sockets only), the places above as
  `InaccessiblePaths`, 1 GB of memory, 128 tasks and two CPUs;
  `timeout` stops the command at its limit (at most 580 s) and systemd
  removes whatever is left.
- Output of both passes through redaction: private key blocks, `$1$`…`$y$`
  password hashes, well-known token formats (`sk-`, `ghp_`, `github_pat_`,
  `glpat-`, `xox?-`, `AKIA`, `AIza`, …) and the values of settings whose
  name says secret (`password=`, `"api_key":`, `DB_PASSWORD=`) become
  `[hidden]`, with a count at the end.

An agent's `tools` list narrows this (`null` = all). Every call waits for the
chat's owner: the reply is stored with `status:"waiting"` and a block
`{type:"tool", id, name, input, status:"pending"}`; the owner answers
`allow` or `deny`, and once every call is answered the reply goes on (calls
run in order, then the model hears their results). The owner may let a tool
run without asking for the rest of a chat (`always:true`, kept in
`allowedTools`; `run_command` too, where it still runs as that account's
commands do). Sending a message or stopping instead
ends the request (its calls become `skipped`). Each call that runs is audited
(`ai.tool.<name>`). A reply makes at most 25 model calls per run.

| Method | Path | Notes |
| --- | --- | --- |
| GET | `/api/ai/options` | what the caller may chat with: `{isAdmin, configured, providers:[{id, kind, kindLabel, name, models}], agents:[{id, name, description, providerId, model}], allowance}` (agents without their personality; a signed-in plan is listed only to the account that signed in) |
| GET | `/api/ai/usage` | `{limits:{providers, dailyTokens, weeklyTokens, monthlyTokens, dailyCost, weeklyCost, monthlyCost, source}, allowance:{usedToday, usedWeek, usedMonth, costToday, costWeek, costMonth` (USD)`, dailyTokens, weeklyTokens, monthlyTokens, dailyCost, weeklyCost, monthlyCost, dayEndsAt, weekEndsAt, monthEndsAt, source}}` |
| GET | `/api/ai/conversations?q=&limit=` | the caller's own chats, newest first: `{items:[{id, userId, agentId, providerId, model, title, allowedTools, effort, createdAt, updatedAt, running, waiting}]}` (`waiting`: the last reply waits for a tool answer) |
| POST | `/api/ai/conversations` | `{agentId}` and/or `{providerId, model}` (an agent brings its own model; a model must be one the provider offers), plus `effort?` and `allowedTools?` to start with |
| GET | `/api/ai/conversations/{id}` | `{conversation, messages:[{id, role, content:[blocks], providerId, model, inputTokens, outputTokens, status:"streaming"\|"waiting"\|"done"\|"stopped"\|"error", error, createdAt, finishedAt}], seq, readOnly}`. Blocks: `{type:"text"\|"reasoning", text}` and `{type:"tool", id, name, input, status:"pending"\|"allowed"\|"running"\|"done"\|"error"\|"denied"\|"skipped", output?, error?}`, in reply order. A reply being written comes with what it has so far and `seq` (deltas already in it). Administrators may read anyone's chat (`readOnly:true`, `conversation.owner`; audited `ai.conversation.view`) |
| PATCH | `/api/ai/conversations/{id}` | `{title?, agentId?` (`""` = none)`, providerId?, model?, allowedTools?, effort?}` (`allowedTools: []` = ask before every tool again; `effort: ""` = the provider's default); owner only |
| DELETE | `/api/ai/conversations/{id}` | owner only; stops a running reply |
| POST | `/api/ai/conversations/{id}/messages` | `{text}` (≤ 100 KiB): stores it and starts the reply in the background; returns `{conversation, message, reply}` (`reply.status:"streaming"`). `409` while a reply is running, `429` (with `Retry-After`) once the allowance is used, `403` for a provider the account may not use |
| POST | `/api/ai/conversations/{id}/stop` | `{stopped}`; the reply keeps what was written (`status:"stopped"`); a reply waiting for tool answers ends too (its calls `skipped`) |
| POST | `/api/ai/conversations/{id}/retry` | `{retrying}`; a reply waiting to try a step again (see *Trying again* below) tries it now |
| GET | `/api/ai/view?url=` | an image, video or PDF on the web for the viewer beside the chat (see *The viewer* below); `400` with the reason for anything else |
| GET | `/api/ai/conversations/{id}/artifacts/{callId}` | an `html` or `svg` artifact (a finished `artifact` call) as a page, for whoever may read the chat; `Content-Security-Policy: sandbox allow-scripts …` without `allow-same-origin` (an opaque origin: no cookies, storage or RAST API), `default-src 'none'`, scripts and styles inline or from cdnjs, jsDelivr, unpkg and esm.sh, `frame-ancestors 'self'`; `X-Frame-Options: SAMEORIGIN`. Other kinds `404` (the app shows them) |
| POST | `/api/ai/conversations/{id}/tools/{callId}` | owner only: `{decision:"allow"\|"deny", always?}` → `{message, conversation}`; once no call is `pending` the reply goes on (checked like a new message: `429`, `403`, …). `409` when the chat is not waiting or the call was answered |
| GET | `/api/ai/admin/overview` | admin: `{kinds:[{id, label, defaultBaseUrl, needsKey, group, keyUrl, about, signsIn, signInFlow, site, callbackHint, risk}], providers, agents, skills, limits, users, groups, usage:[{userId, today, week, month, costToday, costWeek, costMonth, lastAt}], tools:[{name, label}], search, serverAccess}` (`signsIn`, `signInFlow:"browser"\|"device"\|""`, `site`, `callbackHint` and `risk` are set for the plan kinds: see above) |
| GET | `/api/ai/admin/usage/{userId}?days=30` | admin: one account's use: `{user, allowance, providers, days:[{date, input, output, tokens, cost, calls}]` (oldest first, every day of the last `days` (1–366), server time)`, models:[{providerId, provider, model, input, output, tokens, cost, calls, lastAt}]}` |
| PUT | `/api/ai/server-access` | admin: `{enabled}` — whether accounts that are not administrators may read the server and run read-only commands there; audited `ai.server_access.update` |
| GET / PUT | `/api/ai/search` | admin: the web search service `{kind:"brave"\|"tavily"\|"searxng"\|"", url, hasKey, keyHint, configured}`; PUT `{kind, url?` (SearXNG)`, apiKey?` (absent = keep)`}`; audited `ai.search.update` |
| POST | `/api/ai/search/test` | admin: one search with the saved service → `{results, first}` |
| GET | `/api/ai/admin/conversations?userId=&q=&limit=` | admin: every account's chats with `owner` and `messageCount` |
| POST | `/api/ai/providers` | admin: `{kind, name, baseUrl?` (empty = the kind's usual address)`, apiKey?, models:[…], prices?:{model: Price}` (set by an administrator; USD per million tokens, 0–10 000; models not offered are dropped)`, listPrices?:{model: Price}` (what the model list gave, all its models)`, enabled?}`; a plan kind is refused (`400`): it is added by signing in |
| PUT / DELETE | `/api/ai/providers/{id}` | admin: same body (kind cannot change; `apiKey` absent = keep, `removeKey:true` = remove; `prices` / `listPrices` absent = keep); a plan kind keeps its own address and takes no key (`400`) |
| POST | `/api/ai/providers/models` | admin: test a provider and list its models: `{providerId}` or `{kind, baseUrl?, apiKey?}` → `{models, prices}` (`prices`: those the list gives). A signed-in plan is asked with its tokens (its live catalog; `prices` is `{}`) |
| POST | `/api/ai/providers/sign-in` | admin: start signing in to a plan: `{kind, providerId?` (sign in again to that provider)`, name?` (a new provider's name)}`. A browser flow answers `{flow:"browser", url, state, redirectUri, label, site, callbackHint, risk}`: open `url`, allow it, and the browser lands on `redirectUri?code=…&state=…` (a page that does not open); paste that address back. A device flow answers `{flow:"device", state, userCode, verificationUri, interval, expiresIn, label, site, risk}`: enter `userCode` at `verificationUri` and the page polls `/api/ai/providers/sign-in/poll`. `risk` says why using a plan outside the provider's own app is unsupported (`""` none) and `callbackHint` what to paste (`""` for a device flow) |
| POST | `/api/ai/providers/sign-in/finish` | admin, the account that started it, within 20 minutes, once: `{callback` (the address landed on, or the `code#state` Claude's copy-code page shows)`, name?}` → `{provider, offered}`: trades the code for tokens and adds the provider (or signs it in again) owned by the caller, with the models the plan offers (`offered`, `null` when the catalog could not be read) |
| POST | `/api/ai/providers/sign-in/poll` | admin, the account that started a device sign-in: `{state}` → `{"status":"waiting"}` until the person finishes on the provider's page, then `{"status":"done", provider, offered}` (same as finishing). A code the provider refuses is a `400` |
| POST | `/api/ai/providers/{id}/sign-out` | admin: forget a plan's tokens and owner → `{provider}` with `signedIn:null` and `ownerId:null` (`400` for a provider that does not sign in); the provider and its models stay, but nobody can chat with it until someone signs in again; audited `ai.provider.sign_out` |
| POST | `/api/ai/providers/import/claude-code` | admin: use the Claude Code login already on the server instead of a browser round trip: `{providerId?` (sign in again to that provider)`, name?}` → `{provider, offered}`; `400` when the server has no Claude Code login (`~/.claude/.credentials.json`, or `$CLAUDE_CONFIG_DIR/.credentials.json`) or it cannot be read |
| PUT | `/api/ai/providers/{id}/prices` | admin: set one model's price there: `{model, price: Price \| null}` (`null`: the list's or the built-in price again) → the listing below |
| GET | `/api/ai/prices` | admin: `{providers:[{id, name, kind, kindLabel, enabled, local, checkedAt, models:[{model, price, source, match, fallback?}]}], builtIn:{models:[{name, input, output, cached, cacheWrite}], patterns:[{pattern, …}]}}`; `source` is `set`, `list`, `built-in` (`match`: the entry), `local`, `plan` (a signed-in plan: free here) or `none`; `fallback` (with `source: set`) is the price without it |
| POST | `/api/ai/prices/refresh` | admin: ask every enabled, non-local provider's model list for its prices now → the listing plus `results:[{id, name, priced \| error}]` |
| POST | `/api/ai/agents` | admin: `{name, description?, systemPrompt?, providerId?, model?, skills:[ids], tools?:[names] \| null, temperature?` (0–2)`, maxOutputTokens?, enabled?}` |
| PUT / DELETE | `/api/ai/agents/{id}` | admin |
| POST | `/api/ai/skills` | admin: `{name, description?, instructions}` → `{id}` |
| PUT / DELETE | `/api/ai/skills/{id}` | admin (agents stop carrying a deleted skill) |
| PUT / DELETE | `/api/ai/limits/{subject}` | admin: `subject` is `default`, `group:<id>` or `user:<id>`; body `{providers: [ids] \| null, dailyTokens, weeklyTokens, monthlyTokens: n \| null, dailyCost, weeklyCost, monthlyCost: USD \| null}` (`null` or absent = all / no limit) |

Replies are written on the server, one per chat at a time, and stream to the
owner's pages as `ai.delta` events `{conversationId, messageId, seq, text,
reasoning}` (text added since the previous delta; `seq` counts up from 1 in
each run, so a gap means the page should load the chat again). A delta goes
to the reply's current step, after its last tool block: reasoning into the
step's reasoning block (made at the step's start), text into the text block
that ends it (or a new one). `ai.message` `{conversationId, message, running,
seq?}` carries each stored message (the owner and administrators), also
whenever a running reply's tool calls change (`seq`: deltas already in it),
`ai.conversation` `{conversation, deleted?}` chat changes, and `ai.config`
`{what}` setup changes. A reply cut off by a restart is marked `error`
("Interrupted") when RAST starts; a reply waiting for an answer keeps waiting.
Configuration changes are audited (`ai.provider.*`, `ai.agent.*`,
`ai.skill.*`, `ai.limit.*`, `ai.search.update`).

### Trying again

A step that fails in passing (a timeout, the network, HTTP 408/500/502/503/
504/52x/529, or an error the provider streams such as `overloaded_error`,
`server_error` or `UNAVAILABLE`) is tried again 30 seconds later, up to 10
times in a row; a step that works starts the count again. What the failed
step had written is taken back first. While it waits, the reply's message
(in `ai.message` and in `GET /api/ai/conversations/{id}`) carries
`retry: {attempt, of, inMs, error}`: this try (1–10), of how many, the time
left in milliseconds when it was sent, and what failed. Stop ends the wait
(`status:"stopped"`); `POST …/retry` starts the try now. After the tenth the
reply ends `error`, saying it tried again 10 times.

### The panel

What a chat names opens in a tab of the panel beside it, without leaving
the conversation: artifacts, links in an answer (`[…](https://…)`, `![…](…)`,
bare addresses), paths in the person's files (`[report](/shared/team/q3.pdf)`
or `` `/notes/plan.md` ``), a finished `read_file`/`write_file` call
(**View**, and a card under the reply for each file it wrote), files sent in
the chat, and a path or address typed after **+**. The same thing opens in
the same tab. Each tab keeps its view while open; a chat keeps its tabs (with
the window, and while another chat is in front). The path bar shows where it
is and what it is, Preview/Source where both exist, Reload, Copy, Download,
Open in a new tab and Show in File Station; **Fill** covers the window,
**Hide** gives the chat its room (the chat's header brings the tabs back).

By extension: pictures (png, jpg/jpeg, gif, webp, avif, bmp, svg, ico),
videos (mp4, m4v, webm, ogv, mov), PDFs, documents (doc/docx/dotx/docm, odt,
rtf; xls/xlsx/xlsm, ods; ppt/pptx/ppsx/pptm, odp), Markdown (rendered), CSV
and TSV (a table, first 2,000 rows), JSON (formatted), HTML (a page), text
and code (with line numbers, first 4 MB). Other files offer Download and
File Station. An address with none of these is a web page.

- A path opens with the person's own access through RAST: pictures and PDFs
  from `GET /api/files/preview`, text from `GET /api/files/text`, videos and
  documents from `GET /api/files/download`. A path that is not there or
  theirs says so.
- An HTML file shows through `GET /api/ai/view/html?path=` (up to 2 MiB,
  with their access) under the artifacts' policy: `sandbox` without
  `allow-same-origin`, so its scripts reach neither the session nor RAST's
  API; a few public CDNs only.
- A chat file comes from `GET /api/ai/files/{id}`; PDFs are served `inline`
  with a `sandbox` CSP.
- A web address goes through `GET /api/ai/view?url=&as=` (signed in), so the
  page keeps its strict CSP and the site sees the server, not the person:
  http(s) to public addresses only (each redirect checked again, at most 6).
  `as=media` (default): image, video or PDF types, one `bytes=` range passed
  on (`206` and `Content-Range` back), at most 512 MB, streamed, with the
  site's type, `inline`, `nosniff`, `Referrer-Policy: no-referrer`,
  `Cache-Control: private, max-age=300` and a `sandbox` CSP. `as=text`: text,
  JSON, XML, YAML and code types, up to 4 MB, as `text/plain; charset=utf-8`.
  `as=document`: office, OpenDocument and RTF types (or `octet-stream`/zip),
  up to 64 MB, as an `application/octet-stream` attachment.
- A web page: `GET /api/ai/view/page?url=` reads its head (public addresses,
  first 512 KB) → `{url, title, description, siteName, image, icon,
  contentType, frameable, refused}`. `frameable` is false when the site
  sends `X-Frame-Options` (other than `ALLOWALL`) or a CSP `frame-ancestors`
  without `*`/`https:`; `refused` says which. A frameable HTTPS page opens in
  a sandboxed frame of its own origin (`allow-scripts allow-same-origin
  allow-forms allow-popups allow-popups-to-escape-sandbox allow-downloads
  allow-modals`, no top navigation), which needs RAST 0.9.19's page policy
  (`frame-src 'self' https:`); else a card (picture, title, description,
  Open in a new tab). An address that turns out to be a picture, video, PDF,
  text or document opens as one.

#### Documents as pages

`POST /api/ai/view/convert?name=` (raw body, up to 64 MiB, CSRF) turns a
Word, Excel, PowerPoint, OpenDocument or RTF document into a PDF → `{id}`;
`GET /api/ai/view/converted/{id}` serves it (`inline`, `sandbox` CSP,
`nosniff`) to the account that converted it. The window fetches the bytes
(with the person's access, or through `/api/ai/view?as=document`) and posts
them.

- The extension picks the kind and the contents must match it (a ZIP for
  Office Open XML/OpenDocument, an OLE file for older Office, `{\rtf`).
- LibreOffice on the server converts it, in a systemd unit of its own
  started by the root terminal helper (Terminal must be on): as `nobody`,
  `PrivateNetwork`, `RestrictAddressFamilies=AF_UNIX`, home folders and
  RAST's private paths hidden, a private `/tmp`, `ProtectSystem=strict`, no
  capabilities, 1 GiB, 2 minutes, two at a time. The document goes in on the
  unit's standard input and the PDF comes out on its standard output, so the
  unit opens no file of the server.
- Text documents need `libreoffice-writer-nogui` (or Writer), spreadsheets
  `libreoffice-calc-nogui`, slides `libreoffice-impress-nogui`; a missing
  part says which package adds it.
- A PDF is kept per account under the app's data (`converted/<account>/
  <sha256>.pdf`) for a week from its last use, so the same document opens at
  once.
