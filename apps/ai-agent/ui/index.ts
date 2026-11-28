// AI Agent: chat with the agents and models an administrator sets up
// (src/ai). Each account has its own conversations; replies are written on
// the server and stream to every open page (`ai.delta`), so a reload or
// another device picks a reply up where it is. Administrators also get the
// settings (providers, agents, skills, limits) and can read everyone's
// conversations (settings.ts). Agents may ask to use tools (files, commands,
// the web); each call shows as a card the chat's owner allows or denies.

import "./agent.css";
import type { AppContext, AppDefinition, AppInstance } from "../../core/app";
import { ApiError } from "../../core/api";
import { h } from "../../core/dom";
import { appIconSvg, iconSvg } from "../../core/icons";
import { debounceTask, getRealtime } from "../../core/realtime";
import { confirmDialog, contextMenu, copyText, dialogOpen, errorMessage, promptDialog } from "../../core/ui";
import { agentAvatar, providerMark } from "./marks";
import { artifactCard, artifactKey, createArtifactView, type ArtifactView } from "./artifact";
import { createAttachments, picture, sentFiles } from "./attachments";
import { createSidePanel, type TabSpec } from "./panel";
import { badgeOf, folderOf, isWebUrl, kindLabel, looksLikePath, viewOfChatFile, viewOfLink, viewOfPath, viewOfUrl, type ViewTarget } from "./view";
import { createFileView } from "./viewer";
import { createModelMenu } from "./modelmenu";
import { createUsageBar } from "./usagebar";
import { formatEstimate } from "./usage";
import { renderMarkdown } from "./markdown";
import { showWhatsNew, updateBanner } from "./update";
import {
  allowanceText,
  allowanceUsed,
  applyDelta,
  EFFORTS,
  formatTokens,
  formatElapsed,
  chatTotals,
  replyActivity,
  retryLabel,
  groupByDay,
  maySkipAsking,
  messageText,
  messageFiles,
  PERMISSION_MODES,
  permissionMode,
  TOOL_INFO,
  chatArtifacts,
  fileUrl,
  toolArgument,
  toolBlocks,
  upsertMessage,
  type ContentBlock,
  type Conversation,
  type ConversationDetail,
  type ConversationEvent,
  type DeltaEvent,
  type Effort,
  type Message,
  type MessageEvent,
  type Options,
  type Retry,
  type ToolBlock,
} from "./model";
import { mountSettings, type SettingsView } from "./settings";

export const agentApp: AppDefinition = {
  id: "agent",
  name: "AI Agent",
  icon: "activity",
  description: "Chat with AI agents on the models your administrator sets up",
  singleton: true,
  defaultSize: { w: 1000, h: 680 },
  minSize: { w: 420, h: 360 },
  mount(ctx) {
    return mountAgentApp(ctx);
  },
};

interface Params {
  conversationId?: string;
  settings?: boolean;
  /** The artifact version open beside the chat (before tabs). */
  artifact?: string;
  /** The panel's tabs, the one in front and whether it is hidden. */
  tabs?: SavedTab[];
  active?: number;
  hidden?: boolean;
}

/** A tab of the panel, as the window keeps it: an artifact (`a`), a path
 * in the person's files (`p`), a web address (`u`) or a file sent in the
 * chat (`f`, with its name and kind). */
type SavedTab = { a: string } | { p: string } | { u: string } | { f: string; n: string; k: string };

const MAX_SAVED_TABS = 12;

function savedTab(value: unknown): SavedTab | null {
  if (!value || typeof value !== "object") return null;
  const raw = value as Record<string, unknown>;
  const text = (v: unknown, max = 4096) => (typeof v === "string" && v.length > 0 && v.length <= max ? v : null);
  if (text(raw.a, 200)) return { a: raw.a as string };
  if (text(raw.p)) return { p: raw.p as string };
  if (text(raw.u)) return { u: raw.u as string };
  if (text(raw.f, 200) && text(raw.n, 500) && text(raw.k, 20)) return { f: raw.f as string, n: raw.n as string, k: raw.k as string };
  return null;
}

const CHOICE_KEY = "rast.agent.choice";

/** Per-browser conveniences (last choice, effort, hidden chat list). */
function readLocal(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeLocal(key: string, value: string): void {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    /* per-browser convenience only */
  }
}

function readChoice(): string {
  return readLocal(CHOICE_KEY) ?? "";
}

function writeChoice(value: string): void {
  writeLocal(CHOICE_KEY, value);
}

/** Picker values: `agent:<id>` or `model:<providerId>|<model>`. */
function choiceOf(conversation: Pick<Conversation, "agentId" | "providerId" | "model">): string {
  if (conversation.agentId) return `agent:${conversation.agentId}`;
  if (conversation.providerId) return `model:${conversation.providerId}|${conversation.model}`;
  return "";
}

function choiceBody(choice: string): { agentId?: string; providerId?: string; model?: string } {
  if (choice.startsWith("agent:")) return { agentId: choice.slice(6) };
  if (choice.startsWith("model:")) {
    const rest = choice.slice(6);
    const bar = rest.indexOf("|");
    return { providerId: rest.slice(0, bar), model: rest.slice(bar + 1) };
  }
  return {};
}

function asParams(value: unknown): Params {
  if (!value || typeof value !== "object") return {};
  const raw = value as Record<string, unknown>;
  return {
    ...(typeof raw.conversationId === "string" ? { conversationId: raw.conversationId } : {}),
    ...(raw.settings === true ? { settings: true } : {}),
    ...(typeof raw.artifact === "string" ? { artifact: raw.artifact } : {}),
    ...(Array.isArray(raw.tabs)
      ? {
          tabs: raw.tabs
            .slice(0, MAX_SAVED_TABS)
            .map(savedTab)
            .filter((t): t is SavedTab => t !== null),
        }
      : {}),
    ...(typeof raw.active === "number" && Number.isInteger(raw.active) ? { active: raw.active } : {}),
    ...(raw.hidden === true ? { hidden: true } : {}),
  };
}

