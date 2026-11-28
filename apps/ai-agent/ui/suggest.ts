// Composer autocomplete popover for `/` (commands & skills) and `@`
// (apps, browser, rules, files, conversations, skills).
//
// Pure trigger/replacement logic is exported for unit tests; the UI controller
// attaches directly to the composer textarea and renders a floating menu right
// above `.ai-input-wrap`.

import type { AppContext } from "../../core/app";
import { clear, h } from "../../core/dom";
import { appIconSvg, iconSvg } from "../../core/icons";
import type { FileEntry, FileListing } from "../../core/types";
import type { Conversation, PublicSkill } from "./model";
import type { Workspace } from "./workspaces";

export type TriggerKind = "slash" | "mention" | "file" | "chat";

export interface ActiveTrigger {
  kind: TriggerKind;
  /** Inclusive index in `value` where the trigger token starts (`/` or `@`). */
  start: number;
  /** Exclusive index (the caret position). */
  end: number;
  /** Filter text typed after the prefix. */
  query: string;
}

export interface SuggestItem {
  id: string;
  group: string;
  label: string;
  description: string;
  /** Icon name in `core/icons.ts`, or `app:<id>` to use `appIconSvg`. */
  icon: string;
  /** Text inserted in place of `[start, end]`. */
  insert: string;
  /** Keep the menu open right after insertion (e.g. `@file:` / `@chat:`). */
  continueSuggest?: boolean;
  /** Execute immediately instead of inserting text (e.g. `/compact`). */
  run?: () => void;
}

/**
 * Inspect the textarea value and caret to find an active `/` or `@` trigger.
 * - `/` only activates at the start of the input (or right after a leading `/cmd `).
 * - `@` activates anywhere at a word boundary (never inside an email address).
 * - `@file:<q>` and `@chat:<q>` enter sub-search modes.
 */
export function findTrigger(value: string, caret: number): ActiveTrigger | null {
  if (caret < 0 || caret > value.length) return null;
  const before = value.slice(0, caret);

  // Check `@` first (closest to caret on the current token).
  const lastSpace = Math.max(before.lastIndexOf(" "), before.lastIndexOf("\n"), before.lastIndexOf("\t"));
  const tokenStart = lastSpace + 1;
  const token = before.slice(tokenStart);

  if (token.startsWith("@")) {
    const rest = token.slice(1);
    if (rest.startsWith("file:")) {
      return { kind: "file", start: tokenStart, end: caret, query: rest.slice(5) };
    }
    if (rest.startsWith("chat:")) {
      return { kind: "chat", start: tokenStart, end: caret, query: rest.slice(5) };
    }
    if (/^[a-zA-Z0-9_-]*$/.test(rest)) {
      return { kind: "mention", start: tokenStart, end: caret, query: rest };
    }
    return null;
  }

  if (token.startsWith("/")) {
    // Only allow `/` at the very beginning or chained with earlier leading `/cmd` tokens.
    const prefix = before.slice(0, tokenStart).trim();
    const onlySlashBefore =
      prefix.length === 0 ||
      prefix.split(/\s+/).every((part) => /^\/[a-zA-Z0-9_-]+$/.test(part));
    const rest = token.slice(1);
    if (onlySlashBefore && /^[a-zA-Z0-9_-]*$/.test(rest)) {
      return { kind: "slash", start: tokenStart, end: caret, query: rest };
    }
  }

  return null;
}

/** Replace `[trigger.start, trigger.end]` with `replacement` and return the new text + caret. */
export function applyTrigger(
  value: string,
  trigger: Pick<ActiveTrigger, "start" | "end">,
  replacement: string,
): { value: string; caret: number } {
  const head = value.slice(0, trigger.start);
  const tail = value.slice(trigger.end);
  const next = `${head}${replacement}${tail}`;
  return { value: next, caret: head.length + replacement.length };
}

/** Format a file path for `@file:` insertion (quotes paths containing spaces). */
export function formatFileMention(path: string): string {
  const clean = path.startsWith("/") ? path : `/${path}`;
  return /\s/.test(clean) ? `@file:"${clean}" ` : `@file:${clean} `;
}

export interface BuiltinCommand {
  slug: string;
  description: string;
  icon: string;
  requiresApp?: string;
  immediate?: boolean;
}

