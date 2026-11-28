import assert from "node:assert/strict";
import test from "node:test";

import { agentHues } from "../src/apps/agent/marks";
import { parseInline, parseMarkdown, safeHref } from "../src/apps/agent/markdown";
import { compareVersions, whatsNewSince, WHATS_NEW } from "../src/apps/agent/whats-new";
import {
  badgeOf,
  extensionOf,
  folderOf,
  kindLabel,
  looksLikePath,
  parseDelimited,
  viewKey,
  viewKindOf,
  viewOfChatFile,
  viewOfLink,
  viewOfPath,
  viewOfUrl,
} from "../src/apps/agent/view";
import {
  chatStats,
  contextWindow,
  shownAccount,
  countedFigures,
  timeLeft,
  usageBars,
  type ChatQuotaAccount,
  type ContentBlock,
  type Message,
} from "../src/apps/agent/model";
import {
  accountState,
  countedBars,
  leftTone,
  parseTokens,
  retryLabel,
  type QuotaAccount,
  resetIn,
  windowLeft,
  type ProviderAccount,
  addToStep,
  groupKinds,
  allowanceText,
  allowanceUsed,
  formatUsd,
  formatPrice,
  priceRules,
  planLabel,
  formatElapsed,
  replyActivity,
  applyDelta,
  groupByDay,
  maySkipAsking,
  messageText,
  toolArgument,
  useAppCall,
  wholeMessage,
  chatArtifacts,
  artifactFileName,
  artifactSlug,
  upsertMessage,
  type ContentBlock,
  type ContentBlock,
  type Message,
  EFFORTS,
  effortLabel,
  PERMISSION_MODES,
  permissionMode,
} from "../src/apps/agent/model";

test("markdown blocks", () => {
  const blocks = parseMarkdown(
    [
      "# Title",
      "",
      "Some *text*",
      "on two lines.",
      "",
      "- one",
      "- two",
      "  - nested",
      "",
      "1. first",
      "2. second",
      "",
      "> quoted",
      "",
      "```ts",
      "const x = 1;",
      "```",
      "",
      "| a | b |",
      "|---|:-:|",
      "| 1 | 2 |",
      "",
      "---",
    ].join("\n"),
  );
  assert.deepEqual(
    blocks.map((b) => b.t),
    ["h", "p", "list", "list", "quote", "code", "table", "hr"],
  );
  const list = blocks[2];
  assert.ok(list?.t === "list" && !list.ordered && list.items.length === 2);
  assert.equal(list.items[1]?.children[0]?.t, "list");
  const ordered = blocks[3];
  assert.ok(ordered?.t === "list" && ordered.ordered && ordered.items.length === 2);
  const code = blocks[5];
  assert.deepEqual(code, { t: "code", lang: "ts", v: "const x = 1;", open: false });
  const table = blocks[6];
  assert.ok(table?.t === "table");
  assert.deepEqual(table.head, ["a", "b"]);
  assert.deepEqual(table.align, ["", "center"]);
  assert.deepEqual(table.rows, [["1", "2"]]);
});

test("an unclosed fence is open (a reply being written)", () => {
  const blocks = parseMarkdown("Here:\n```py\nprint(1)");
  assert.deepEqual(blocks[1], { t: "code", lang: "py", v: "print(1)", open: true });
});

test("markdown inline", () => {
  assert.deepEqual(parseInline("a **b** *c* `d` ~~e~~"), [
    { t: "text", v: "a " },
    { t: "strong", c: [{ t: "text", v: "b" }] },
    { t: "text", v: " " },
    { t: "em", c: [{ t: "text", v: "c" }] },
    { t: "text", v: " " },
    { t: "code", v: "d" },
    { t: "text", v: " " },
    { t: "del", c: [{ t: "text", v: "e" }] },
  ]);
  assert.deepEqual(parseInline("[docs](https://example.com/a)"), [
    { t: "link", href: "https://example.com/a", c: [{ t: "text", v: "docs" }] },
  ]);
  // Unsafe links keep their text only.
  assert.deepEqual(parseInline("[x](javascript:alert(1))"), [{ t: "text", v: "x" }]);
  assert.deepEqual(parseInline("see https://a.io/x."), [
    { t: "text", v: "see " },
    { t: "link", href: "https://a.io/x", c: [{ t: "text", v: "https://a.io/x" }] },
    { t: "text", v: "." },
  ]);
  assert.deepEqual(parseInline("snake_case_name and 2 * 3 * 4"), [{ t: "text", v: "snake_case_name and 2 * 3 * 4" }]);
  assert.deepEqual(parseInline("<b>hi</b>"), [{ t: "text", v: "<b>hi</b>" }]);
  assert.equal(safeHref("mailto:a@b.c"), "mailto:a@b.c");
  assert.equal(safeHref("data:text/html,x"), null);
});

function message(id: number, status: Message["status"], text = ""): Message {
  return {
    id,
    conversationId: "c",
    role: "assistant",
    content: [{ type: "text", text }],
    providerId: null,
    model: "m",
    inputTokens: 0,
    outputTokens: 0,
    status,
    error: "",
    createdAt: 0,
    finishedAt: null,
  };
}

