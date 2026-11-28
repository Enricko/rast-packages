// The bar under the message box: what the chat used (turns, steps, writing
// speed; tokens and the cache's share), the limits of the provider that
// answers (its windows, time left and share used; a click lists every
// provider's), and how full the model's context is.

import type { AppContext } from "../../core/app";
import { h } from "../../core/dom";
import { iconSvg } from "../../core/icons";
import { providerMark } from "./marks";
import { openPopover, type Popover } from "./popover";
import {
  chatStats,
  contextWindow,
  formatTokens,
  leftTone,
  shownAccount,
  timeLeft,
  usageBars,
  type ChatQuota,
  type ChatQuotaAccount,
  type ChatQuotaProvider,
  type Message,
  type UsageBar,
} from "./model";

export interface UsageBarSource {
  root: HTMLElement;
  /** The open chat's messages (none for a new chat). */
  messages(): Message[];
  /** The provider that answers in this chat (or would, in a new one). */
  providerId(): string | null;
  /** Its model, for the context ring before a reply says. */
  model(): string;
}

export interface UsageBarView {
  el: HTMLElement;
  update(): void;
  /** Ask the server for the limits again. */
  refresh(): Promise<void>;
  dispose(): void;
}

const SVG = "http://www.w3.org/2000/svg";

export function createUsageBar(ctx: AppContext, source: UsageBarSource): UsageBarView {
  const statsText = h("span", {});
  const stats = h("span", { class: "ai-usage-item", title: "Messages sent, tool calls, and how fast the last reply was written" }, iconSvg("activity", 13), statsText);
  const tokensText = h("span", {});
  const tokens = h("span", { class: "ai-usage-item", title: "Tokens this chat used, and the share of its input read back from the provider's cache" }, iconSvg("database", 13), tokensText);
  const quotaText = h("span", { class: "ai-usage-quota-text" });
  const quota = h("button", { class: "ai-usage-item ai-usage-quota", type: "button", title: "Usage limits" }, iconSvg("gauge", 13), quotaText);
  quota.setAttribute("aria-haspopup", "dialog");
  quota.setAttribute("aria-expanded", "false");
  const ring = contextRing();
  const contextText = h("span", {});
  const context = h("span", { class: "ai-usage-item ai-usage-context" }, ring.el, contextText);
  const el = h("div", { class: "ai-usage", role: "status", ariaLabel: "Chat usage" }, stats, tokens, quota, context);

  let data: ChatQuota | null = null;
  let loading: Promise<void> | null = null;
  let popover: Popover | null = null;
  let disposed = false;

  function update(): void {
    const now = Date.now();
    const s = chatStats(source.messages());
    const parts = [`${s.turns} turn${s.turns === 1 ? "" : "s"}`];
    if (s.steps) parts.push(`${s.steps} step${s.steps === 1 ? "" : "s"}`);
    if (s.tokensPerSecond !== null) parts.push(`${Math.round(s.tokensPerSecond)} tok/s`);
    statsText.textContent = parts.join(" · ");
    stats.classList.toggle("hidden", !s.turns);
    tokensText.textContent = s.cacheHit === null ? `${formatTokens(s.tokens)} tok` : `${formatTokens(s.tokens)} tok · Cache hit ${Math.round(s.cacheHit)}%`;
    tokens.classList.toggle("hidden", !s.tokens);

    const model = s.model || source.model();
    const window_ = contextWindow(model);
    const share = window_ && s.context ? Math.min(100, (s.context * 100) / window_) : null;
    context.classList.toggle("hidden", share === null);
    if (share !== null && window_) {
      ring.set(share);
      contextText.textContent = `${Math.round(share)}%`;
      context.title = `Context: about ${formatTokens(s.context)} of ${formatTokens(window_)} tokens that ${model} reads at once`;
      context.classList.toggle("is-mid", share >= 75 && share < 90);
      context.classList.toggle("is-low", share >= 90);
    }

    const provider = data?.providers.find((p) => p.id === source.providerId());
    const account = provider ? shownAccount(provider, now) : null;
    const bars = account ? usageBars(account, now) : [];
    quota.classList.toggle("hidden", !provider || !bars.length);
    if (provider && bars.length) {
      quotaText.replaceChildren(
        h("span", { class: "ai-usage-quota-name" }, provider.name),
        ...bars.slice(0, 2).flatMap((bar, i) => [
          ...(i ? [h("span", { class: "ai-usage-sep" }, "·")] : []),
          h("span", { class: `ai-usage-quota-bar is-${leftTone(100 - bar.used) || "none"}` }, barText(bar, now)),
        ]),
      );
      quota.title = `${provider.name}: ${bars.map((b) => `${b.name} ${b.used}% used${b.resetsAt ? `, resets in ${timeLeft(b.resetsAt - now)}` : ""}`).join("; ")}`;
    }
    el.classList.toggle("hidden", Array.from(el.children).every((c) => c.classList.contains("hidden")));
    if (popover) drawPopover();
  }

  async function refresh(): Promise<void> {
    if (loading) return loading;
    loading = (async () => {
      try {
        data = await ctx.api.get<ChatQuota>("/api/ai/quota");
      } catch {
        // The bar keeps what it had; the chat works without it.
      } finally {
        loading = null;
      }
      if (!disposed) update();
    })();
    return loading;
  }

  function drawPopover(): void {
    if (!popover) return;
    const now = Date.now();
    const current = source.providerId();
    const providers = (data?.providers ?? [])
      .map((p) => ({ provider: p, accounts: p.accounts.filter((a) => usageBars(a, now).length) }))
      .filter((p) => p.accounts.length)
      .sort((a, b) => Number(b.provider.id === current) - Number(a.provider.id === current));
    const checked = Math.max(0, ...(data?.providers ?? []).flatMap((p) => p.accounts.map((a) => a.quota.checkedAt ?? 0)));
    popover.el.replaceChildren(
      h("div", { class: "ai-qp-head" }, iconSvg("gauge", 15), h("strong", {}, "Usage limits")),
      ...(providers.length
        ? providers.map(({ provider, accounts }) => providerBlock(provider, accounts, provider.id === current, now))
        : [h("p", { class: "small muted ai-qp-empty" }, "The providers you use have not reported limits yet. They show here after a reply.")]),
      ...(checked ? [h("div", { class: "ai-qp-foot small muted" }, `Reported ${agoText(now - checked)}`)] : []),
    );
    popover.place();
  }

  quota.addEventListener("click", () => {
    if (popover) {
      popover.close();
      return;
    }
    popover = openPopover(source.root, quota, h("div", {}), {
      className: "ai-qp",
      label: "Usage limits",
      onClose: () => {
        popover = null;
      },
    });
    drawPopover();
    void refresh();
  });

  // Time left moves on; the limits are asked for again now and then.
  const tick = window.setInterval(() => update(), 30_000);
  const poll = window.setInterval(() => {
    if (!document.hidden) void refresh();
  }, 5 * 60_000);

  return {
    el,
    update,
    refresh,
    dispose() {
      disposed = true;
      window.clearInterval(tick);
      window.clearInterval(poll);
      popover?.close();
    },
  };
}