export const BUILTIN_COMMANDS: BuiltinCommand[] = [
  { slug: "plan", description: "Plan carefully before making changes.", icon: "list" },
  { slug: "goal", description: "Work until the goal is completely finished and verified.", icon: "check" },
  { slug: "compact", description: "Compact the conversation now to free up context window.", icon: "layers", immediate: true },
  { slug: "browser", description: "Use the browser agent for web tasks.", icon: "browser" },
  { slug: "btw", description: "Ask a quick side question without changing the main task.", icon: "info" },
  { slug: "grill-me", description: "Interview me with focused questions to align on a plan.", icon: "compose" },
  { slug: "code", description: "Act as a coding agent in Code Editor.", icon: "app:code", requiresApp: "code" },
  { slug: "word", description: "Write and format documents in WordCraft.", icon: "app:wordcraft", requiresApp: "wordcraft" },
];

export interface SuggestDeps {
  ctx: AppContext;
  input: HTMLTextAreaElement;
  anchor: HTMLElement;
  skills: () => PublicSkill[];
  conversations: () => Conversation[];
  currentConversationId: () => string | null;
  workspace: () => Workspace | null;
  onCompact: () => void;
  onChange: () => void;
}

export interface ComposerSuggest {
  /** Handle a keydown event on the textarea; returns true if consumed. */
  onKeyDown(event: KeyboardEvent): boolean;
  /** Re-evaluate suggestions after input/selection changes. */
  refresh(): void;
  /** Close the popover if open. */
  close(): void;
  /** Clean up listeners. */
  dispose(): void;
}