test("deltas apply in order and report gaps", () => {
  const detail = { messages: [message(5, "streaming", "He")], seq: 1 };
  assert.equal(applyDelta(detail, { conversationId: "c", messageId: 5, seq: 1, text: "He", reasoning: "" }), "ignored");
  assert.equal(applyDelta(detail, { conversationId: "c", messageId: 5, seq: 2, text: "llo", reasoning: "hmm" }), "applied");
  assert.equal(messageText(detail.messages[0]!), "Hello");
  assert.equal(detail.messages[0]?.content[0]?.type, "reasoning");
  assert.equal(applyDelta(detail, { conversationId: "c", messageId: 5, seq: 4, text: "!", reasoning: "" }), "stale");
  assert.equal(applyDelta(detail, { conversationId: "c", messageId: 9, seq: 3, text: "!", reasoning: "" }), "stale");
  detail.messages[0]!.status = "done";
  assert.equal(applyDelta(detail, { conversationId: "c", messageId: 5, seq: 3, text: "!", reasoning: "" }), "ignored");
});

test("deltas fill the current step, as on the server", () => {
  // Same cases as src/ai/mod.rs deltas_fill_the_current_step.
  const blocks: ContentBlock[] = [];
  addToStep(blocks, "", "think");
  addToStep(blocks, "Hel", "");
  addToStep(blocks, "lo", " more");
  assert.deepEqual(blocks, [
    { type: "reasoning", text: "think more" },
    { type: "text", text: "Hello" },
  ]);
  blocks.push({ type: "tool", id: "c1", name: "list_files", input: { path: "/" }, status: "pending" });
  addToStep(blocks, "Found", "");
  addToStep(blocks, "", "again");
  assert.deepEqual(blocks.slice(2), [
    { type: "tool", id: "c1", name: "list_files", input: { path: "/" }, status: "pending" },
    { type: "reasoning", text: "again" },
    { type: "text", text: "Found" },
  ]);
  assert.equal(messageText({ content: blocks }), "HelloFound");
});

test("tool cards show their main argument", () => {
  assert.equal(toolArgument({ name: "run_command", input: { command: "ls -la" } }), "ls -la");
  assert.equal(toolArgument({ name: "fetch_url", input: { url: "https://x.io" } }), "https://x.io");
  assert.equal(toolArgument({ name: "browser", input: { action: "open", url: "https://x.io" } }), "open https://x.io");
  assert.equal(toolArgument({ name: "browser", input: { action: "click", ref: 12 } }), "click [12]");
  assert.equal(toolArgument({ name: "browser", input: { action: "type", ref: "3", text: "cats", submit: true } }), 'type "cats" into [3] + Enter');
  assert.equal(toolArgument({ name: "browser", input: { action: "scroll" } }), "scroll down");
  assert.equal(toolArgument({ name: "read_file", input: "not json" }), "not json");
  assert.equal(toolArgument({ name: "unknown", input: { a: 1 } }), '{"a":1}');
  assert.equal(maySkipAsking("write_file"), true);
  assert.equal(maySkipAsking("run_command"), true);
  assert.equal(maySkipAsking("format_disk"), false);
});

test("work in an app shows the app and the command", () => {
  const names = (id: string) => (id === "wordcraft" ? "WordCraft" : id);
  const insert = { name: "use_app", input: { app: "wordcraft", method: "engine.execute", params: { command: "text.insert", params: { text: "Hi" } } } };
  assert.equal(toolArgument(insert, names), "WordCraft · text.insert");
  assert.equal(toolArgument({ name: "use_app", input: { app: "wordcraft", method: "document.inspect" } }, names), "WordCraft · document.inspect");
  assert.deepEqual(useAppCall(insert), { app: "wordcraft", steps: [{ method: "engine.execute", params: { command: "text.insert", params: { text: "Hi" } } }], batch: false });
  assert.deepEqual(useAppCall({ name: "use_app", input: { app: "wordcraft", method: "ui.inspect", params: [1] } }), { app: "wordcraft", steps: [{ method: "ui.inspect", params: {} }], batch: false });
  assert.equal(useAppCall({ name: "use_app", input: { app: "wordcraft" } }), null);
  assert.equal(useAppCall({ name: "open_app", input: { app: "wordcraft", method: "x.y" } }), null);
  assert.equal(maySkipAsking("use_app"), true);

  // A series of methods in one call.
  const series = {
    name: "use_app",
    input: { app: "wordcraft", calls: [{ method: "engine.execute", params: { command: "shape.fill" } }, { method: "engine.execute", params: { command: "shape.outline" } }, { method: "ui.screenshot" }] },
  };
  assert.equal(toolArgument(series, names), "WordCraft · shape.fill and 2 more");
  assert.deepEqual(useAppCall(series)?.steps.map((step) => step.method), ["engine.execute", "engine.execute", "ui.screenshot"]);
  assert.equal(useAppCall(series)?.batch, true);
  assert.equal(useAppCall({ name: "use_app", input: { app: "wordcraft", calls: [] } }), null);
  assert.equal(useAppCall({ name: "use_app", input: { app: "wordcraft", calls: [{ method: "ui.inspect" }, { params: {} }] } }), null);
});

