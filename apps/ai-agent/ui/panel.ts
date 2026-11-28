// The panel beside the chat: tabs of what the chat showed or linked to
// (artifacts, files, web pages), like a small browser. Each tab is a view
// (artifact.ts, viewer.ts) with its own body, kept while the tab is open,
// so a video keeps playing and a page keeps its place between tabs. A tab
// strip with + (open a path or address), Fill and Hide; under it the path
// bar: where the tab's thing is, its own controls and its actions.

import type { AppContext } from "../../core/app";
import { h } from "../../core/dom";
import { iconSvg } from "../../core/icons";
import type { Badge, ViewTarget } from "./view";

/** What reopens a tab (kept with the window). */
export type TabSpec = { t: "artifact"; callId: string } | { t: "view"; target: ViewTarget };

export interface TabView {
  /** The same key is the same tab: opening it again shows that tab. */
  key: string;
  title: string;
  badge: Badge;
  /** An icon address for the badge (a site's), loaded as a picture. */
  badgeImage?: string;
  /** Where it is (a path or address), for the path bar. */
  where: string;
  /** What it is (Markdown, PDF, …), beside the path. */
  label: string;
  /** Its own controls for the path bar (modes, versions). */
  tools: HTMLElement | null;
  /** Its actions (copy, download, open, show in File Station). */
  actions: HTMLElement[];
  body: HTMLElement;
  spec(): TabSpec;
  /** Shown for the first time, or again. */
  shown?(): void;
  refresh?(): void;
  dispose(): void;
  /** Set by the panel: call when title, badge, where or label change. */
  changed?: () => void;
}

export interface SidePanel {
  el: HTMLElement;
  /** Open `view` in a tab (or show the tab that has its key). */
  open(view: TabView, options?: { background?: boolean }): TabView;
  get(key: string): TabView | undefined;
  activate(key: string): void;
  close(key: string): void;
  closeAll(): void;
  active(): TabView | null;
  views(): TabView[];
  /** The panel is open (it has tabs and is not hidden). */
  isOpen(): boolean;
  hidden(): boolean;
  setHidden(hidden: boolean): void;
}

export interface PanelHooks {
  /** Open what the + field names (a path or an address); false: unknown. */
  onAdd(text: string): boolean;
  /** Tabs, the active one or the panel's state changed. */
  onChange(): void;
}

