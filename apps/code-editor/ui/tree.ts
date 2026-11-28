// The folder on the left: its files and folders, opened one level at a time.

import type { AppContext } from "../../core/app";
import { h } from "../../core/dom";
import { iconSvg } from "../../core/icons";
import type { FileEntry, FileListing } from "../../core/types";
import { errorMessage } from "../../core/ui";
import { baseName, isInside, parentOf } from "./paths";

export interface TreeOptions {
  onOpen(path: string): void;
  onMenu(entry: TreeEntry | null, x: number, y: number): void;
}

export interface TreeEntry {
  name: string;
  path: string;
  kind: "dir" | "file";
}

export interface Tree {
  el: HTMLElement;
  root(): string;
  setRoot(root: string): Promise<void>;
  /** Load a folder's entries again (it changed). */
  refresh(dir?: string): Promise<void>;
  /** Open the folders down to `path` and mark it. */
  reveal(path: string): Promise<void>;
  setActive(path: string | null): void;
  /** Folders whose listings are shown (to know which changes matter). */
  shows(dir: string): boolean;
  focus(): void;
}

const LIMIT = 2000;

export function createTree(ctx: AppContext, options: TreeOptions): Tree {
  let rootPath = "/";
  let active: string | null = null;
  const expanded = new Set<string>(["/"]);
  const listings = new Map<string, TreeEntry[] | { error: string }>();
  const loading = new Set<string>();
  const el = h("div", { class: "ce-tree", role: "tree", ariaLabel: "Files", tabIndex: 0 });

  async function load(dir: string): Promise<void> {
    if (loading.has(dir)) return;
    loading.add(dir);
    try {
      const listing = await ctx.api.get<FileListing>("/api/files", { path: dir, limit: LIMIT });
      const items: TreeEntry[] = listing.items
        .filter((e: FileEntry) => e.kind === "dir" || e.kind === "file")
        .map((e: FileEntry) => ({ name: e.name, path: e.path, kind: e.kind as "dir" | "file" }))
        .sort((a, b) => (a.kind === b.kind ? a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" }) : a.kind === "dir" ? -1 : 1));
      listings.set(dir, items);
    } catch (error) {
      listings.set(dir, { error: errorMessage(error) });
    } finally {
      loading.delete(dir);
    }
    render();
  }

  function row(entry: TreeEntry, depth: number): HTMLElement {
    const open = entry.kind === "dir" && expanded.has(entry.path);
    const button = h(
      "div",
      {
        class: `ce-row${entry.path === active ? " is-active" : ""}`,
        role: "treeitem",
        tabIndex: -1,
        title: entry.path,
        dataset: { path: entry.path, kind: entry.kind },
      },
      h("span", { class: "ce-row-indent", style: `width:${depth * 14}px` }),
      entry.kind === "dir" ? h("span", { class: `ce-row-chevron${open ? " is-open" : ""}` }, iconSvg("chevronRight", 12)) : h("span", { class: "ce-row-chevron" }),
      iconSvg(entry.kind === "dir" ? "folder" : "file", 14),
      h("span", { class: "ce-row-name" }, entry.name),
    );
    button.setAttribute("aria-level", String(depth + 1));
    if (entry.kind === "dir") button.setAttribute("aria-expanded", String(open));
    if (entry.path === active) button.setAttribute("aria-selected", "true");
    return button;
  }

  function render(): void {
    const focused = (document.activeElement as HTMLElement | null)?.dataset?.path;
    const rows: HTMLElement[] = [];
    const walk = (dir: string, depth: number) => {
      const listing = listings.get(dir);
      if (!listing) {
        if (!loading.has(dir)) void load(dir);
        rows.push(h("div", { class: "ce-row is-note", style: `padding-left:${depth * 14 + 26}px` }, "Loading…"));
        return;
      }
      if ("error" in listing) {
        rows.push(h("div", { class: "ce-row is-note is-error", style: `padding-left:${depth * 14 + 26}px`, title: listing.error }, listing.error));
        return;
      }
      if (!listing.length && depth === 0) {
        rows.push(h("div", { class: "ce-row is-note", style: "padding-left:12px" }, "This folder is empty."));
      }
      for (const entry of listing) {
        rows.push(row(entry, depth));
        if (entry.kind === "dir" && expanded.has(entry.path)) walk(entry.path, depth + 1);
      }
      if (listing.length >= LIMIT) rows.push(h("div", { class: "ce-row is-note", style: `padding-left:${depth * 14 + 26}px` }, `Showing the first ${LIMIT}.`));
    };
    walk(rootPath, 0);
    el.replaceChildren(...rows);
    if (focused) el.querySelector<HTMLElement>(`.ce-row[data-path="${CSS.escape(focused)}"]`)?.focus({ preventScroll: true });
  }

  function entryOf(target: EventTarget | null): TreeEntry | null {
    const rowEl = (target as HTMLElement | null)?.closest<HTMLElement>(".ce-row[data-path]");
    if (!rowEl) return null;
    const path = rowEl.dataset.path ?? "";
    return { path, name: baseName(path), kind: rowEl.dataset.kind === "dir" ? "dir" : "file" };
  }

  function toggle(path: string, open?: boolean): void {
    const next = open ?? !expanded.has(path);
    if (next) expanded.add(path);
    else expanded.delete(path);
    render();
  }

  function activate(entry: TreeEntry): void {
    if (entry.kind === "dir") toggle(entry.path);
    else options.onOpen(entry.path);
  }

  el.addEventListener("click", (event) => {
    const entry = entryOf(event.target);
    if (!entry) return;
    (event.target as HTMLElement).closest<HTMLElement>(".ce-row")?.focus({ preventScroll: true });
    activate(entry);
  });
  el.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    options.onMenu(entryOf(event.target), event.clientX, event.clientY);
  });
  el.addEventListener("keydown", (event) => {
    const rows = Array.from(el.querySelectorAll<HTMLElement>(".ce-row[data-path]"));
    const current = document.activeElement as HTMLElement | null;
    const at = current ? rows.indexOf(current) : -1;
    const entry = entryOf(current);
    const go = (index: number) => rows[Math.max(0, Math.min(rows.length - 1, index))]?.focus();
    switch (event.key) {
      case "ArrowDown":
        go(at + 1);
        break;
      case "ArrowUp":
        go(at < 0 ? 0 : at - 1);
        break;
      case "ArrowRight":
        if (entry?.kind === "dir" && !expanded.has(entry.path)) toggle(entry.path, true);
        else go(at + 1);
        break;
      case "ArrowLeft":
        if (entry?.kind === "dir" && expanded.has(entry.path)) toggle(entry.path, false);
        else if (entry) el.querySelector<HTMLElement>(`.ce-row[data-path="${CSS.escape(parentOf(entry.path))}"]`)?.focus();
        break;
      case "Enter":
        if (entry) activate(entry);
        break;
      case "ContextMenu": {
        const rect = current?.getBoundingClientRect();
        if (rect) options.onMenu(entry, rect.left + 24, rect.bottom);
        break;
      }
      default:
        return;
    }
    event.preventDefault();
  });

  return {
    el,
    root: () => rootPath,
    async setRoot(root: string) {
      rootPath = root;
      listings.clear();
      expanded.clear();
      expanded.add(root);
      render();
    },
    async refresh(dir?: string) {
      if (dir === undefined) {
        const open = [...listings.keys()];
        listings.clear();
        await Promise.all(open.filter((d) => d === rootPath || expanded.has(d)).map((d) => load(d)));
        render();
        return;
      }
      if (listings.has(dir)) await load(dir);
    },
    async reveal(path: string) {
      if (!isInside(path, rootPath)) return;
      const chain: string[] = [];
      for (let dir = parentOf(path); dir !== rootPath && isInside(dir, rootPath); dir = parentOf(dir)) chain.unshift(dir);
      for (const dir of chain) {
        expanded.add(dir);
        if (!listings.has(dir)) await load(dir);
      }
      active = path;
      render();
      el.querySelector<HTMLElement>(`.ce-row[data-path="${CSS.escape(path)}"]`)?.scrollIntoView({ block: "nearest" });
    },
    setActive(path: string | null) {
      active = path;
      for (const rowEl of el.querySelectorAll<HTMLElement>(".ce-row[data-path]")) {
        const on = rowEl.dataset.path === path;
        rowEl.classList.toggle("is-active", on);
        if (on) rowEl.setAttribute("aria-selected", "true");
        else rowEl.removeAttribute("aria-selected");
      }
    },
    shows: (dir: string) => listings.has(dir),
    focus() {
      (el.querySelector<HTMLElement>(".ce-row.is-active") ?? el.querySelector<HTMLElement>(".ce-row[data-path]") ?? el).focus();
    },
  };
}