test("a long reply's live event puts its newest blocks after the page's", () => {
  const block = (n: number): ContentBlock => ({ type: "tool", id: `c${n}`, name: "use_app", input: {}, status: "done", output: `${n}` });
  const known = { ...message(5, "streaming"), content: [block(0), block(1), block(2)] };
  const event = (content: ContentBlock[], from?: number, length?: number) => ({
    conversationId: "c",
    running: true,
    message: { ...message(5, "streaming"), content, ...(from === undefined ? {} : { contentFrom: from, contentLength: length }) },
  });
  const whole = event([block(9)]);
  assert.equal(wholeMessage(known, whole), whole.message);
  const merged = wholeMessage(known, event([{ ...block(2), output: "two" }, block(3)], 2, 4));
  assert.deepEqual(merged?.content.map((b) => (b as { output?: string }).output), ["0", "1", "two", "3"]);
  assert.equal(merged?.status, "streaming");
  assert.equal("contentFrom" in (merged ?? {}), false);
  assert.equal(wholeMessage(known, event([block(5)], 5, 6)), null);
  assert.equal(wholeMessage(undefined, event([block(3)], 3, 4)), null);
  assert.equal(wholeMessage(known, event([block(3)], 3, 9)), null);
});

test("messages stay in id order", () => {
  const list = [message(1, "done"), message(3, "done")];
  upsertMessage(list, message(2, "done"));
  upsertMessage(list, message(3, "stopped"));
  assert.deepEqual(list.map((m) => [m.id, m.status]), [[1, "done"], [2, "done"], [3, "stopped"]]);
});

test("allowance summary", () => {
  const base = { usedToday: 1500, usedMonth: 40_000, dayEndsAt: 0, monthEndsAt: 0, source: "user" };
  assert.equal(allowanceText({ ...base, dailyTokens: null, monthlyTokens: null }), null);
  assert.equal(allowanceUsed({ ...base, dailyTokens: null, monthlyTokens: null }), null);
  assert.equal(allowanceText({ ...base, dailyTokens: 10_000, monthlyTokens: null }), "1.5k of 10k tokens today");
  assert.equal(allowanceUsed({ ...base, dailyTokens: 10_000, monthlyTokens: 50_000 }), 0.8);
  assert.equal(allowanceUsed({ ...base, dailyTokens: 0, monthlyTokens: null }), 1);
  // The tightest limit is the one shown, money included.
  const week = { ...base, dailyTokens: 10_000, monthlyTokens: null, usedWeek: 9000, weeklyTokens: 10_000, costMonth: 4.75, monthlyCost: 5 };
  assert.equal(allowanceText(week), "$4.75 of $5.00 spent this month");
  assert.equal(allowanceUsed(week), 0.95);
  assert.equal(allowanceText({ ...week, monthlyCost: null }), "9.0k of 10k tokens this week");
  assert.equal(formatUsd(0), "$0");
  assert.equal(formatUsd(0.0042), "$0.0042");
  assert.equal(formatUsd(1234.5), "$1,235");
  assert.equal(formatPrice(3), "$3");
  assert.equal(formatPrice(0.3), "$0.30");
  assert.equal(formatPrice(0.625), "$0.625");
  assert.equal(formatPrice(0.0028), "$0.0028");
  assert.equal(formatPrice(0), "$0");
  assert.equal(formatPrice(12.5), "$12.50");
  assert.deepEqual(
    priceRules({
      input: 2,
      output: 6,
      multipliers: [
        { factor: 2, days: ["mon", "tue", "wed", "thu", "fri"], hours: [["01:00", "04:00"], ["06:00", "10:00"]] },
        { factor: 2, from: "2026-12-01" },
      ],
      tiers: [{ above: 200_000, input: 4, output: 12 }],
    }),
    ["×2 Mon–Fri 01:00–04:00, 06:00–10:00 UTC", "×2 from Dec 1, 2026", "Over 200k tokens: $4 in, $12 out"],
  );
});

test("chats group by day, week, month and year", () => {
  const now = new Date(2026, 10, 26, 15).getTime();
  const at = (days: number) => ({ id: String(days), userId: 1, agentId: null, providerId: null, model: "", title: "", createdAt: 0, updatedAt: now - days * 86_400_000, running: false });
  const groups = groupByDay([at(800), at(3), at(0), at(40), at(1), at(10), at(70), at(400), at(45)] as never, now);
  assert.deepEqual(
    groups.map((g) => [g.label, g.items.map((c) => c.id)]),
    [
      ["Today", ["0"]],
      ["Yesterday", ["1"]],
      ["Previous 7 days", ["3"]],
      ["Previous 30 days", ["10"]],
      ["October", ["40", "45"]],
      ["September", ["70"]],
      ["2025", ["400"]],
      ["2024", ["800"]],
    ],
  );
});