export function createSidePanel(ctx: AppContext, hooks: PanelHooks): SidePanel {
  const views: TabView[] = [];
  const buttons = new Map<string, HTMLElement>();
  let activeKey: string | null = null;
  let isHidden = false;

  const iconButton = (icon: string, label: string, onClick: () => void) =>
    h("button", { class: "btn btn-ghost btn-icon btn-sm", type: "button", title: label, ariaLabel: label, onClick }, iconSvg(icon, 15));

  const strip = h("div", { class: "ai-tabs", role: "tablist", ariaLabel: "Open in the panel" });
  const addField = h("input", {
    class: "input ai-tab-add-field",
    type: "text",
    placeholder: "A path in your files (/…) or a web address",
    ariaLabel: "Open a path or address",
    spellcheck: false,
    autocomplete: "off",
  }) as HTMLInputElement;
  const addRow = h("form", { class: "ai-tab-add hidden" }, addField);
  addRow.addEventListener("submit", (event) => {
    event.preventDefault();
    const text = addField.value.trim();
    if (!text) return;
    if (hooks.onAdd(text)) {
      addField.value = "";
      addRow.classList.add("hidden");
    } else ctx.toast.error("Write a path in your files, like /reports/q3.pdf, or a web address.");
  });
  addField.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      addRow.classList.add("hidden");
    }
  });
  const add = iconButton("plus", "Open a file or address", () => {
    addRow.classList.toggle("hidden");
    if (!addRow.classList.contains("hidden")) addField.focus();
  });
  const wide = iconButton("fullscreen", "Fill the window", () => setWide(!ctx.root.classList.contains("is-panel-wide")));
  function setWide(on: boolean): void {
    ctx.root.classList.toggle("is-panel-wide", on);
    wide.title = on ? "Beside the chat" : "Fill the window";
    wide.setAttribute("aria-label", wide.title);
    wide.setAttribute("aria-pressed", String(on));
  }
  const hide = iconButton("close", "Hide the panel", () => setHidden(true));
  const bar = h("div", { class: "ai-tabbar" }, strip, h("div", { class: "ai-tabbar-tools" }, add, wide, hide));

  const where = h("div", { class: "ai-panel-where small" });
  const label = h("span", { class: "ai-panel-label small muted" });
  const tools = h("div", { class: "ai-panel-tools" });
  const actions = h("div", { class: "ai-panel-actions" });
  const refresh = iconButton("refresh", "Reload", () => active()?.refresh?.());
  const pathbar = h("div", { class: "ai-pathbar" }, where, label, tools, h("div", { class: "ai-panel-actions" }, refresh), actions);
  const bodies = h("div", { class: "ai-panel-bodies" });
  const el = h("aside", { class: "ai-artifact ai-panel", ariaLabel: "Panel" }, bar, addRow, pathbar, bodies);
  el.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && views.length && !(event.target as Element | null)?.closest("input, textarea")) {
      event.preventDefault();
      event.stopPropagation();
      setHidden(true);
    }
  });

  function badgeEl(view: TabView): HTMLElement {
    const badge = view.badge;
    const el = h("span", { class: `ai-badge is-${badge.tone}`, ariaHidden: true });
    if (view.badgeImage) {
      const img = document.createElement("img");
      img.alt = "";
      img.decoding = "async";
      img.addEventListener("error", () => {
        img.remove();
        el.classList.remove("has-image");
        el.append(iconSvg(badge.icon ?? "browser", 12));
      });
      img.src = view.badgeImage;
      el.classList.add("has-image");
      el.append(img);
    } else if (badge.icon) el.append(iconSvg(badge.icon, 12));
    else el.textContent = badge.text;
    return el;
  }

  function tabEl(view: TabView): HTMLElement {
    const close = h(
      "button",
      { class: "ai-tab-close", type: "button", title: "Close tab", ariaLabel: `Close ${view.title}`, tabIndex: -1 },
      iconSvg("close", 12),
    );
    close.addEventListener("click", (event) => {
      event.stopPropagation();
      closeTab(view.key);
    });
    const tab = h(
      "div",
      { class: "ai-tab", role: "tab", tabIndex: 0, title: view.where || view.title, dataset: { key: view.key } },
      badgeEl(view),
      h("span", { class: "ai-tab-title" }, view.title),
      close,
    );
    tab.addEventListener("click", () => activate(view.key));
    tab.addEventListener("auxclick", (event) => {
      if (event.button === 1) closeTab(view.key);
    });
    tab.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        activate(view.key);
      } else if (event.key === "Delete") closeTab(view.key);
    });
    return tab;
  }

  function redrawTab(view: TabView): void {
    const old = buttons.get(view.key);
    const fresh = tabEl(view);
    buttons.set(view.key, fresh);
    if (old) old.replaceWith(fresh);
    else strip.append(fresh);
    fresh.classList.toggle("is-active", view.key === activeKey);
    fresh.setAttribute("aria-selected", String(view.key === activeKey));
    if (view.key === activeKey) drawBar(view);
  }

  function drawBar(view: TabView): void {
    // The start of a long path gives way (an ellipsis), its end stays.
    const text = h("bdi", {}, view.where);
    text.dir = "ltr";
    where.replaceChildren(text);
    where.title = view.where;
    label.textContent = view.label;
    tools.replaceChildren(...(view.tools ? [view.tools] : []));
    actions.replaceChildren(...view.actions);
    refresh.classList.toggle("hidden", !view.refresh);
  }

  function active(): TabView | null {
    return views.find((v) => v.key === activeKey) ?? null;
  }

  function activate(key: string): void {
    const view = views.find((v) => v.key === key);
    if (!view) return;
    activeKey = key;
    for (const [k, button] of buttons) {
      button.classList.toggle("is-active", k === key);
      button.setAttribute("aria-selected", String(k === key));
    }
    for (const v of views) v.body.classList.toggle("hidden", v.key !== key);
    drawBar(view);
    buttons.get(key)?.scrollIntoView({ block: "nearest", inline: "nearest" });
    view.shown?.();
    if (isHidden) setHidden(false, false);
    hooks.onChange();
  }

  function closeTab(key: string): void {
    const at = views.findIndex((v) => v.key === key);
    if (at < 0) return;
    const view = views.splice(at, 1)[0]!;
    view.dispose();
    view.body.remove();
    buttons.get(key)?.remove();
    buttons.delete(key);
    if (activeKey === key) {
      const next = views[at] ?? views[at - 1];
      activeKey = null;
      if (next) {
        activate(next.key);
        return;
      }
      setWide(false);
    }
    hooks.onChange();
  }

  function setHidden(next: boolean, notify = true): void {
    isHidden = next;
    if (next) setWide(false);
    if (notify) hooks.onChange();
  }

  return {
    el,
    open(view, options = {}) {
      const existing = views.find((v) => v.key === view.key);
      if (existing) {
        view.dispose();
        if (!options.background) activate(existing.key);
        return existing;
      }
      view.changed = () => redrawTab(view);
      views.push(view);
      view.body.classList.add("ai-panel-body", "hidden");
      bodies.append(view.body);
      redrawTab(view);
      if (!options.background || !activeKey) activate(view.key);
      else hooks.onChange();
      return view;
    },
    get: (key) => views.find((v) => v.key === key),
    activate,
    close: closeTab,
    closeAll() {
      for (const view of views.splice(0)) {
        view.dispose();
        view.body.remove();
      }
      buttons.clear();
      strip.replaceChildren();
      activeKey = null;
      isHidden = false;
      addRow.classList.add("hidden");
      setWide(false);
    },
    active,
    views: () => [...views],
    isOpen: () => views.length > 0 && !isHidden,
    hidden: () => isHidden,
    setHidden,
  };
}