function mountAgentApp(ctx: AppContext): AppInstance {
  ctx.root.classList.add("app-agent");
  const realtime = getRealtime(ctx.events);
  const me = ctx.store.get().user?.id ?? -1;
  const isAdmin = ctx.store.isAdmin();
  let disposed = false;
  let options: Options | null = null;
  let conversations: Conversation[] = [];
  let current: ConversationDetail | null = null;
  let loadToken = 0;
  let settings: SettingsView | null = null;
  const unsubscribers: Array<() => void> = [];

  // ------------------------------------------------------------- layout
  const newButton = h(
    "button",
    { class: "ai-new", type: "button", onClick: () => openNew() },
    iconSvg("compose", 16),
    h("span", {}, "New chat"),
  );
  const search = h("input", {
    class: "ai-search",
    type: "search",
    placeholder: "Search chats",
    ariaLabel: "Search chats",
    autocomplete: "off",
  });
  const list = h("nav", { class: "ai-list", ariaLabel: "Chats" });
  const allowanceEl = h("div", { class: "ai-allowance hidden" });
  const updater = updateBanner(ctx, realtime, isAdmin);
  const settingsButton = h(
    "button",
    { class: "btn btn-ghost ai-settings-btn", type: "button", onClick: () => openSettings() },
    iconSvg("settings", 15),
    h("span", {}, "Settings"),
  );
  if (!isAdmin) settingsButton.classList.add("hidden");
  const sideClose = h(
    "button",
    { class: "btn btn-ghost btn-icon btn-sm ai-side-close", type: "button", title: "Hide chats", ariaLabel: "Hide chats" },
    iconSvg("menu", 16),
  );
  const side = h(
    "aside",
    { class: "ai-side" },
    h(
      "div",
      { class: "ai-brand" },
      h("span", { class: "ai-brand-icon" }, appIconSvg("agent", "activity", 22)),
      h("span", { class: "ai-brand-name" }, "AI Agent"),
      sideClose,
    ),
    newButton,
    h("label", { class: "ai-search-wrap" }, iconSvg("search", 14), search),
    list,
    h("div", { class: "ai-side-foot" }, updater.el, allowanceEl, settingsButton),
  );

  const titleEl = h("h2", { class: "ai-title" }, "New chat");
  const subEl = h("span", { class: "ai-sub" });
  const ownerEl = h("span", { class: "ai-owner hidden" });
  const headAvatar = h("span", { class: "ai-head-avatar" });
  // The choices (agents, then each provider's models) and the one picked;
  // the model menu under the message box shows and changes them.
  const picker = h("select", { class: "ai-picker", ariaLabel: "Agent or model" });
  const menuButton = h(
    "button",
    { class: "btn btn-ghost btn-icon btn-sm", type: "button", title: "More", ariaLabel: "More" },
    iconSvg("more", 16),
  );
  const sideToggle = h(
    "button",
    { class: "btn btn-ghost btn-icon btn-sm ai-side-toggle", type: "button", title: "Chats", ariaLabel: "Show chats" },
    iconSvg("menu", 16),
  );
  // Brings back a hidden panel and its tabs.
  const panelButton = h(
    "button",
    { class: "btn btn-ghost btn-sm ai-panel-show hidden", type: "button", title: "Show the panel", ariaLabel: "Show the panel" },
    iconSvg("layers", 15),
    h("span", { class: "ai-panel-count" }),
  );
  const head = h(
    "header",
    { class: "ai-head" },
    sideToggle,
    headAvatar,
    h("div", { class: "ai-head-text" }, titleEl, h("span", { class: "ai-head-sub" }, subEl, ownerEl)),
    panelButton,
    menuButton,
  );
  const thread = h("div", { class: "ai-thread" });
  // The chat keeps to its own scrolling (it opens at the latest message and
  // follows a reply), not to the window's saved position.
  const scroller = h("div", { class: "ai-scroll", dataset: { sync: "off" } }, thread);
  const jumpButton = h(
    "button",
    { class: "ai-jump", type: "button", title: "Go to the latest", ariaLabel: "Go to the latest" },
    iconSvg("arrowDown", 16),
  );
  const scrollArea = h("div", { class: "ai-scroll-area" }, scroller, jumpButton);
  const input = h("textarea", {
    class: "ai-input",
    rows: 1,
    placeholder: "Message",
    ariaLabel: "Message",
    spellcheck: true,
  });
  const sendButton = h(
    "button",
    { class: "btn btn-primary btn-icon ai-send", type: "button", title: "Send (Enter)", ariaLabel: "Send" },
    iconSvg("arrowUp", 17),
  );
  // What may run without asking, and how hard the model thinks.
  const guardLabel = h("span", { class: "ai-chip-label" });
  const guard = h(
    "button",
    { class: "ai-chip ai-guard", type: "button", title: "What the agent may do without asking", ariaLabel: "Permissions" },
    iconSvg("shield", 13),
    guardLabel,
    iconSvg("chevronDown", 12),
  );
  guard.setAttribute("aria-haspopup", "menu");
  const composerNote = h("div", { class: "ai-composer-note small muted" });
  // Files for the next message: the attach button, a drop on the chat or a
  // paste; they upload at once and wait above the text.
  const attachments = createAttachments(ctx, () => updateComposer());
  // The model and how hard it thinks: one chip with a menu.
  const modelMenu = createModelMenu({
    root: ctx.root,
    picker,
    providers: () => options?.providers ?? [],
    effort: () => (current ? (current.conversation.effort ?? "") : draftEffort),
    describe: (choice) => describe(choice),
    onPick: (choice) => {
      picker.value = choice;
      picker.dispatchEvent(new Event("change"));
    },
    onEffort: (effort) => void setEffort(effort),
  });
  // What the chat used and the limits of the provider answering.
  const usage = createUsageBar(ctx, {
    root: ctx.root,
    messages: () => current?.messages ?? [],
    providerId: () => answering().providerId,
    model: () => answering().model,
  });
  const composer = h(
    "div",
    { class: "ai-composer" },
    composerNote,
    h(
      "div",
      { class: "ai-input-wrap" },
      attachments.tray,
      input,
      h("div", { class: "ai-composer-bar" }, attachments.button, modelMenu.button, guard, sendButton),
    ),
    usage.el,
  );
  const dropHint = h("div", { class: "ai-drop-hint", ariaHidden: true }, iconSvg("attach", 22), h("span", {}, "Drop files to add them"));
  const main = h("section", { class: "ai-main" }, head, scrollArea, composer, dropHint);
  const settingsHost = h("section", { class: "ai-settings hidden" });
  const scrim = h("div", { class: "ai-scrim" });
  // The panel beside the chat: tabs of artifacts, files and web pages
  // (panel.ts). Each chat keeps its own tabs (with the window, and while
  // this window is open when another chat is in front).
  let restoring = false;
  const panel = createSidePanel(ctx, {
    onAdd: (text) => {
      const target = isWebUrl(text) ? viewOfUrl(text) : looksLikePath(text) ? viewOfPath(text) : /^[\w-]+(\.[\w-]+)+(\/\S*)?$/.test(text) ? viewOfUrl(`https://${text}`) : null;
      if (!target) return false;
      openView(target);
      return true;
    },
    onChange: () => {
      syncPanel();
      if (!restoring) saveWindow();
    },
  });
  ctx.root.append(side, main, panel.el, settingsHost, scrim);
  const tabMemory = new Map<string, Params>();

  panelButton.addEventListener("click", () => panel.setHidden(false));

  /** The panel shows beside the chat (not over Settings). */
  function syncPanel(): void {
    const shown = panel.isOpen() && settingsHost.classList.contains("hidden");
    const was = ctx.root.classList.contains("has-artifact");
    ctx.root.classList.toggle("has-artifact", shown);
    if (shown && !was) ctx.root.classList.remove("ai-side-open");
    const tabs = panel.views().length;
    panelButton.classList.toggle("hidden", !tabs || panel.isOpen());
    panelButton.querySelector(".ai-panel-count")!.textContent = String(tabs);
    panelButton.title = tabs === 1 ? "Show the panel (1 tab)" : `Show the panel (${tabs} tabs)`;
  }

  function tabsState(): Pick<Params, "tabs" | "active" | "hidden"> {
    const views = panel.views();
    const tabs: SavedTab[] = [];
    let active = 0;
    for (const view of views) {
      const saved = toSaved(view.spec());
      if (!saved) continue;
      if (view === panel.active()) active = tabs.length;
      tabs.push(saved);
    }
    if (!tabs.length) return {};
    return { tabs: tabs.slice(-MAX_SAVED_TABS), active: Math.min(active, MAX_SAVED_TABS - 1), ...(panel.hidden() ? { hidden: true } : {}) };
  }

  function toSaved(spec: TabSpec): SavedTab | null {
    if (spec.t === "artifact") return { a: spec.callId };
    const target = spec.target;
    if (target.path) return { p: target.path };
    if (target.file) return { f: target.file, n: target.title, k: target.kind === "image" || target.kind === "pdf" ? target.kind : "text" };
    if (target.external) return { u: target.external };
    return null;
  }

  /** Remembers the chat and its tabs with the window. */
  function saveWindow(): void {
    if (!current || current.readOnly) return;
    ctx.saveState({ conversationId: current.conversation.id, ...tabsState() });
  }

  /** Put the panel away (its tabs are kept for the chat). */
  function stowPanel(): void {
    if (current && panel.views().length) tabMemory.set(current.conversation.id, tabsState());
    panel.closeAll();
    syncPanel();
  }

  /** Open a chat's tabs (what the window kept, or an artifact's). */
  function restoreTabs(state: Params): void {
    const wanted = state.tabs ?? (state.artifact ? [{ a: state.artifact }] : []);
    // The same tabs (another device saying what this one did): as they are.
    if (!state.artifact) {
      const asked = wanted.length ? { tabs: wanted, active: state.active ?? 0, ...(state.hidden ? { hidden: true } : {}) } : {};
      if (JSON.stringify(asked) === JSON.stringify(tabsState())) return;
    }
    restoring = true;
    try {
      panel.closeAll();
      for (const saved of wanted) {
        if ("a" in saved) openArtifact(saved.a, { background: true });
        else {
          const target =
            "p" in saved
              ? viewOfPath(saved.p)
              : "u" in saved
                ? viewOfUrl(saved.u)
                : viewOfChatFile({ id: saved.f, name: saved.n, kind: (saved.k === "image" || saved.k === "pdf" ? saved.k : "text") as "image" | "pdf" | "text" });
          if (target) panel.open(createFileView(ctx, target), { background: true });
        }
      }
      const views = panel.views();
      const front = views[Math.min(Math.max(state.active ?? 0, 0), views.length - 1)];
      if (state.artifact) {
        const artifact = current ? chatArtifacts(current.messages).find((a) => a.callId === state.artifact) : undefined;
        if (artifact) panel.activate(artifactKey(artifact));
      } else if (front) panel.activate(front.key);
      panel.setHidden(Boolean(state.hidden));
    } finally {
      restoring = false;
    }
    syncPanel();
  }

  /** A file, picture, document or web page in a tab of the panel. */
  function openView(target: ViewTarget): void {
    panel.open(createFileView(ctx, target));
  }

  // Artifacts: a tab of the panel (its versions share it). A new one opens
  // by itself while the chat is in view; those already there when a chat
  // opens wait for a click.
  let seenArtifacts = new Set<string>();
  function openArtifact(callId: string, options: { background?: boolean } = {}): void {
    if (!current) return;
    const list = chatArtifacts(current.messages);
    const artifact = list.find((a) => a.callId === callId);
    if (!artifact) return;
    const existing = panel.get(artifactKey(artifact)) as ArtifactView | undefined;
    if (existing) {
      existing.update(list, callId);
      if (!options.background) panel.activate(existing.key);
      return;
    }
    const view = createArtifactView(ctx, list, callId, current.conversation.id);
    if (view) panel.open(view, options);
  }
  function noticeArtifacts(): void {
    if (!current) return;
    const list = chatArtifacts(current.messages);
    let fresh: string | null = null;
    for (const artifact of list) {
      if (seenArtifacts.has(artifact.callId)) continue;
      seenArtifacts.add(artifact.callId);
      fresh = artifact.callId;
    }
    for (const view of panel.views()) if (view.key.startsWith("a:")) (view as ArtifactView).update(list);
    if (fresh) openArtifact(fresh);
  }

  // The chat list: a column that hides in a wide window, a drawer in a
  // narrow one (where it is positioned over the chat).
  const SIDE_KEY = "rast.agent.sideHidden";
  if (readLocal(SIDE_KEY) === "1") ctx.root.classList.add("ai-side-hidden");
  const isDrawer = () => getComputedStyle(side).position === "absolute";
  function setDrawer(open: boolean): void {
    ctx.root.classList.toggle("ai-side-open", open);
    if (open) window.setTimeout(() => newButton.focus({ preventScroll: true }), 30);
  }
  function toggleSide(): void {
    if (isDrawer()) {
      setDrawer(!ctx.root.classList.contains("ai-side-open"));
      return;
    }
    const hidden = ctx.root.classList.toggle("ai-side-hidden");
    writeLocal(SIDE_KEY, hidden ? "1" : "0");
  }
  sideToggle.addEventListener("click", toggleSide);
  sideClose.addEventListener("click", () => {
    toggleSide();
    sideToggle.focus({ preventScroll: true });
  });
  scrim.addEventListener("click", () => setDrawer(false));
  ctx.root.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && ctx.root.classList.contains("ai-side-open") && isDrawer()) {
      event.preventDefault();
      event.stopPropagation();
      setDrawer(false);
      sideToggle.focus({ preventScroll: true });
    }
  });

  // New chats start with these (the effort last chosen; asking first).
  const EFFORT_KEY = "rast.agent.effort";
  const storedEffort = (): Effort => {
    const value = readLocal(EFFORT_KEY) ?? "";
    return EFFORTS.some((e) => e.id === value) ? (value as Effort) : "";
  };
  let draftEffort: Effort = storedEffort();
  let draftAllowed: string[] = [];
  guard.addEventListener("click", () => {
    const allowed = current ? current.conversation.allowedTools ?? [] : draftAllowed;
    const mode = permissionMode(allowed);
    const icons: Record<string, string> = { ask: "shield", read: "eye", edit: "edit", act: "network", all: "terminal" };
    contextMenu(
      0,
      0,
      [
        ...PERMISSION_MODES.map((m) => ({
          label: m.label,
          hint: m.id === "all" ? (isAdmin ? "Also runs commands without asking, as root on this server" : "Also runs commands without asking (they cannot change the server)") : m.hint,
          icon: icons[m.id],
          checked: mode?.id === m.id,
          onClick: () => void setPermissions([...m.tools]),
        })),
        ...(mode
          ? []
          : [
              {
                label: "Allowed one by one",
                hint: allowed.map((name) => TOOL_INFO[name]?.label ?? name).join(", "),
                icon: "check",
                checked: true,
                onClick: () => undefined,
              },
            ]),
      ],
      { anchor: guard, ariaLabel: "Permissions" },
    );
  });
  search.addEventListener("input", () => renderList());
  input.addEventListener("input", () => {
    autoGrow();
    updateComposer();
  });
  // A pasted picture or file joins the message (text pastes as text).
  input.addEventListener("paste", (event) => {
    const data = event.clipboardData;
    const files = Array.from(data?.files ?? []);
    if (!files.length || data?.types.includes("text/plain")) return;
    event.preventDefault();
    attachments.add(files);
  });
  // Files dropped anywhere on the chat.
  let dragDepth = 0;
  const carriesFiles = (event: DragEvent) =>
    Boolean(event.dataTransfer?.types.includes("Files")) && !composer.classList.contains("hidden") && settingsHost.classList.contains("hidden");
  main.addEventListener("dragenter", (event) => {
    if (!carriesFiles(event)) return;
    event.preventDefault();
    dragDepth += 1;
    main.classList.add("is-dropping");
  });
  main.addEventListener("dragover", (event) => {
    if (!carriesFiles(event)) return;
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
  });
  main.addEventListener("dragleave", () => {
    dragDepth = Math.max(0, dragDepth - 1);
    if (!dragDepth) main.classList.remove("is-dropping");
  });
  main.addEventListener("drop", (event) => {
    dragDepth = 0;
    main.classList.remove("is-dropping");
    if (!carriesFiles(event)) return;
    event.preventDefault();
    event.stopPropagation();
    attachments.add(Array.from(event.dataTransfer?.files ?? []));
    input.focus();
  });
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      void send();
    }
  });
  sendButton.addEventListener("click", () => {
    if (current?.conversation.running) void stop();
    else void send();
  });
  picker.addEventListener("change", () => {
    drawPickFace();
    updateTitle();
    if (!current) renderThread();
    void choose(picker.value);
  });
  menuButton.addEventListener("click", (event) => {
    if (!current || current.readOnly) return;
    const conversation = current.conversation;
    contextMenu(event.clientX, event.clientY, [
      conversation.pinnedAt
        ? { label: "Unpin", icon: "unpin", onClick: () => void pin(conversation, false) }
        : { label: "Pin to top", icon: "pin", onClick: () => void pin(conversation, true) },
      { label: "Rename", icon: "rename", onClick: () => void rename(conversation) },
      { label: "Copy chat", icon: "copy", onClick: () => void copyChat() },
      "separator",
      { label: "Delete", icon: "trash", danger: true, onClick: () => void remove(conversation) },
    ]);
  });

  function autoGrow(): void {
    input.style.height = "auto";
    input.style.height = `${Math.min(input.scrollHeight, 220)}px`;
  }

  // -------------------------------------------------------------- data
  async function loadOptions(): Promise<void> {
    try {
      options = await ctx.api.get<Options>("/api/ai/options");
    } catch (error) {
      options = null;
      ctx.toast.error(errorMessage(error));
    }
    renderPicker();
    renderAllowance();
    updateComposer();
    if (!current) renderThread();
    void usage.refresh();
  }

  async function loadList(): Promise<void> {
    try {
      const body = await ctx.api.get<{ items: Conversation[] }>("/api/ai/conversations");
      conversations = body.items;
    } catch (error) {
      ctx.toast.error(errorMessage(error));
    }
    renderList();
  }

  async function openConversation(id: string, options_: { quiet?: boolean; place?: Params } = {}): Promise<void> {
    closeSettings();
    const token = ++loadToken;
    try {
      const detail = await ctx.api.get<ConversationDetail>(`/api/ai/conversations/${encodeURIComponent(id)}`);
      if (token !== loadToken || disposed) return;
      const switching = current?.conversation.id !== id;
      if (switching) stowPanel();
      current = detail;
      if (switching) seenArtifacts = new Set(chatArtifacts(detail.messages).map((a) => a.callId));
      renderAll(switching);
      const place = options_.place;
      if (place && (place.tabs || place.artifact)) restoreTabs(place);
      else if (switching) restoreTabs(tabMemory.get(id) ?? {});
      else noticeArtifacts();
      saveWindow();
      if (switching && !detail.readOnly) input.focus();
    } catch (error) {
      if (token !== loadToken) return;
      if (error instanceof ApiError && error.isNotFound) {
        if (!options_.quiet) ctx.toast.warning("That chat no longer exists.");
        openNew();
        return;
      }
      ctx.toast.error(errorMessage(error));
    }
  }

  function openNew(): void {
    closeSettings();
    stowPanel();
    seenArtifacts = new Set();
    loadToken += 1;
    current = null;
    draftAllowed = [];
    draftEffort = storedEffort();
    ctx.saveState({});
    renderAll(true);
    input.focus();
  }

  function openSettings(): void {
    if (!isAdmin) return;
    ctx.saveState({ settings: true });
    main.classList.add("hidden");
    settingsHost.classList.remove("hidden");
    syncPanel();
    ctx.root.classList.remove("ai-side-open");
    if (!settings) {
      settings = mountSettings({
        ctx,
        host: settingsHost,
        onClose: () => {
          closeSettings();
          if (current) saveWindow();
          else ctx.saveState({});
        },
        openConversation: (id) => void openConversation(id),
      });
    } else {
      settings.refresh();
    }
    ctx.setTitle("AI Agent — Settings");
  }

  function closeSettings(): void {
    if (settingsHost.classList.contains("hidden")) return;
    settingsHost.classList.add("hidden");
    main.classList.remove("hidden");
    syncPanel();
    updateTitle();
  }

  // ----------------------------------------------------------- actions
  async function send(): Promise<void> {
    const text = input.value.trim();
    const files = attachments.ready();
    if ((!text && !files.length) || current?.readOnly || current?.conversation.running) return;
    if (attachments.busy()) {
      ctx.toast.warning("Files are still uploading; they send once they are ready.");
      return;
    }
    const choice = picker.value;
    if (!current && !choice) {
      ctx.toast.warning("Pick an agent or a model first.");
      return;
    }
    sendButton.disabled = true;
    try {
      let id = current?.conversation.id;
      if (!id) {
        const created = await ctx.api.post<{ conversation: Conversation }>("/api/ai/conversations", {
          ...choiceBody(choice),
          effort: draftEffort,
          allowedTools: draftAllowed,
        });
        id = created.conversation.id;
        conversations = [created.conversation, ...conversations.filter((c) => c.id !== id)];
        current = { conversation: created.conversation, messages: [], seq: 0, readOnly: false };
        saveWindow();
      }
      const sent = await ctx.api.post<{ conversation: Conversation; message: Message; reply: Message }>(
        `/api/ai/conversations/${encodeURIComponent(id)}/messages`,
        { text, files: files.map((file) => file.id) },
      );
      input.value = "";
      attachments.sent();
      autoGrow();
      if (current?.conversation.id === id) {
        current.conversation = sent.conversation;
        upsertMessage(current.messages, sent.message);
        upsertMessage(current.messages, sent.reply);
      }
      upsertConversation(sent.conversation);
      renderAll(false);
      followLatest();
    } catch (error) {
      ctx.toast.error(errorMessage(error));
      if (error instanceof ApiError && (error.isRateLimited || error.isForbidden)) void loadOptions();
    } finally {
      sendButton.disabled = false;
      updateComposer();
    }
  }

  async function stop(): Promise<void> {
    if (!current) return;
    try {
      await ctx.api.post(`/api/ai/conversations/${encodeURIComponent(current.conversation.id)}/stop`);
    } catch (error) {
      ctx.toast.error(errorMessage(error));
    }
  }

  /** A reply waiting to try again tries now. */
  async function retryNow(): Promise<void> {
    if (!current) return;
    try {
      await ctx.api.post(`/api/ai/conversations/${encodeURIComponent(current.conversation.id)}/retry`);
    } catch (error) {
      ctx.toast.error(errorMessage(error));
    }
  }

  /** The owner's answer to a tool request. */
  async function decide(call: ToolBlock, decision: "allow" | "deny", always = false): Promise<void> {
    if (!current || current.readOnly) return;
    const id = current.conversation.id;
    try {
      const body = await ctx.api.post<{ message: Message; conversation: Conversation }>(
        `/api/ai/conversations/${encodeURIComponent(id)}/tools/${encodeURIComponent(call.id)}`,
        { decision, always },
      );
      if (current?.conversation.id !== id) return;
      upsertMessage(current.messages, body.message);
      current.conversation = { ...current.conversation, ...body.conversation };
      if (body.conversation.running) current.seq = 0;
      upsertConversation(body.conversation);
      refreshMessage(body.message);
      updateComposer();
    } catch (error) {
      ctx.toast.error(errorMessage(error));
      if (error instanceof ApiError && error.isConflict) void openConversation(id, { quiet: true });
      else refreshCurrent();
    }
  }

  /** What runs without asking: this chat's, or the next new chat's. */
  async function setPermissions(tools: string[]): Promise<void> {
    // Commands as root without asking: say so once more.
    const allowed = current ? current.conversation.allowedTools ?? [] : draftAllowed;
    if (isAdmin && tools.includes("run_command") && !allowed.includes("run_command")) {
      const ok = await confirmDialog({
        title: "Allow everything",
        message: "The agent will run commands as root on this server without asking, in this chat. You can still stop a reply, and every command shows in the chat.",
        confirmLabel: "Allow everything",
        danger: true,
        within: ctx.root,
      });
      if (!ok) return;
    }
    if (!current) {
      draftAllowed = tools;
      updateComposer();
      return;
    }
    await patchConversation({ allowedTools: tools });
  }

  /** How hard the model thinks: this chat's, and new chats' from now on. */
  async function setEffort(effort: Effort): Promise<void> {
    writeLocal(EFFORT_KEY, effort);
    draftEffort = effort;
    if (!current) {
      updateComposer();
      return;
    }
    await patchConversation({ effort });
  }

  async function patchConversation(change: { allowedTools?: string[]; effort?: Effort }): Promise<void> {
    if (!current || current.readOnly) return;
    const id = current.conversation.id;
    try {
      const updated = await ctx.api.patch<{ conversation: Conversation }>(`/api/ai/conversations/${encodeURIComponent(id)}`, change);
      upsertConversation(updated.conversation);
      if (current?.conversation.id === id) current.conversation = { ...current.conversation, ...updated.conversation };
      updateComposer();
    } catch (error) {
      ctx.toast.error(errorMessage(error));
    }
  }

  function refreshCurrent(): void {
    const reply = current?.messages[current.messages.length - 1];
    if (reply) refreshMessage(reply);
  }

  async function choose(choice: string): Promise<void> {
    writeChoice(choice);
    if (!current || current.readOnly) return;
    try {
      const body = choiceBody(choice);
      const patch = body.agentId ? { agentId: body.agentId } : { agentId: "", providerId: body.providerId, model: body.model };
      const updated = await ctx.api.patch<{ conversation: Conversation }>(
        `/api/ai/conversations/${encodeURIComponent(current.conversation.id)}`,
        patch,
      );
      current.conversation = updated.conversation;
      upsertConversation(updated.conversation);
    } catch (error) {
      ctx.toast.error(errorMessage(error));
      renderPicker();
    }
  }

  /** Keep a chat at the top of the list (or let it go back to its day). */
  async function pin(conversation: Conversation, pinned: boolean): Promise<void> {
    try {
      const updated = await ctx.api.patch<{ conversation: Conversation }>(
        `/api/ai/conversations/${encodeURIComponent(conversation.id)}`,
        { pinned },
      );
      upsertConversation(updated.conversation);
      if (current?.conversation.id === conversation.id) current.conversation = updated.conversation;
      renderList();
    } catch (error) {
      ctx.toast.error(errorMessage(error));
    }
  }

  async function rename(conversation: Conversation): Promise<void> {
    const title = await promptDialog({
      title: "Rename chat",
      label: "Name",
      value: conversation.title,
      confirmLabel: "Rename",
      session: ctx.dialog("rename", { id: conversation.id }),
    });
    if (title === null) return;
    try {
      const updated = await ctx.api.patch<{ conversation: Conversation }>(
        `/api/ai/conversations/${encodeURIComponent(conversation.id)}`,
        { title: title.trim() },
      );
      upsertConversation(updated.conversation);
      if (current?.conversation.id === conversation.id) current.conversation = updated.conversation;
      renderList();
      updateTitle();
    } catch (error) {
      ctx.toast.error(errorMessage(error));
    }
  }

  async function remove(conversation: Conversation): Promise<void> {
    const ok = await confirmDialog({
      title: "Delete chat",
      message: `Delete "${conversation.title || "New chat"}"? Its messages cannot be brought back.`,
      confirmLabel: "Delete",
      danger: true,
      session: ctx.dialog("delete", { id: conversation.id }),
    });
    if (!ok) return;
    try {
      await ctx.api.del(`/api/ai/conversations/${encodeURIComponent(conversation.id)}`);
      conversations = conversations.filter((c) => c.id !== conversation.id);
      if (current?.conversation.id === conversation.id) openNew();
      else renderList();
    } catch (error) {
      ctx.toast.error(errorMessage(error));
    }
  }

  async function copyChat(): Promise<void> {
    if (!current) return;
    const lines = current.messages.map((m) => {
      const calls = toolBlocks(m).map((call) => `[${TOOL_INFO[call.name]?.label ?? call.name}: ${toolArgument(call)} (${call.status})]`);
      const files = messageFiles(m).map((file) => `[${file.name}]`);
      return `${m.role === "user" ? "You" : "Assistant"}:\n${[...files, messageText(m), ...calls].filter(Boolean).join("\n")}`;
    });
    if (await copyText(lines.join("\n\n"))) ctx.toast.success("Chat copied.");
  }

  ctx.onDialog("rename", (params) => {
    const id = (params as { id?: string } | null)?.id;
    const conversation = conversations.find((c) => c.id === id);
    if (conversation) void rename(conversation);
  });
  ctx.onDialog("delete", (params) => {
    const id = (params as { id?: string } | null)?.id;
    const conversation = conversations.find((c) => c.id === id);
    if (conversation) void remove(conversation);
  });

  function upsertConversation(conversation: Conversation): void {
    if (conversation.userId !== me) return;
    const index = conversations.findIndex((c) => c.id === conversation.id);
    if (index >= 0) conversations[index] = { ...conversations[index], ...conversation };
    else conversations.unshift(conversation);
    conversations.sort((a, b) => b.updatedAt - a.updatedAt);
    renderList();
  }

  // --------------------------------------------------------- rendering
  function renderAll(resetScroll: boolean): void {
    renderList();
    renderPicker();
    renderThread();
    updateTitle();
    updateComposer();
    if (resetScroll) followLatest();
  }

  /** Who answers for a picker value: name, the model behind it, picture. */
  function describe(choice: string): { name: string; detail: string; picture: (size: number) => HTMLElement } | null {
    const body = choiceBody(choice);
    if (body.agentId) {
      const agent = options?.agents.find((a) => a.id === body.agentId);
      const name = agent?.name ?? "Agent";
      return { name, detail: agent?.model ?? "", picture: (size) => agentAvatar(name, size) };
    }
    if (body.providerId) {
      const provider = options?.providers.find((p) => p.id === body.providerId);
      const kind = provider?.kind ?? "custom";
      return { name: body.model ?? "", detail: provider?.name ?? "", picture: (size) => providerMark(kind, size) };
    }
    return null;
  }

  function updateTitle(): void {
    const conversation = current?.conversation;
    titleEl.textContent = conversation ? conversation.title || "New chat" : "New chat";
    const who = describe(conversation ? choiceOf(conversation) : picker.value);
    headAvatar.replaceChildren(who ? who.picture(26) : agentAvatar("AI", 26));
    // The chat's tokens and what they are worth at API prices.
    const totals = current ? chatTotals(current.messages) : { tokens: 0, estimate: 0 };
    const spent = totals.tokens ? `${formatTokens(totals.tokens)} tokens${totals.estimate ? ` · ${formatEstimate(totals.estimate)}` : ""}` : "";
    subEl.textContent = who ? [who.name, who.detail, spent].filter(Boolean).join(" · ") : spent;
    subEl.title = spent ? `This chat so far: ${spent} (estimated at API prices)` : "";
    ownerEl.classList.toggle("hidden", !current?.readOnly);
    ownerEl.textContent = current?.readOnly ? `${conversation?.owner ?? "?"}'s chat · read only` : "";
    menuButton.classList.toggle("hidden", !current || current.readOnly);
    if (settingsHost.classList.contains("hidden")) ctx.setTitle(conversation?.title ? `AI Agent — ${conversation.title}` : "AI Agent");
  }

  function renderList(): void {
    const query = search.value.trim().toLowerCase();
    const items = conversations.filter((c) => !query || c.title.toLowerCase().includes(query));
    list.replaceChildren();
    if (!items.length) {
      list.appendChild(h("p", { class: "ai-list-empty small muted" }, query ? "No chats match." : "No chats yet."));
      return;
    }
    for (const group of groupByDay(items)) {
      list.appendChild(h("div", { class: "ai-list-group" }, group.label));
      for (const item of group.items) {
        const active = current?.conversation.id === item.id;
        const row = h(
          "button",
          {
            class: `ai-list-item${active ? " is-active" : ""}`,
            type: "button",
            title: item.title || "New chat",
            onClick: () => {
              ctx.root.classList.remove("ai-side-open");
              void openConversation(item.id);
            },
            onContextMenu: (event) => {
              event.preventDefault();
              contextMenu(event.clientX, event.clientY, [
                item.pinnedAt
                  ? { label: "Unpin", icon: "unpin", onClick: () => void pin(item, false) }
                  : { label: "Pin to top", icon: "pin", onClick: () => void pin(item, true) },
                { label: "Rename", icon: "rename", onClick: () => void rename(item) },
                { label: "Delete", icon: "trash", danger: true, onClick: () => void remove(item) },
              ]);
            },
          },
          h("span", { class: "ai-list-title" }, item.title || "New chat"),
          item.pinnedAt && !item.running && !item.waiting ? h("span", { class: "ai-list-pin", title: "Pinned", ariaHidden: true }, iconSvg("pin", 12)) : null,
          item.running
            ? h("span", { class: "ai-dot", title: "Writing a reply" })
            : item.waiting
              ? h("span", { class: "ai-dot is-waiting", title: "Waiting for your answer" })
              : null,
        );
        list.appendChild(row);
      }
    }
  }

  function renderPicker(): void {
    picker.replaceChildren();
    const agents = options?.agents ?? [];
    const providers = (options?.providers ?? []).filter((p) => p.models.length);
    const wanted = current ? choiceOf(current.conversation) : readChoice();
    const values: string[] = [];
    if (agents.length) {
      const group = h("optgroup", {});
      group.label = "Agents";
      for (const agent of agents) {
        const value = `agent:${agent.id}`;
        values.push(value);
        group.appendChild(h("option", { value }, agent.name));
      }
      picker.appendChild(group);
    }
    for (const provider of providers) {
      const group = h("optgroup", {});
      group.label = provider.name;
      for (const model of provider.models) {
        const value = `model:${provider.id}|${model}`;
        values.push(value);
        group.appendChild(h("option", { value }, model));
      }
      picker.appendChild(group);
    }
    // A conversation's own choice that is no longer offered stays visible.
    if (wanted && !values.includes(wanted) && current) {
      const label = current.conversation.agentId ? "Agent no longer available" : current.conversation.model || "Unknown model";
      picker.prepend(h("option", { value: wanted }, label));
      values.unshift(wanted);
    }
    picker.value = values.includes(wanted) ? wanted : (values[0] ?? "");
    picker.disabled = !values.length || Boolean(current?.readOnly);
    drawPickFace();
  }

  function drawPickFace(): void {
    modelMenu.update();
    usage.update();
  }

  /** The provider and model that answer in this chat (or would, in a new one). */
  function answering(): { providerId: string | null; model: string } {
    const body = choiceBody(current ? choiceOf(current.conversation) : picker.value);
    if (body.agentId) {
      const agent = options?.agents.find((a) => a.id === body.agentId);
      return { providerId: agent?.providerId ?? null, model: agent?.model ?? "" };
    }
    return { providerId: body.providerId ?? null, model: body.model ?? "" };
  }

  function renderAllowance(): void {
    const allowance = options?.allowance;
    const textValue = allowance ? allowanceText(allowance) : null;
    allowanceEl.classList.toggle("hidden", !textValue);
    if (!allowance || !textValue) return;
    const used = allowanceUsed(allowance) ?? 0;
    allowanceEl.replaceChildren(
      h("div", { class: "ai-allowance-bar" }, h("span", { style: `width:${Math.round(used * 100)}%` })),
      h("span", { class: "small muted" }, textValue),
    );
    allowanceEl.classList.toggle("is-full", used >= 1);
  }

  function updateComposer(): void {
    const running = Boolean(current?.conversation.running);
    const readOnly = Boolean(current?.readOnly);
    composer.classList.toggle("hidden", readOnly || !options?.configured);
    sendButton.replaceChildren(iconSvg(running ? "stop" : "arrowUp", 17));
    sendButton.title = running ? "Stop" : "Send (Enter)";
    sendButton.setAttribute("aria-label", running ? "Stop" : "Send");
    sendButton.classList.toggle("is-stop", running);
    sendButton.disabled = !running && ((!input.value.trim() && !attachments.ready().length) || !picker.value);
    const allowed = current ? current.conversation.allowedTools ?? [] : draftAllowed;
    const mode = permissionMode(allowed);
    guardLabel.textContent = mode?.short ?? "Some allowed";
    guard.classList.toggle("is-on", mode?.id !== "ask");
    modelMenu.update();
    usage.update();
    const allowance = options?.allowance;
    const waiting = !running && current?.messages[current.messages.length - 1]?.status === "waiting";
    input.placeholder = waiting ? "Or write something else" : "Message";
    composerNote.textContent =
      allowance && allowanceUsed(allowance) === 1
        ? "You have used your AI allowance for now."
        : waiting
          ? "The agent is waiting for your answer above. Writing a message instead skips its request."
          : "";
  }

  const bodies = new Map<number, { el: HTMLElement; frame: number }>();
  const allowanceRefresh = debounceTask(() => void loadOptions(), 400);
  const quotaRefresh = debounceTask(() => void usage.refresh(), 1500);

  function renderThread(): void {
    thread.replaceChildren();
    bodies.clear();
    if (!options) return;
    if (!current) {
      thread.appendChild(welcome());
      return;
    }
    if (!current.messages.length) {
      thread.appendChild(welcome());
    }
    for (const message of current.messages) thread.appendChild(messageEl(message));
  }

  function welcome(): HTMLElement {
    if (!options?.configured) {
      return h(
        "div",
        { class: "ai-welcome" },
        h("div", { class: "ai-welcome-icon" }, iconSvg("activity", 28)),
        h("p", { class: "ai-welcome-title" }, "No AI models yet"),
        h(
          "p",
          { class: "small muted" },
          isAdmin ? "Add a model provider to start chatting." : "An administrator has not set up AI models yet.",
        ),
        isAdmin
          ? h("button", { class: "btn btn-primary btn-sm", type: "button", onClick: () => openSettings() }, "Add a provider")
          : null,
      );
    }
    if (!picker.value) {
      return h(
        "div",
        { class: "ai-welcome" },
        h("p", { class: "ai-welcome-title" }, "No models for this account"),
        h("p", { class: "small muted" }, "Ask an administrator to let your account use an AI model."),
      );
    }
    const agents = options.agents;
    const who = describe(picker.value);
    const user = ctx.store.get().user;
    const first = (user?.displayName || user?.username || "").trim().split(/\s+/)[0] ?? "";
    const hour = new Date().getHours();
    const greeting = hour < 5 ? "Working late" : hour < 12 ? "Good morning" : hour < 18 ? "Good afternoon" : "Good evening";
    const agent = agents.find((a) => `agent:${a.id}` === picker.value);
    const suggestions: Array<[string, string, string]> = [
      ["folder", "What's in my home folder?", "List what is in my home folder and tell me what takes the most space."],
      ["edit", "Write a note", "Write a short note to notes.md with a checklist for today."],
      isAdmin
        ? ["terminal", "Check the server", "How is the server doing? Check disk space, memory and uptime."]
        : ["search", "Find a file", "Find the files in my folders whose name mentions \"invoice\"."],
      ["external", "Read a web page", "Summarize this page for me: https://"],
    ];
    const orb = h("div", { class: "ai-orb" }, who ? who.picture(60) : agentAvatar("AI", 60));
    return h(
      "div",
      { class: "ai-welcome" },
      orb,
      h("p", { class: "ai-welcome-title" }, first ? `${greeting}, ${first}` : greeting),
      h(
        "p",
        { class: "ai-welcome-sub muted" },
        agent?.description || (who ? `${who.name} is ready. It asks you before it touches your files, runs a command or opens the web.` : ""),
      ),
      agents.length > 1
        ? h(
            "div",
            { class: "ai-agent-chips" },
            ...agents.map((a) =>
              h(
                "button",
                {
                  class: `ai-chip${picker.value === `agent:${a.id}` ? " is-active" : ""}`,
                  type: "button",
                  title: a.description || a.name,
                  onClick: () => {
                    picker.value = `agent:${a.id}`;
                    writeChoice(picker.value);
                    drawPickFace();
                    updateTitle();
                    renderThread();
                    input.focus();
                  },
                },
                agentAvatar(a.name, 18),
                h("span", {}, a.name),
              ),
            ),
          )
        : null,
      h(
        "div",
        { class: "ai-ideas" },
        ...suggestions.map(([icon, label, prompt]) =>
          h(
            "button",
            {
              class: "ai-idea",
              type: "button",
              onClick: () => {
                input.value = prompt;
                autoGrow();
                updateComposer();
                input.focus();
                input.setSelectionRange(prompt.length, prompt.length);
              },
            },
            h("span", { class: "ai-idea-icon" }, iconSvg(icon, 15)),
            h("span", { class: "ai-idea-label" }, label),
          ),
        ),
      ),
    );
  }

  function messageEl(message: Message): HTMLElement {
    if (message.role === "user") {
      const text = messageText(message);
      return h(
        "div",
        { class: "ai-msg ai-msg-user", dataset: { id: String(message.id) } },
        h("div", { class: "ai-user-stack" }, sentFiles(messageFiles(message)), text ? h("div", { class: "ai-bubble" }, text) : null),
        h(
          "div",
          { class: text ? "ai-msg-tools" : "ai-msg-tools hidden" },
          h(
            "button",
            {
              class: "btn btn-ghost btn-icon btn-sm",
              type: "button",
              title: "Copy",
              ariaLabel: "Copy",
              onClick: () => void copyText(text).then((ok) => ok && ctx.toast.success("Copied.")),
            },
            iconSvg("copy", 13),
          ),
        ),
      );
    }
    const body = h("div", { class: "ai-reply" });
    const who = current ? describe(choiceOf(current.conversation)) : null;
    const name = who?.name || message.model || "Assistant";
    const wrap = h(
      "div",
      { class: "ai-msg ai-msg-assistant", dataset: { id: String(message.id) } },
      h("div", { class: "ai-who" }, who ? who.picture(22) : agentAvatar(name, 22), h("span", { class: "ai-who-name" }, name)),
      body,
    );
    bodies.set(message.id, { el: body, frame: 0 });
    fillReply(body, message);
    return wrap;
  }

  /** Parts of a reply already shown, by block: kept while unchanged, so a
   * streaming reply redraws only what grew (and a selection in an earlier
   * part survives). */
  const shownParts = new WeakMap<HTMLElement, Map<number, { sig: string; el: HTMLElement }>>();

  function fillReply(body: HTMLElement, message: Message): void {
    const text = messageText(message);
    const streaming = message.status === "streaming";
    const before = shownParts.get(body) ?? new Map<number, { sig: string; el: HTMLElement }>();
    const now = new Map<number, { sig: string; el: HTMLElement }>();
    const reuse = (index: number, sig: string, make: () => HTMLElement): HTMLElement => {
      const old = before.get(index);
      const el = old && old.sig === sig ? old.el : make();
      now.set(index, { sig, el });
      return el;
    };
    // Opened sections stay open while the reply changes.
    const opened = new Map<string, boolean>();
    for (const details of body.querySelectorAll<HTMLDetailsElement>("details[data-key]")) {
      opened.set(details.dataset.key ?? "", details.open);
    }
    const parts: HTMLElement[] = [];
    const blocks: ContentBlock[] = message.content;
    const last = blocks.length - 1;
    blocks.forEach((block, index) => {
      const live = streaming && index === last;
      const sig = `${live}|${message.status}|${current?.readOnly ?? false}|${JSON.stringify(block)}`;
      if (block.type === "reasoning") {
        if (!block.text) return;
        const key = `reasoning:${index}`;
        parts.push(
          reuse(index, sig, () => {
            const details = h("details", { class: "ai-thinking", dataset: { key } });
            details.open = opened.get(key) ?? live;
            details.classList.toggle("is-live", live);
            details.append(
              h(
                "summary",
                {},
                iconSvg("sparkle", 13),
                h("span", { class: "ai-thinking-label" }, live ? "Thinking…" : "Thought it through"),
                live ? elapsedSince(message.createdAt) : null,
              ),
              h("div", { class: "ai-thinking-text" }, block.text),
            );
            return details;
          }),
        );
      } else if (block.type === "text") {
        if (!block.text) return;
        parts.push(
          reuse(index, sig, () => {
            const md = h("div", { class: "ai-md" });
            renderMarkdown(md, block.text, {
              onCopy: (code) => void copyText(code).then((ok) => ok && ctx.toast.success("Code copied.")),
            });
            if (live) caretInto(md);
            return md;
          }),
        );
      } else if (block.type === "tool" && block.name === "artifact") {
        parts.push(
          reuse(index, sig, () =>
            artifactCard(block, current ? (chatArtifacts(current.messages).find((a) => a.callId === block.id) ?? null) : null, (callId) => openArtifact(callId)),
          ),
        );
      } else if (block.type === "tool") {
        parts.push(reuse(index, sig, () => toolCard(message, block, opened)));
      }
    });
    // The files it wrote: a card each, to open beside the chat.
    const written = writtenFiles(message);
    if (written.length) parts.push(reuse(-4, `files|${written.join("\n")}`, () => fileCards(written)));
    // Waiting to try again: the try, a countdown and what failed.
    const retry = streaming ? message.retry : null;
    if (retry) {
      parts.push(reuse(-1, `retry|${retry.attempt}|${retry.inMs}|${retry.error}`, () => retryBanner(retry)));
    }
    // While it is written: what it is doing, and for how long.
    const activity = retry ? null : replyActivity(message);
    if (activity) {
      parts.push(
        reuse(-1, `working|${activity}|${message.createdAt}`, () =>
          h(
            "div",
            { class: "ai-working", role: "status" },
            h("span", { class: "ai-working-dots", ariaHidden: true }, h("span", {}), h("span", {}), h("span", {})),
            h("span", { class: "ai-working-label" }, activity),
            elapsedSince(message.createdAt),
          ),
        ),
      );
    }
    if (message.status === "error") {
      const error = message.error || "The reply failed.";
      parts.push(reuse(-2, error, () => h("div", { class: "ai-error", role: "alert" }, iconSvg("warning", 14), h("span", {}, error))));
    }
    if (!streaming && message.status !== "waiting") {
      const meta: string[] = [];
      if (message.model) meta.push(message.model);
      if (message.status === "stopped") meta.push("stopped");
      if (message.inputTokens || message.outputTokens) meta.push(`${formatTokens(message.inputTokens + message.outputTokens)} tokens`);
      if (message.estimatedCost) meta.push(formatEstimate(message.estimatedCost));
      if (message.finishedAt && message.createdAt && message.finishedAt >= message.createdAt) meta.push(formatElapsed(message.finishedAt - message.createdAt));
      parts.push(
        reuse(-3, `${meta.join("|")}|${text}`, () =>
          h(
            "div",
            { class: "ai-msg-tools" },
            text
              ? h(
                  "button",
                  {
                    class: "btn btn-ghost btn-icon btn-sm",
                    type: "button",
                    title: "Copy",
                    ariaLabel: "Copy",
                    onClick: () => void copyText(text).then((ok) => ok && ctx.toast.success("Copied.")),
                  },
                  iconSvg("copy", 13),
                )
              : null,
            h("span", { class: "ai-meta small faint" }, meta.join(" · ")),
          ),
        ),
      );
    }
    shownParts.set(body, now);
    // Only what changed is put in again.
    const same = parts.length === body.children.length && parts.every((part, i) => body.children[i] === part);
    if (!same) body.replaceChildren(...parts);
  }

  function toolCard(message: Message, block: ToolBlock, opened: Map<string, boolean>): HTMLElement {
    const info = TOOL_INFO[block.name];
    const arg = toolArgument(block);
    const waiting = message.status === "waiting" && block.status === "pending";
    const canAnswer = waiting && !current?.readOnly;
    const state: Record<ToolBlock["status"], [string, string]> = {
      pending: ["clock", "Asks first"],
      allowed: ["clock", "Allowed"],
      running: ["refresh", "Running"],
      done: ["check", "Done"],
      error: ["warning", "Failed"],
      denied: ["close", "Not allowed"],
      skipped: ["close", "Not run"],
    };
    const [icon, label] = state[block.status] ?? ["info", block.status];
    const card = h(
      "div",
      { class: `ai-tool is-${block.status}`, dataset: { call: block.id } },
      h(
        "div",
        { class: "ai-tool-head" },
        h("span", { class: "ai-tool-icon" }, iconSvg(info?.icon ?? "apps", 14)),
        h("span", { class: "ai-tool-label" }, info?.label ?? block.name),
        h("span", { class: "ai-tool-arg", title: arg }, arg),
        viewable(block)
          ? h(
              "a",
              {
                class: "btn btn-ghost btn-sm ai-tool-view ai-view-link",
                href: "#",
                title: "Open beside the chat",
                dataset: { viewPath: String((block.input as { path?: unknown }).path) },
              },
              iconSvg("eye", 13),
              h("span", {}, "View"),
            )
          : null,
        h("span", { class: "ai-tool-state small" }, iconSvg(icon, 12), h("span", {}, label)),
      ),
    );
    const input = typeof block.input === "object" && block.input ? block.input : {};
    // What exactly would run or be written.
    if (block.name === "run_command" && (waiting || arg.length > 60 || arg.includes("\n"))) {
      card.appendChild(h("pre", { class: "ai-tool-code" }, arg));
    }
    if (block.name === "write_file" && typeof input.content === "string") {
      const key = `content:${block.id}`;
      const details = h("details", { class: "ai-tool-more", dataset: { key } });
      details.open = opened.get(key) ?? false;
      const size = new TextEncoder().encode(input.content).length;
      details.append(h("summary", {}, `Content · ${size.toLocaleString()} bytes`), h("pre", { class: "ai-tool-code" }, input.content));
      card.appendChild(details);
    }
    if (canAnswer) {
      const buttons: HTMLButtonElement[] = [];
      const act = (decision: "allow" | "deny", always: boolean) => () => {
        for (const button of buttons) button.disabled = true;
        void decide(block, decision, always);
      };
      buttons.push(h("button", { class: "btn btn-primary btn-sm", type: "button", onClick: act("allow", false) }, "Allow"));
      if (maySkipAsking(block.name)) {
        buttons.push(
          h(
            "button",
            { class: "btn btn-sm", type: "button", title: `Run "${info?.label ?? block.name}" without asking for the rest of this chat`, onClick: act("allow", true) },
            "Allow for this chat",
          ),
        );
      }
      buttons.push(h("button", { class: "btn btn-ghost btn-sm", type: "button", onClick: act("deny", false) }, "Deny"));
      card.appendChild(h("div", { class: "ai-tool-actions" }, ...buttons));
    } else if (waiting) {
      card.appendChild(h("p", { class: "ai-tool-note small muted" }, `Waiting for ${current?.conversation.owner ?? "the owner"} to answer.`));
    }
    if (block.status === "error" && block.error) {
      card.appendChild(h("p", { class: "ai-tool-error small" }, block.error));
    }
    if (block.image) {
      // The browser's view after the step; opens full size.
      card.appendChild(
        h(
          "a",
          { class: "ai-tool-shot", href: fileUrl(block.image.id), target: "_blank", rel: "noopener", title: "Open the screenshot" },
          picture(fileUrl(block.image.id), `The page after: ${arg}`),
        ),
      );
    }
    if (block.status === "done" && block.output) {
      const key = `output:${block.id}`;
      const details = h("details", { class: "ai-tool-more", dataset: { key } });
      details.open = opened.get(key) ?? false;
      details.append(h("summary", {}, "Result"), h("pre", { class: "ai-tool-code" }, block.output));
      card.appendChild(details);
    }
    return card;
  }

  /** The typing caret goes at the end of the last line of text. */
  /** A time that counts up from `since` (kept current by `ticker`). */
  function retryBanner(retry: Retry): HTMLElement {
    const until = Date.now() + retry.inMs;
    return h(
      "div",
      { class: "ai-retry", role: "status" },
      h("span", { class: "ai-retry-icon", ariaHidden: true }, iconSvg("refresh", 14)),
      h(
        "span",
        { class: "ai-retry-text" },
        h("span", { class: "ai-retry-label", dataset: { until: String(until), attempt: String(retry.attempt), of: String(retry.of) } }, retryLabel(retry, retry.inMs)),
        h("span", { class: "ai-retry-error", title: retry.error }, retry.error),
      ),
      current && !current.readOnly
        ? h("button", { class: "btn btn-sm ai-retry-now", type: "button", onClick: () => void retryNow() }, "Retry now")
        : null,
    );
  }

  /** The paths a reply's write_file calls wrote, in order, once each. */
  function writtenFiles(message: Message): string[] {
    const paths = new Set<string>();
    for (const block of message.content) {
      if (block.type !== "tool" || block.name !== "write_file" || block.status !== "done") continue;
      const path = (block.input as { path?: unknown } | null)?.path;
      if (typeof path === "string" && viewOfPath(path)) paths.add(path);
    }
    return [...paths];
  }

  /** A card per written file: its type, name and folder; Open shows it in
   * the panel, the arrow offers a download and File Station. */
  function fileCards(paths: string[]): HTMLElement {
    return h(
      "div",
      { class: "ai-file-cards" },
      ...paths.map((path) => {
        const target = viewOfPath(path)!;
        const badge = badgeOf(target);
        const more = h(
          "button",
          { class: "btn btn-ghost btn-icon btn-sm ai-file-card-more", type: "button", title: "More", ariaLabel: `More for ${target.title}` },
          iconSvg("chevronDown", 14),
        );
        more.addEventListener("click", () =>
          contextMenu(
            0,
            0,
            [
              { label: "Open beside the chat", icon: "eye", onClick: () => openView(target) },
              {
                label: "Download",
                icon: "download",
                onClick: () => {
                  const link = h("a", { href: target.download ?? target.src, download: target.title });
                  document.body.appendChild(link);
                  link.click();
                  link.remove();
                },
              },
              { label: "Show in File Station", icon: "folder", onClick: () => ctx.openApp("files", { path: folderOf(path) }) },
            ],
            { anchor: more, ariaLabel: target.title },
          ),
        );
        return h(
          "div",
          { class: "ai-file-card" },
          h(
            "button",
            { class: "ai-file-card-main", type: "button", title: `Open ${path}`, dataset: { viewPath: path } },
            badge.icon
              ? h("span", { class: `ai-badge is-big is-${badge.tone}`, ariaHidden: true }, iconSvg(badge.icon, 15))
              : h("span", { class: `ai-badge is-big is-${badge.tone}`, ariaHidden: true }, badge.text),
            h(
              "span",
              { class: "ai-file-card-text" },
              h("span", { class: "ai-file-card-name" }, target.title),
              h("span", { class: "ai-file-card-sub small muted" }, `${kindLabel(target)} · ${folderOf(path)}`),
            ),
          ),
          h("div", { class: "ai-file-card-actions" }, h("button", { class: "btn btn-sm", type: "button", dataset: { viewPath: path } }, "Open"), more),
        );
      }),
    );
  }

  /** A file a file tool read or wrote in the person's files, once there. */
  function viewable(block: ToolBlock): boolean {
    if (block.status !== "done" || (block.name !== "read_file" && block.name !== "write_file")) return false;
    const path = (block.input as { path?: unknown } | null)?.path;
    return typeof path === "string" && viewOfPath(path) !== null;
  }

  function elapsedSince(since: number): HTMLElement {
    return h("span", { class: "ai-working-time", ariaHidden: true, dataset: { since: String(since) } }, formatElapsed(Date.now() - since));
  }

  function caretInto(md: HTMLElement): void {
    const caret = h("span", { class: "ai-caret", ariaHidden: true });
    let target: Element = md;
    for (;;) {
      const last: Element | null = target.lastElementChild;
      if (!last || !/^(P|LI|UL|OL|H[1-6]|BLOCKQUOTE)$/.test(last.tagName)) break;
      target = last;
    }
    target.appendChild(caret);
  }

  function refreshMessage(message: Message): void {
    const entry = bodies.get(message.id);
    if (!entry) {
      renderThread();
      return;
    }
    if (entry.frame) return;
    entry.frame = window.requestAnimationFrame(() => {
      entry.frame = 0;
      const latest = current?.messages.find((m) => m.id === message.id);
      if (latest) fillReply(entry.el, latest);
    });
  }

  // Following the latest: while the reader is at the end, the chat stays
  // there as replies grow (and as the window or the composer resizes). A
  // scroll the reader makes upwards stops it; the button, or scrolling back
  // down to the end, starts it again.
  let following = true;
  let readerAt = 0;
  const farFromEnd = (): number => scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
  function toEnd(): void {
    scroller.scrollTop = scroller.scrollHeight;
  }
  function followLatest(): void {
    following = true;
    toEnd();
    updateJump();
  }
  function updateJump(): void {
    const away = !following && farFromEnd() > 48;
    jumpButton.classList.toggle("is-shown", away);
    if (!away) jumpButton.classList.remove("has-new");
  }
  for (const type of ["wheel", "touchmove", "pointerdown", "keydown"]) {
    scroller.addEventListener(type, () => (readerAt = performance.now()), { passive: true });
  }
  scroller.addEventListener(
    "scroll",
    () => {
      // Only the reader's own scrolling decides (not the chat growing or a
      // re-render); reaching the end always follows again.
      const far = farFromEnd();
      if (far <= 4) following = true;
      else if (performance.now() - readerAt < 1000) following = far <= 48;
      updateJump();
    },
    { passive: true },
  );
  jumpButton.addEventListener("click", () => {
    following = true;
    scroller.scrollTo({ top: scroller.scrollHeight, behavior: "smooth" });
    updateJump();
  });
  let threadHeight = 0;
  const followObserver = new ResizeObserver(() => {
    const grew = thread.offsetHeight > threadHeight;
    threadHeight = thread.offsetHeight;
    if (following) toEnd();
    else if (grew && current?.conversation.running) jumpButton.classList.add("has-new");
    updateJump();
  });
  followObserver.observe(thread);
  followObserver.observe(scroller);

  // Links, paths and files in the chat open in the viewer (a click with a
  // modifier key keeps the browser's way).
  thread.addEventListener("click", (event) => {
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    const el = (event.target as Element | null)?.closest<HTMLElement>("[data-view-url],[data-view-path],[data-view-file]");
    if (!el || !thread.contains(el)) return;
    const data = el.dataset;
    const target = data.viewUrl
      ? viewOfLink(data.viewUrl)
      : data.viewPath
        ? viewOfPath(data.viewPath)
        : viewOfChatFile({ id: data.viewFile ?? "", name: data.viewName ?? "", kind: (data.viewKind ?? "text") as "image" | "pdf" | "text" });
    if (!target) return;
    event.preventDefault();
    openView(target);
  });

  // ---------------------------------------------------------- realtime
  unsubscribers.push(
    realtime.on<DeltaEvent>("ai.delta", (delta) => {
      if (!current || current.conversation.id !== delta.conversationId) return;
      const result = applyDelta(current, delta);
      if (result === "stale") void openConversation(current.conversation.id, { quiet: true });
      else if (result === "applied") {
        const message = current.messages.find((m) => m.id === delta.messageId);
        if (message) refreshMessage(message);
      }
    }),
    realtime.on<MessageEvent>("ai.message", (event) => {
      const listed = conversations.find((c) => c.id === event.conversationId);
      const waiting = event.message.role === "assistant" ? event.message.status === "waiting" : listed?.waiting;
      if (listed && (listed.running !== event.running || listed.waiting !== waiting)) {
        listed.running = event.running;
        listed.waiting = Boolean(waiting);
        renderList();
      }
      if (listed && !event.running && event.message.role === "assistant" && options?.allowance && allowanceText(options.allowance)) {
        allowanceRefresh.run();
      }
      // A reply that ended reported its provider's limits.
      if (!event.running && event.message.role === "assistant") quotaRefresh.run();
      if (!current || current.conversation.id !== event.conversationId) return;
      const known = current.messages.some((m) => m.id === event.message.id);
      upsertMessage(current.messages, event.message);
      current.conversation.running = event.running;
      if (!event.running) current.seq = 0;
      else if (typeof event.seq === "number" && event.message.status === "streaming") current.seq = event.seq;
      if (event.message.role === "assistant") current.conversation.waiting = event.message.status === "waiting";
      if (known) refreshMessage(event.message);
      else renderThread();
      noticeArtifacts();
      updateComposer();
    }),
    realtime.on<ConversationEvent>("ai.conversation", (event) => {
      if (event.deleted) {
        conversations = conversations.filter((c) => c.id !== event.conversation.id);
        renderList();
        if (current?.conversation.id === event.conversation.id) openNew();
        return;
      }
      upsertConversation(event.conversation);
      if (current?.conversation.id === event.conversation.id) {
        const running = current.conversation.running;
        current.conversation = { ...current.conversation, ...event.conversation, running: running || event.conversation.running };
        renderPicker();
        updateTitle();
        updateComposer();
      }
    }),
    realtime.on("ai.config", () => {
      void loadOptions();
      settings?.refresh();
    }),
    realtime.onResync(() => {
      void loadOptions();
      void loadList();
      if (current) void openConversation(current.conversation.id, { quiet: true });
    }),
  );

  ctx.onPlace((value) => {
    const params = asParams(value);
    if (params.settings) openSettings();
    else if (params.conversationId) void openConversation(params.conversationId, { quiet: true, place: params });
    else openNew();
  });
  ctx.onReopen((value) => {
    const params = asParams(value);
    if (params.settings) openSettings();
    else if (params.conversationId) void openConversation(params.conversationId, { place: params });
  });
  // Typing while the window is in front writes in the message box, as in
  // chat apps: the first key moves the cursor there and the rest follows.
  const typeToCompose = (event: KeyboardEvent) => {
    if (event.defaultPrevented || event.isComposing || event.ctrlKey || event.metaKey || event.altKey) return;
    // Printable keys only; a space may be pressing a focused button.
    if (event.key.length !== 1 || event.key === " ") return;
    const win = ctx.root.closest(".window");
    if (!win?.classList.contains("is-focused")) return;
    const active = document.activeElement;
    if (active && active !== document.body && !win.contains(active)) return;
    if (active instanceof HTMLElement && active.closest("input, textarea, select, [contenteditable]")) return;
    if (dialogOpen(win)) return;
    if (!settingsHost.classList.contains("hidden") || composer.classList.contains("hidden") || input.disabled) return;
    event.preventDefault();
    input.focus();
    input.setRangeText(event.key, input.selectionStart, input.selectionEnd, "end");
    input.dispatchEvent(new Event("input", { bubbles: true }));
  };
  document.addEventListener("keydown", typeToCompose);

  // Counting times of replies being written, and down to tries again, once a second.
  const ticker = window.setInterval(() => {
    for (const el of ctx.root.querySelectorAll<HTMLElement>(".ai-working-time[data-since]")) {
      el.textContent = formatElapsed(Date.now() - Number(el.dataset.since));
    }
    for (const el of ctx.root.querySelectorAll<HTMLElement>(".ai-retry-label[data-until]")) {
      const retry = { attempt: Number(el.dataset.attempt), of: Number(el.dataset.of) };
      el.textContent = retryLabel(retry, Number(el.dataset.until) - Date.now());
    }
  }, 1000);
  ctx.onDispose(() => {
    disposed = true;
    window.clearInterval(ticker);
    document.removeEventListener("keydown", typeToCompose);
    attachments.clear();
    followObserver.disconnect();
    for (const unsubscribe of unsubscribers.splice(0)) unsubscribe();
    for (const entry of bodies.values()) if (entry.frame) window.cancelAnimationFrame(entry.frame);
    allowanceRefresh.cancel();
    quotaRefresh.cancel();
    usage.dispose();
    modelMenu.close();
    updater.destroy();
    settings?.destroy();
  });

  // ------------------------------------------------------------- start
  const start = asParams(ctx.params);
  renderAll(false);
  void (async () => {
    await Promise.all([loadOptions(), loadList()]);
    if (disposed) return;
    const loaded = options as Options | null;
    showWhatsNew(ctx, loaded?.version, loaded?.whatsNewSeen);
    if (start.settings && isAdmin) openSettings();
    else if (start.conversationId) await openConversation(start.conversationId, { quiet: true, place: start });
    else renderThread();
  })();
  return {};
}