test("pinned chats come first, whatever their age", () => {
  const now = new Date(2026, 10, 26, 15).getTime();
  const at = (id: string, days: number, pinnedAt: number | null = null) =>
    ({ id, userId: 1, agentId: null, providerId: null, model: "", title: "", createdAt: 0, updatedAt: now - days * 86_400_000, pinnedAt, running: false }) as never;
  const groups = groupByDay([at("a", 0), at("old", 400, now), at("b", 2), at("week", 3, now - 5)], now);
  assert.deepEqual(
    groups.map((g: { label: string; items: Array<{ id: string }> }) => [g.label, g.items.map((c) => c.id)]),
    [
      ["Pinned", ["week", "old"]],
      ["Today", ["a"]],
      ["Previous 7 days", ["b"]],
    ],
  );
});

test("the provider picker groups kinds and finds them by name or what they offer", () => {
  const kind = (id: string, group: string, label: string, about = "") => ({
    id,
    label,
    group,
    about,
    defaultBaseUrl: "",
    needsKey: true,
    keyUrl: "",
  });
  const kinds = [
    kind("anthropic", "maker", "Anthropic", "Claude models"),
    kind("opencode", "gateway", "OpenCode Zen", "Claude, GPT and more"),
    kind("ollama", "local", "Ollama"),
    kind("codex", "plan", "ChatGPT Codex", "Sign in with your ChatGPT plan"),
    kind("custom", "custom", "OpenAI-compatible"),
  ];
  assert.deepEqual(
    groupKinds(kinds, "").map((g) => [g.id, g.kinds.map((k) => k.id)]),
    [
      ["maker", ["anthropic"]],
      ["plan", ["codex"]],
      ["gateway", ["opencode"]],
      ["local", ["ollama"]],
      ["custom", ["custom"]],
    ],
  );
  assert.deepEqual(
    groupKinds(kinds, "claude").map((g) => g.kinds.map((k) => k.id)),
    [["anthropic"], ["opencode"]],
  );
  assert.deepEqual(groupKinds(kinds, "nothing like it"), []);
  assert.deepEqual(
    groupKinds(kinds, "chatgpt").map((g) => g.id),
    ["plan"],
  );
  assert.equal(planLabel("plus"), "Plus");
  assert.equal(planLabel("PRO"), "Pro");
  assert.equal(planLabel("k12"), "K12");
  assert.equal(planLabel(""), "");
});

test("an agent keeps its colours", () => {
  assert.deepEqual(agentHues("Writer"), agentHues(" writer "));
  assert.notDeepEqual(agentHues("Writer"), agentHues("Captain"));
  for (const hue of agentHues("Captain")) assert.ok(hue >= 0 && hue < 360);
});

test("a chat's allowed tools match a permission preset in any order", () => {
  assert.equal(permissionMode([])?.id, "ask");
  const reads = ["read_server_file", "search_files", "list_files", "read_file", "list_server_files"];
  assert.equal(permissionMode(reads)?.id, "read");
  assert.equal(permissionMode([...reads, "write_file", "create_folder"])?.id, "edit");
  // An older chat that allowed only the RAST file reads.
  assert.equal(permissionMode(["search_files", "list_files", "read_file"]), null);
  assert.equal(permissionMode(PERMISSION_MODES[3]!.tools)?.id, "act");
  // Allowed one by one ("Allow for this chat" on a single tool).
  assert.equal(permissionMode(["read_file"]), null);
  // Only "Allow everything" runs commands without asking.
  assert.deepEqual(PERMISSION_MODES.filter((m) => m.tools.includes("run_command")).map((m) => m.id), ["all"]);
  assert.equal(permissionMode(PERMISSION_MODES[4]!.tools)?.id, "all");
});

test("effort levels are labelled, Auto for none", () => {
  assert.equal(effortLabel(""), "Auto");
  assert.equal(effortLabel(undefined), "Auto");
  assert.equal(effortLabel("max"), "Max");
  assert.deepEqual(
    EFFORTS.map((e) => e.id),
    ["", "low", "medium", "high", "max"],
  );
});

test("a reply being written says what it does and for how long", () => {
  assert.equal(formatElapsed(-5), "0s");
  assert.equal(formatElapsed(4_900), "4s");
  assert.equal(formatElapsed(65_000), "1m 05s");
  assert.equal(formatElapsed(3_725_000), "1h 02m");
  const reply = (status: Message["status"], content: ContentBlock[]) => replyActivity({ status, content });
  assert.equal(reply("streaming", []), "Thinking");
  assert.equal(reply("streaming", [{ type: "text", text: "" }]), "Thinking");
  assert.equal(reply("streaming", [{ type: "text", text: "Hel" }]), "Writing");
  // Thinking that streams shows itself.
  assert.equal(reply("streaming", [{ type: "reasoning", text: "Plan" }]), null);
  const tool = (status: string) => ({ type: "tool", id: "c1", name: "list_files", input: {}, status }) as unknown as ContentBlock;
  assert.equal(reply("streaming", [tool("running")]), null);
  assert.equal(reply("streaming", [tool("done")]), "Reading the results");
  assert.equal(reply("done", []), null);
  assert.equal(reply("waiting", [tool("pending")]), null);
});