function barText(bar: UsageBar, now: number): string {
  return bar.resetsAt ? `${timeLeft(bar.resetsAt - now)} ${bar.used}%` : `${bar.used}%`;
}

function providerBlock(provider: ChatQuotaProvider, accounts: ChatQuotaAccount[], current: boolean, now: number): HTMLElement {
  const shown = shownAccount(provider, now);
  const several = accounts.length > 1;
  const who = (account: ChatQuotaAccount) => {
    const bits = [account.label, account.plan ? `Plan: ${account.plan}` : ""].filter(Boolean).join(" · ");
    const resting = account.resting && account.resting.until > now ? ` · rests ${timeLeft(account.resting.until - now)}` : "";
    return bits || resting
      ? h("span", { class: "ai-qp-who" }, account.id === shown?.id ? iconSvg("sparkle", 12) : null, `${bits}${resting}`)
      : null;
  };
  return h(
    "section",
    { class: "ai-qp-provider" },
    h(
      "div",
      { class: "ai-qp-title" },
      providerMark(provider.kind, 18),
      h("strong", {}, provider.name),
      current ? h("span", { class: "ai-qp-current" }, "current") : null,
      several ? null : who(accounts[0] as ChatQuotaAccount),
    ),
    ...accounts.flatMap((account, i) => [
      several ? h("div", { class: "ai-qp-account" }, who(account) ?? h("span", { class: "ai-qp-who" }, `Account ${i + 1}`)) : null,
      ...usageBars(account, now).map((bar) => barRow(bar, now)),
    ]),
  );
}

function barRow(bar: UsageBar, now: number): HTMLElement {
  const fill = h("span", { class: "ai-qp-fill" });
  fill.style.width = `${Math.max(0, Math.min(100, bar.used))}%`;
  return h(
    "div",
    { class: `ai-qp-bar is-${leftTone(100 - bar.used) || "none"}` },
    h("span", { class: "ai-qp-name" }, bar.name),
    h("span", { class: "ai-qp-value" }, bar.resetsAt ? `${bar.used}% · ${timeLeft(bar.resetsAt - now)}` : `${bar.used}%`),
    h("span", { class: "ai-qp-track", role: "meter", ariaLabel: `${bar.name}: ${bar.used}% used` }, fill),
  );
}

function agoText(ms: number): string {
  const minutes = Math.floor(ms / 60_000);
  if (minutes < 1) return "just now";
  return `${timeLeft(ms)} ago`;
}

/** A small ring that fills with the share of the context used. */
function contextRing(): { el: Element; set(share: number): void } {
  const svg = document.createElementNS(SVG, "svg");
  svg.setAttribute("viewBox", "0 0 16 16");
  svg.setAttribute("width", "14");
  svg.setAttribute("height", "14");
  svg.setAttribute("aria-hidden", "true");
  svg.classList.add("ai-ring");
  const track = document.createElementNS(SVG, "circle");
  const fill = document.createElementNS(SVG, "circle");
  for (const c of [track, fill]) {
    c.setAttribute("cx", "8");
    c.setAttribute("cy", "8");
    c.setAttribute("r", "6");
    c.setAttribute("fill", "none");
    c.setAttribute("stroke-width", "2.4");
  }
  track.classList.add("ai-ring-track");
  fill.classList.add("ai-ring-fill");
  const length = 2 * Math.PI * 6;
  fill.setAttribute("stroke-dasharray", `${length}`);
  fill.setAttribute("transform", "rotate(-90 8 8)");
  fill.setAttribute("stroke-linecap", "round");
  svg.append(track, fill);
  return {
    el: svg,
    set(share: number) {
      fill.setAttribute("stroke-dashoffset", `${length * (1 - Math.max(0, Math.min(100, share)) / 100)}`);
    },
  };
}