export function createComposerSuggest(deps: SuggestDeps): ComposerSuggest {
  const { ctx, input, anchor } = deps;
  const menu = h("div", { class: "ai-suggest hidden", role: "listbox", ariaLabel: "Suggestions" });
  const list = h("div", { class: "ai-suggest-list" });
  const footer = h("div", { class: "ai-suggest-foot" }, "↑↓ navigate · Enter or Tab to select · Esc to close");
  menu.append(list, footer);
  anchor.appendChild(menu);

  let active: ActiveTrigger | null = null;
  let items: SuggestItem[] = [];
  let selected = 0;
  let fileTimer = 0;
  let fileSeq = 0;
  let fileCache: { key: string; entries: FileEntry[] } = { key: "", entries: [] };

  function isOpen(): boolean {
    return !menu.classList.contains("hidden");
  }

  function close(): void {
    active = null;
    items = [];
    selected = 0;
    menu.classList.add("hidden");
    clear(list);
  }

  function installedApps() {
    return ctx.desktop
      .apps()
      .filter((a) => a.id !== "agent" && a.id !== "launcher");
  }

  function buildStaticItems(trigger: ActiveTrigger): SuggestItem[] {
    const q = trigger.query.trim().toLowerCase();
    const matches = (a: string, b = "") => !q || a.toLowerCase().includes(q) || b.toLowerCase().includes(q);
    const apps = installedApps();
    const appIds = new Set(apps.map((a) => a.id));
    const skills = deps.skills();

    if (trigger.kind === "slash") {
      const out: SuggestItem[] = [];
      for (const cmd of BUILTIN_COMMANDS) {
        if (cmd.requiresApp && !appIds.has(cmd.requiresApp)) continue;
        if (!matches(cmd.slug, cmd.description)) continue;
        out.push({
          id: `cmd:${cmd.slug}`,
          group: "Commands",
          label: `/${cmd.slug}`,
          description: cmd.description,
          icon: cmd.icon,
          insert: `/${cmd.slug} `,
          run: cmd.immediate ? () => deps.onCompact() : undefined,
        });
      }
      for (const skill of skills) {
        if (!matches(skill.slug, `${skill.name} ${skill.description}`)) continue;
        out.push({
          id: `skill:${skill.id}`,
          group: "Skills",
          label: `/${skill.slug}`,
          description: skill.description || skill.name,
          icon: "sparkle",
          insert: `/${skill.slug} `,
        });
      }
      return out;
    }

    if (trigger.kind === "mention") {
      const out: SuggestItem[] = [];
      // Browse & Context
      const browse: SuggestItem[] = [
        {
          id: "browse:file",
          group: "Context",
          label: "@file:",
          description: "Attach a file from your RAST files or workspace",
          icon: "file",
          insert: "@file:",
          continueSuggest: true,
        },
        {
          id: "browse:rules",
          group: "Context",
          label: "@rules",
          description: "Include workspace rules (AGENTS.md / CLAUDE.md / README.md)",
          icon: "shield",
          insert: "@rules ",
        },
        {
          id: "browse:chat",
          group: "Context",
          label: "@chat:",
          description: "Reference another conversation",
          icon: "compose",
          insert: "@chat:",
          continueSuggest: true,
        },
        {
          id: "browse:browser",
          group: "Tools",
          label: "@browser",
          description: "Use the built-in web browser agent",
          icon: "browser",
          insert: "@browser ",
        },
      ];
      for (const item of browse) {
        if (matches(item.label, item.description)) out.push(item);
      }

      // Installed RAST Apps
      for (const app of apps) {
        if (!matches(app.id, `${app.name} ${app.description}`)) continue;
        out.push({
          id: `app:${app.id}`,
          group: "Apps",
          label: `@${app.id}`,
          description: app.description ? `${app.name} — ${app.description}` : app.name,
          icon: `app:${app.id}`,
          insert: `@${app.id} `,
        });
      }

      // Custom Skills
      for (const skill of skills) {
        if (!matches(skill.slug, `${skill.name} ${skill.description}`)) continue;
        out.push({
          id: `mskill:${skill.id}`,
          group: "Skills",
          label: `@${skill.slug}`,
          description: skill.description || skill.name,
          icon: "sparkle",
          insert: `@${skill.slug} `,
        });
      }
      return out;
    }

    if (trigger.kind === "chat") {
      const curId = deps.currentConversationId();
      const out: SuggestItem[] = [];
      for (const c of deps.conversations()) {
        if (c.id === curId) continue;
        const title = c.title || "New chat";
        if (!matches(title, c.id)) continue;
        out.push({
          id: `chat:${c.id}`,
          group: "Conversations",
          label: title,
          description: `@chat:${c.id}`,
          icon: "compose",
          insert: `@chat:${c.id} `,
        });
        if (out.length >= 20) break;
      }
      return out;
    }

    return [];
  }

  function renderIcon(icon: string): HTMLElement {
    if (icon.startsWith("app:")) {
      const appId = icon.slice(4);
      const wrap = h("span", { class: "ai-suggest-ico" });
      wrap.appendChild(appIconSvg(appId, "apps", 16));
      return wrap;
    }
    return h("span", { class: "ai-suggest-ico" }, iconSvg(icon, 15));
  }

  function render(loading = false): void {
    clear(list);
    if (loading) {
      list.appendChild(h("div", { class: "ai-suggest-empty" }, "Searching files…"));
      menu.classList.remove("hidden");
      return;
    }
    if (items.length === 0) {
      if (active && (active.kind === "file" || active.kind === "chat")) {
        list.appendChild(
          h(
            "div",
            { class: "ai-suggest-empty" },
            active.kind === "file" ? "No matching files found." : "No matching conversations.",
          ),
        );
        menu.classList.remove("hidden");
        return;
      }
      menu.classList.add("hidden");
      return;
    }
    if (selected >= items.length) selected = 0;
    if (selected < 0) selected = items.length - 1;

    let currentGroup = "";
    items.forEach((item, index) => {
      if (item.group !== currentGroup) {
        currentGroup = item.group;
        list.appendChild(h("div", { class: "ai-suggest-group" }, currentGroup));
      }
      const row = h(
        "button",
        {
          class: `ai-suggest-item${index === selected ? " is-active" : ""}`,
          type: "button",
          role: "option",
          ariaSelected: index === selected,
          dataset: { idx: String(index) },
        },
        renderIcon(item.icon),
        h("span", { class: "ai-suggest-name" }, item.label),
        h("span", { class: "ai-suggest-desc" }, item.description),
      );
      row.addEventListener("pointerenter", () => {
        if (selected === index) return;
        selected = index;
        updateHighlight();
      });
      row.addEventListener("pointerdown", (event) => {
        // Keep focus on the textarea when clicking an item.
        event.preventDefault();
        pick(index);
      });
      list.appendChild(row);
    });
    menu.classList.remove("hidden");
    scrollActiveIntoView();
  }

  function updateHighlight(): void {
    for (const el of list.querySelectorAll<HTMLElement>(".ai-suggest-item")) {
      const idx = Number(el.dataset.idx);
      const on = idx === selected;
      el.classList.toggle("is-active", on);
      el.setAttribute("aria-selected", String(on));
    }
    scrollActiveIntoView();
  }

  function scrollActiveIntoView(): void {
    const activeEl = list.querySelector<HTMLElement>(".ai-suggest-item.is-active");
    if (activeEl && typeof activeEl.scrollIntoView === "function") {
      activeEl.scrollIntoView({ block: "nearest" });
    }
  }

  function pick(index: number): void {
    const item = items[index];
    const trigger = active;
    if (!item || !trigger) return;
    if (item.run) {
      const next = applyTrigger(input.value, trigger, "");
      input.value = next.value;
      input.setSelectionRange(next.caret, next.caret);
      close();
      deps.onChange();
      item.run();
      return;
    }
    const next = applyTrigger(input.value, trigger, item.insert);
    input.value = next.value;
    input.setSelectionRange(next.caret, next.caret);
    input.focus();
    deps.onChange();
    if (item.continueSuggest) {
      refresh();
    } else {
      close();
    }
  }

  async function loadFiles(trigger: ActiveTrigger): Promise<void> {
    const ws = deps.workspace();
    const basePath = ws && ws.place === "files" ? ws.path : "/";
    const q = trigger.query.trim();
    const cacheKey = `${basePath}|${q}`;
    const seq = ++fileSeq;

    const showEntries = (entries: FileEntry[]) => {
      if (seq !== fileSeq || !active || active.kind !== "file") return;
      items = entries.slice(0, 24).map((entry) => ({
        id: `file:${entry.path}`,
        group: basePath === "/" ? "Files" : `Files in ${basePath}`,
        label: entry.name,
        description: entry.path,
        icon: entry.kind === "dir" ? "folder" : "file",
        insert: formatFileMention(entry.path),
      }));
      selected = 0;
      render(false);
    };

    if (fileCache.key === cacheKey) {
      showEntries(fileCache.entries);
      return;
    }

    render(true);
    try {
      let entries: FileEntry[] = [];
      if (q) {
        const res = await ctx.api.get<{ items?: FileEntry[]; entries?: FileEntry[] }>(
          `/api/files/search?path=${encodeURIComponent(basePath)}&q=${encodeURIComponent(q)}&per_page=25`,
        );
        entries = res.items ?? res.entries ?? [];
      } else {
        const listing = await ctx.api.get<FileListing>(`/api/files?path=${encodeURIComponent(basePath)}&limit=40`);
        entries = listing.items ?? [];
      }
      entries = entries.filter((e) => e.kind !== "dir");
      fileCache = { key: cacheKey, entries };
      showEntries(entries);
    } catch {
      if (seq === fileSeq) {
        items = [];
        render(false);
      }
    }
  }

  function refresh(): void {
    window.clearTimeout(fileTimer);
    if (input.disabled) {
      close();
      return;
    }
    const caret = input.selectionStart ?? input.value.length;
    if ((input.selectionEnd ?? caret) !== caret) {
      close();
      return;
    }
    const trigger = findTrigger(input.value, caret);
    if (!trigger) {
      close();
      return;
    }
    active = trigger;
    if (trigger.kind === "file") {
      fileTimer = window.setTimeout(() => void loadFiles(trigger), trigger.query ? 140 : 0);
      return;
    }
    items = buildStaticItems(trigger);
    selected = 0;
    render(false);
  }

  function onKeyDown(event: KeyboardEvent): boolean {
    if (!isOpen()) return false;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      if (items.length) {
        selected = (selected + 1) % items.length;
        updateHighlight();
      }
      return true;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      if (items.length) {
        selected = (selected - 1 + items.length) % items.length;
        updateHighlight();
      }
      return true;
    }
    if ((event.key === "Enter" || event.key === "Tab") && !event.shiftKey && !event.isComposing) {
      if (items.length) {
        event.preventDefault();
        pick(selected);
        return true;
      }
    }
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close();
      return true;
    }
    return false;
  }

  const onOutside = (event: PointerEvent) => {
    if (!isOpen()) return;
    const target = event.target as Node | null;
    if (target && !menu.contains(target) && target !== input) close();
  };
  const onInput = () => refresh();
  const onClick = () => refresh();
  const onKeyUp = (event: KeyboardEvent) => {
    if (event.key === "ArrowLeft" || event.key === "ArrowRight" || event.key === "Home" || event.key === "End") {
      refresh();
    }
  };

  input.addEventListener("input", onInput);
  input.addEventListener("click", onClick);
  input.addEventListener("keyup", onKeyUp);
  document.addEventListener("pointerdown", onOutside, true);

  return {
    onKeyDown,
    refresh,
    close,
    dispose() {
      window.clearTimeout(fileTimer);
      input.removeEventListener("input", onInput);
      input.removeEventListener("click", onClick);
      input.removeEventListener("keyup", onKeyUp);
      document.removeEventListener("pointerdown", onOutside, true);
      menu.remove();
    },
  };
}