test("artifacts: versions per id, file names, slugs like the server", () => {
  const tool = (id: string, input: Record<string, unknown>, status = "done") => ({ type: "tool", id, name: "artifact", input, status }) as never;
  const messages = [
    { id: 1, content: [tool("c1", { id: "calc", title: "Calculator", kind: "html", content: "<p>1</p>" })] },
    { id: 2, content: [{ type: "text", text: "Here" }, tool("c2", { id: "notes", title: "Notes", kind: "markdown", content: "# N" })] },
    { id: 3, content: [tool("c3", { id: "calc", title: "Calculator", kind: "html", content: "<p>2</p>" }), tool("c4", { kind: "html", content: "x" }, "pending")] },
    { id: 4, content: [tool("c5", { title: "Sales report 2026!", kind: "code", language: "Python", content: "print(1)" })] },
  ] as never;
  const list = chatArtifacts(messages);
  assert.deepEqual(
    list.map((a) => [a.callId, a.key, a.version]),
    [
      ["c1", "calc", 1],
      ["c2", "notes", 1],
      ["c3", "calc", 2],
      ["c5", "sales-report-2026", 1],
    ],
  );
  assert.equal(artifactFileName(list[0]!), "calculator.html");
  assert.equal(artifactFileName(list[1]!), "notes.md");
  assert.equal(artifactFileName(list[3]!), "sales-report-2026.py");
  assert.equal(artifactFileName({ title: "x", kind: "code", language: "cobol" }), "x.txt");
  assert.equal(artifactSlug("Budget Calc"), "budget-calc");
  assert.equal(artifactSlug("--"), "artifact");
});

test("reset times read in hours and minutes", () => {
  assert.equal(resetIn(40_000), "in 40 s");
  assert.equal(resetIn(600_000), "in 10 min");
  assert.equal(resetIn(11_520_000), "in 3 h 12 min");
  assert.equal(resetIn(7_200_000), "in 2 h");
  assert.equal(resetIn(2 * 86_400_000 + 3_600_000), "in 2 d 1 h");
});

test("an account's state says whether it answers", () => {
  const account: ProviderAccount = {
    id: "a",
    label: "me@example.com",
    name: "Antigravity",
    first: true,
    signedIn: { email: "me@example.com", plan: "", expiresAt: 0 },
    hasKey: false,
    keyHint: "",
    ready: true,
    paused: false,
    resting: null,
    quota: {},
    position: 0,
  };
  assert.deepEqual(accountState(account, 1000, true), { label: "Ready", tone: "ok" });
  assert.deepEqual(accountState({ ...account, resting: { until: 1000 + 600_000, reason: "Usage limit reached" } }, 1000, true), {
    label: "Resting · back in 10 min",
    tone: "warn",
  });
  // A rest that is over: ready again.
  assert.equal(accountState({ ...account, resting: { until: 500, reason: "" } }, 1000, true).label, "Ready");
  assert.equal(accountState({ ...account, ready: false, signedIn: null }, 1000, true).label, "Sign in needed");
  assert.equal(accountState({ ...account, ready: false }, 1000, false).label, "Key needed");
  assert.deepEqual(accountState({ ...account, paused: true }, 1000, true), { label: "Set aside", tone: "off" });
});

test("accounts without reported limits get bars from what RAST counted", () => {
  const counted = (h5: number, week: number, leavesAt?: number) => ({
    hours5: { calls: 1, tokens: h5, ...(leavesAt ? { leavesAt } : {}) },
    day: { calls: 1, tokens: h5 },
    week: { calls: 1, tokens: week },
  });
  const base = { id: "a", label: "A", position: 0, paused: false, quota: {}, models: [], canCheck: false } as unknown as QuotaAccount;
  const set = { ...base, counted: counted(250_000, 900_000, 5000), limits: { hours5: 1_000_000, seenWeek: 3_000_000 } } as QuotaAccount;
  const other = { ...base, id: "b", counted: counted(500_000, 600_000) } as QuotaAccount;
  const [h5, week] = countedBars(set, [set, other]);
  assert.deepEqual([h5.source, h5.limit, h5.left, h5.fill, h5.leavesAt], ["set", 1_000_000, 75, 75, 5000]);
  assert.deepEqual([week.source, week.limit, week.left], ["seen", 3_000_000, 70]);
  // No limit: beside the busiest account, with nothing "left".
  const [free] = countedBars(other, [set, other]);
  assert.deepEqual([free.source, free.limit, free.left, free.fill], ["none", null, null, 100]);
  assert.equal(countedBars({ ...other, counted: counted(0, 0) } as QuotaAccount, [other])[0].fill, 0);
  // Over the limit: nothing left.
  assert.equal(countedBars({ ...set, counted: counted(2_000_000, 0) } as QuotaAccount, [])[0].left, 0);
  assert.equal(parseTokens("2M"), 2_000_000);
  assert.equal(parseTokens(" 1.5m "), 1_500_000);
  assert.equal(parseTokens("500k"), 500_000);
  assert.equal(parseTokens("1,000,000"), 1_000_000);
  assert.equal(parseTokens(""), null);
  assert.ok(Number.isNaN(parseTokens("lots")));
  assert.ok(Number.isNaN(parseTokens("0")));
});

test("what is new shows once, for the versions not seen yet", () => {
  assert.equal(compareVersions("1.10.0", "1.9.2"), 1);
  assert.equal(compareVersions("1.2", "1.2.0"), 0);
  const releases = [
    { version: "1.4.0", date: "", items: ["d"] },
    { version: "1.3.0", date: "", items: ["c"] },
    { version: "1.2.0", date: "", items: ["b"] },
    { version: "1.1.0", date: "", items: ["a"] },
  ];
  const shown = (seen: string | null, current: string) => whatsNewSince(seen, current, releases).map((r) => r.version);
  assert.deepEqual(shown("1.2.0", "1.3.0"), ["1.3.0"]);
  assert.deepEqual(shown("1.1.0", "1.4.0"), ["1.4.0", "1.3.0", "1.2.0"]);
  assert.deepEqual(shown(null, "1.3.0"), ["1.3.0"]);
  assert.deepEqual(shown("1.3.0", "1.3.0"), []);
  assert.deepEqual(shown("1.4.0", "1.3.0"), []);
  // Every release has something to say, newest first.
  assert.ok(WHATS_NEW.every((r) => r.items.length > 0));
  assert.ok(WHATS_NEW.every((r, i) => i === 0 || compareVersions(WHATS_NEW[i - 1].version, r.version) > 0));
});

test("quota bars show what is left, coloured by how much", () => {
  assert.equal(windowLeft({ name: "5 hours", usedPercent: 42 }), 58);
  assert.equal(windowLeft({ name: "Requests", used: 360, limit: 1000 }), 64);
  assert.equal(windowLeft({ name: "Weekly", usedPercent: 130 }), 0);
  assert.equal(windowLeft({ name: "Tokens" }), null);
  assert.equal(leftTone(58), "ok");
  assert.equal(leftTone(30), "mid");
  assert.equal(leftTone(9), "low");
  assert.equal(leftTone(null), "");
});

test("a reply waiting to try again counts down in whole seconds", () => {
  const retry = { attempt: 1, of: 10 };
  assert.equal(retryLabel(retry, 30_000), "Retrying 1/10 in 30s");
  assert.equal(retryLabel(retry, 29_001), "Retrying 1/10 in 30s");
  assert.equal(retryLabel({ attempt: 7, of: 10 }, 400), "Retrying 7/10 in 1s");
  assert.equal(retryLabel(retry, -250), "Retrying 1/10 in 0s");
});

test("the panel knows files by their names", () => {
  assert.equal(viewKindOf("/home/report.PDF"), "pdf");
  assert.equal(viewKindOf("https://a.io/x/cat.jpeg?size=2#top"), "image");
  assert.equal(viewKindOf("clip.webm"), "video");
  assert.equal(viewKindOf("notes.txt"), "text");
  assert.equal(viewKindOf("/shared/Q3 plan.docx"), "document");
  assert.equal(viewKindOf("budget.XLSX"), "document");
  assert.equal(viewKindOf("src/main.rs"), "text");
  assert.equal(viewKindOf("Dockerfile"), "text");
  assert.equal(viewKindOf("archive.zip"), null);
  assert.equal(extensionOf("/a/b.tar.gz?x=1"), "gz");
  assert.equal(viewKindOf("/home/.png"), null);
  assert.equal(viewKindOf("https://a.io/download?file=a.png"), null);
  assert.equal(viewKindOf("https://a.io/"), null);
});

test("links open through the server, files with the person's own access", () => {
  const web = viewOfUrl("https://example.com/media/My%20Chart.png");
  assert.equal(web?.kind, "image");
  assert.equal(web?.title, "My Chart.png");
  assert.equal(web?.src, "/api/ai/view?url=https%3A%2F%2Fexample.com%2Fmedia%2FMy%2520Chart.png");
  assert.equal(web?.external, "https://example.com/media/My%20Chart.png");
  assert.equal(viewOfUrl("javascript:alert(1)//x.png"), null);
  const page = viewOfUrl("https://example.com/page.html");
  assert.equal(page?.kind, "website", "a page opens as one, HTML or not");
  assert.equal(page?.src, "https://example.com/page.html");
  assert.equal(viewOfUrl("https://example.com/")?.kind, "website");
  const notes = viewOfUrl("https://example.com/notes.md");
  assert.equal(notes?.kind, "text");
  assert.equal(notes?.format, "markdown");
  assert.equal(notes?.src, "/api/ai/view?url=https%3A%2F%2Fexample.com%2Fnotes.md&as=text");
  const sheet = viewOfUrl("https://example.com/f/budget.xlsx");
  assert.equal(sheet?.kind, "document");
  assert.equal(sheet?.family, "sheet");
  assert.equal(sheet?.src, "/api/ai/view?url=https%3A%2F%2Fexample.com%2Ff%2Fbudget.xlsx&as=document");
  assert.equal(viewOfLink("/reports/q3.docx")?.kind, "document");
  assert.equal(viewOfLink("https://example.com/a.png")?.kind, "image");

  const pdf = viewOfPath("/shared/team/Q3 report.pdf");
  assert.equal(pdf?.src, "/api/files/preview?path=%2Fshared%2Fteam%2FQ3%20report.pdf");
  assert.equal(pdf?.download, "/api/files/download?path=%2Fshared%2Fteam%2FQ3%20report.pdf");
  assert.equal(pdf?.path, "/shared/team/Q3 report.pdf");
  const video = viewOfPath("/videos/demo.mp4");
  assert.equal(video?.src, "/api/files/download?path=%2Fvideos%2Fdemo.mp4", "a video plays from its download");
  assert.equal(viewOfPath("//evil.example/x.png"), null);
  assert.equal(viewOfPath("/api/files/preview?path=x.png"), null);
  assert.equal(viewOfPath("relative/x.png"), null);
  assert.equal(looksLikePath("/home/pic.png"), true);
  assert.equal(looksLikePath("/home/dir/"), false);
  assert.equal(folderOf("/shared/team/Q3 report.pdf"), "/shared/team");
  assert.equal(folderOf("/pic.png"), "/");

  assert.equal(viewOfChatFile({ id: "f1", name: "scan.pdf", kind: "pdf" })?.src, "/api/ai/files/f1");
  const sent = viewOfChatFile({ id: "f2", name: "notes.md", kind: "text" });
  assert.equal(sent?.kind, "text");
  assert.equal(sent?.format, "markdown");
  assert.equal(viewOfChatFile({ id: "f3", name: "page.html", kind: "text" })?.format, "plain", "a sent page shows as its source");

  const doc = viewOfPath("/shared/team/Q3 plan.docx");
  assert.equal(doc?.kind, "document");
  assert.equal(doc?.src, "/api/files/download?path=%2Fshared%2Fteam%2FQ3%20plan.docx", "its bytes go to the converter");
  const md = viewOfPath("/notes/README.md");
  assert.equal(md?.src, "/api/files/text?path=%2Fnotes%2FREADME.md");
  const other = viewOfPath("/backups/site.zip");
  assert.equal(other?.kind, "other", "any file opens: a download and File Station");
  assert.equal(viewKey(doc!), "v:/shared/team/Q3 plan.docx");
  assert.equal(viewKey(viewOfUrl("https://a.io/")!), "v:https://a.io/");
});

test("tabs show what each thing is", () => {
  const kinds: [string, string, string][] = [
    ["/a/plan.docx", "W", "Document"],
    ["/a/budget.xlsx", "XLS", "Spreadsheet"],
    ["/a/deck.pptx", "PPT", "Slides"],
    ["/a/scan.pdf", "PDF", "PDF"],
    ["/a/README.md", "MD", "Markdown"],
    ["/a/data.csv", "CSV", "CSV"],
    ["/a/main.rs", "RS", "Code"],
    ["/a/site.zip", "ZIP", "File"],
  ];
  for (const [path, badge, label] of kinds) {
    const target = viewOfPath(path)!;
    assert.equal(badgeOf(target).text, badge, path);
    assert.equal(kindLabel(target), label, path);
  }
  assert.equal(badgeOf(viewOfUrl("https://a.io/")!).icon, "browser");
  assert.equal(kindLabel(viewOfUrl("https://a.io/")!), "Web page");
});

test("CSV and TSV become rows", () => {
  assert.deepEqual(parseDelimited('name,note\nAda,"says ""hi"", twice"\r\nBo,"two\nlines"\n', ","), [
    ["name", "note"],
    ["Ada", 'says "hi", twice'],
    ["Bo", "two\nlines"],
  ]);
  assert.deepEqual(parseDelimited("a\tb\n1\t2", "\t"), [
    ["a", "b"],
    ["1", "2"],
  ]);
  assert.equal(parseDelimited("1\n2\n3\n4\n", ",", 2).length, 2);
});

test("pictures and file links in answers become links the viewer opens", () => {
  assert.deepEqual(parseInline("![chart](https://a.io/c.png)"), [
    { t: "link", href: "https://a.io/c.png", c: [{ t: "text", v: "chart" }] },
  ]);
  assert.deepEqual(parseInline("see ![](https://a.io/c.png)"), [
    { t: "text", v: "see " },
    { t: "link", href: "https://a.io/c.png", c: [{ t: "text", v: "c.png" }] },
  ]);
  assert.deepEqual(parseInline("[the report](/shared/q3.pdf)"), [
    { t: "file", path: "/shared/q3.pdf", c: [{ t: "text", v: "the report" }] },
  ]);
  assert.deepEqual(parseInline("Wow! [x](javascript:alert(1))"), [{ t: "text", v: "Wow! " }, { t: "text", v: "x" }]);
});

test("the bar under the message box adds up the chat", () => {
  const user = (id: number): Message => ({
    id,
    conversationId: "c",
    role: "user",
    content: [{ type: "text", text: "hi" }],
    providerId: null,
    model: "",
    inputTokens: 0,
    outputTokens: 0,
    status: "done",
    error: "",
    createdAt: 0,
    finishedAt: 0,
  });
  const reply = (id: number, extra: Partial<Message>): Message => ({ ...user(id), role: "assistant", model: "claude-sonnet-5", ...extra });
  const tool = { type: "tool" as const, id: "t", name: "read_file", input: {}, status: "done" as const };
  const stats = chatStats([
    user(1),
    reply(2, { content: [tool, tool], inputTokens: 1000, outputTokens: 200, cacheReadTokens: 900, contextTokens: 700, writingMs: 4000 }),
    user(3),
    reply(4, { inputTokens: 3000, outputTokens: 100, cacheReadTokens: 2700, contextTokens: 1600, writingMs: 100 }),
  ]);
  assert.equal(stats.turns, 2);
  assert.equal(stats.steps, 2);
  assert.equal(stats.tokens, 4300);
  assert.equal(stats.cacheHit, 90);
  // The second reply was too quick to time: the first one's speed stays.
  assert.equal(stats.tokensPerSecond, 50);
  assert.equal(stats.context, 1600);
  assert.equal(stats.model, "claude-sonnet-5");
  assert.equal(chatStats([user(1)]).cacheHit, null);

  assert.equal(contextWindow("anthropic/claude-sonnet-5"), 200_000);
  assert.equal(contextWindow("gemini-3-pro"), 1_048_576);
  assert.equal(contextWindow("GPT-6.2-Sol"), 400_000);
  assert.equal(contextWindow("gpt-oss-120b"), 131_072);
  assert.equal(contextWindow("GLM-5.3"), 200_000);
  assert.equal(contextWindow("my-own-model"), null);

  assert.equal(timeLeft(30_000), "<1m");
  assert.equal(timeLeft(29 * 60_000), "29m");
  assert.equal(timeLeft((4 * 60 + 59) * 60_000), "4h 59m");
  assert.equal(timeLeft((2 * 24 + 7) * 3_600_000), "2d 7h");
});

test("the provider's limits show as bars, reported or counted", () => {
  const now = 1_000_000;
  const account = (extra: Partial<ChatQuotaAccount>): ChatQuotaAccount => ({
    id: "a",
    label: "",
    plan: "plus",
    first: true,
    ready: true,
    resting: null,
    quota: {},
    counted: { hours5: { calls: 0, tokens: 0 }, week: { calls: 0, tokens: 0 } },
    ...extra,
  });
  const codex = account({
    quota: {
      windows: [
        { name: "5 hours", usedPercent: 0.4, resetsAt: now + 299 * 60_000 },
        { name: "Weekly", usedPercent: 79, resetsAt: now + 29 * 60_000 },
        { name: "Credits" },
      ],
    },
  });
  assert.deepEqual(usageBars(codex, now), [
    { name: "5-hour window", used: 0, resetsAt: now + 299 * 60_000 },
    { name: "Weekly", used: 79, resetsAt: now + 29 * 60_000 },
  ]);
  // Without reported windows: what RAST counted against a limit it keeps.
  const counted = account({
    counted: { hours5: { calls: 3, tokens: 50_000, leavesAt: now + 60_000 }, week: { calls: 3, tokens: 50_000 } },
    limits: { hours5: 200_000 },
  });
  assert.deepEqual(usageBars(counted, now), [{ name: "5-hour window", used: 25, resetsAt: now + 60_000 }]);
  assert.deepEqual(usageBars(account({}), now), []);
  // The chip shows an account that may answer.
  const resting = { ...codex, id: "r", resting: { until: now + 1000, reason: "limit" } };
  const provider = { id: "p", name: "Codex", kind: "codex", kindLabel: "ChatGPT (Codex)", signsIn: true, accounts: [resting, { ...codex, id: "b" }] };
  assert.equal(shownAccount(provider, now)?.id, "b");
  // A provider that reports nothing still shows: what RAST counted.
  const quiet = account({ id: "q", counted: { hours5: { calls: 2, tokens: 1200 }, day: { calls: 5, tokens: 9000 }, week: { calls: 9, tokens: 40_000 } } });
  assert.equal(shownAccount({ ...provider, accounts: [quiet] }, now)?.id, "q");
  assert.deepEqual(
    countedFigures(quiet).map((f) => [f.short, f.tokens, f.calls]),
    [
      ["5h", 1200, 2],
      ["24h", 9000, 5],
      ["7d", 40_000, 9],
    ],
  );
  assert.equal(shownAccount({ ...provider, accounts: [] }, now), null);
  // Antigravity keeps a limit per pool of models: the chat model's pool comes first.
  const pools = account({
    quota: {
      windows: [
        { name: "Claude and GPT", usedPercent: 40, models: ["claude-sonnet-5-5-high", "gpt-oss-120b-medium"] },
        { name: "Gemini", usedPercent: 10, models: ["gemini-3-flash"] },
      ],
    },
  });
  assert.deepEqual(
    usageBars(pools, now, "gemini-3-flash").map((b) => [b.name, b.used]),
    [
      ["Gemini", 10],
      ["Claude and GPT", 40],
    ],
  );
  assert.deepEqual(
    usageBars(pools, now).map((b) => b.name),
    ["Claude and GPT", "Gemini"],
  );
});

