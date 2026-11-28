// Code Editor: a folder of the person's RAST files on the left, its files in
// tabs, CodeMirror to write in. Files are read and written with RAST's files
// API, so what AI Agent or anyone else changes shows here as it happens
// (`file.changed`), and unsaved changes stay in the browser until saved.

import { EditorState, type Extension } from "@codemirror/state";
import { gotoLine, openSearchPanel } from "@codemirror/search";
import { EditorView, type KeyBinding, type ViewUpdate } from "@codemirror/view";
import { indentLess, indentMore, toggleComment } from "@codemirror/commands";
import type { AppContext, AppInstance } from "../../core/app";
import { h } from "../../core/dom";
import { relativeTime } from "../../core/format";
import { iconSvg } from "../../core/icons";
import { getRealtime } from "../../core/realtime";
import type { FileEntry, FileRoot } from "../../core/types";
import { confirmDialog, contextMenu, errorMessage, modal, promptDialog, type ContextMenuEntry } from "../../core/ui";
import { dropDraft, moveDraft, readDraft, writeDraft } from "./drafts";
import { fileExtensions, indentExtension, indentSlot, languageSlot, wrapSlot } from "./editor";
import { languageExtension } from "./languages";
import { baseName, detectIndent, isInside, joinPath, languageChoices, languageOf, lineEnding, parentOf, relativeTo, validName, type LanguageId } from "./paths";
import { createTree, type TreeEntry } from "./tree";

/** The largest file opened (RAST's text reads stop at 4 MiB). */
const MAX_BYTES = 4 * 1024 * 1024;
const WRAP_KEY = "rast.code.wrap";

interface Tab {
  path: string;
  state: EditorState;
  /** The text as last read or saved (lines joined with "\n"). */
  saved: string;
  modifiedAt: number;
  language: LanguageId;
  tabs: boolean;
  size: number;
  crlf: boolean;
  /** The line endings the file has on disk. */
  savedCrlf: boolean;
  readOnly: boolean;
  readOnlyWhy: string;
  dirty: boolean;
  notice: Notice | null;
  button: HTMLElement;
}

interface Notice {
  kind: "changed" | "draft" | "gone" | "info";
  text: string;
  actions: Array<{ label: string; primary?: boolean; run: () => void }>;
}

/** What the window keeps across reloads (`ctx.saveState`) and accepts as `params`. */
interface Place {
  root?: string;
  tabs?: string[];
  active?: string | null;
  /** Open this file (from File Station, AI Agent, …), at this line. */
  path?: string;
  line?: number;
}

interface FileChanged {
  op?: string;
  path?: string;
  paths?: string[];
}

export function mountCodeEditor(ctx: AppContext): AppInstance {
  const owner = String(ctx.store.get().user?.id ?? "0");
  const realtime = getRealtime(ctx.events);
  const tabs: Tab[] = [];
  let active: Tab | null = null;
  let wrap = localStorage.getItem(WRAP_KEY) === "1";
  let disposed = false;
  let sideHidden = false;

  ctx.root.classList.add("ce");

  // ---- layout
  const rootButton = h("button", { class: "ce-root", type: "button", title: "Folder" }, iconSvg("folder", 15), h("span", { class: "ce-root-name" }, "Home"), iconSvg("chevronDown", 13));
  const newFileButton = h("button", { class: "ce-icon-btn", type: "button", title: "New file", ariaLabel: "New file" }, iconSvg("compose", 15));
  const newFolderButton = h("button", { class: "ce-icon-btn", type: "button", title: "New folder", ariaLabel: "New folder" }, iconSvg("plus", 15));
  const refreshButton = h("button", { class: "ce-icon-btn", type: "button", title: "Refresh", ariaLabel: "Refresh" }, iconSvg("refresh", 15));
  const tree = createTree(ctx, {
    onOpen: (path) => void openFile(path),
    onMenu: (entry, x, y) => treeMenu(entry, x, y),
  });
  const side = h("aside", { class: "ce-side" }, h("div", { class: "ce-side-head" }, rootButton, newFileButton, newFolderButton, refreshButton), tree.el);
  const sideToggle = h("button", { class: "ce-icon-btn ce-side-toggle", type: "button", title: "Show or hide the folder", ariaLabel: "Show or hide the folder" }, iconSvg("menu", 15));
  const tabStrip = h("div", { class: "ce-tabs", role: "tablist", ariaLabel: "Open files" });
  const moreButton = h("button", { class: "ce-icon-btn", type: "button", title: "More", ariaLabel: "More" }, iconSvg("more", 15));
  const saveButton = h("button", { class: "ce-save", type: "button", title: "Save (Ctrl+S)" }, iconSvg("check", 14), h("span", {}, "Save"));
  const head = h("div", { class: "ce-head" }, sideToggle, tabStrip, saveButton, moreButton);
  const noticeEl = h("div", { class: "ce-notice hidden", role: "status" });
  const host = h("div", { class: "ce-host" });
  const quickOpenButton = h("button", { class: "btn btn-sm", type: "button" }, iconSvg("search", 14), "Find a file");
  const empty = h(
    "div",
    { class: "ce-empty" },
    h("div", { class: "ce-empty-mark" }, iconSvg("compose", 26)),
    h("h2", {}, "Open a file"),
    h("p", { class: "muted" }, "Choose one in the folder on the left, or find it by name."),
    quickOpenButton,
    h("p", { class: "ce-keys small muted" }, "Ctrl+P find a file · Ctrl+S save · Ctrl+F search"),
  );
  const statusPath = h("span", { class: "ce-status-path" });
  const statusPos = h("button", { class: "ce-status-item", type: "button", title: "Go to line (Ctrl+G)" });
  const statusIndent = h("button", { class: "ce-status-item", type: "button", title: "Indentation" });
  const statusEol = h("button", { class: "ce-status-item", type: "button", title: "Line endings" });
  const statusLang = h("button", { class: "ce-status-item", type: "button", title: "Language" });
  const status = h("div", { class: "ce-status" }, statusPath, h("span", { class: "ce-status-fill" }), statusPos, statusIndent, statusEol, h("span", { class: "ce-status-item is-static" }, "UTF-8"), statusLang);
  const main = h("section", { class: "ce-main" }, head, noticeEl, host, empty, status);
  ctx.root.replaceChildren(h("div", { class: "ce-shell" }, side, main));

  const keys: KeyBinding[] = [
    { key: "Mod-s", preventDefault: true, run: () => (void saveActive(), true) },
    { key: "Mod-p", preventDefault: true, run: () => (quickOpen(), true) },
    { key: "Mod-/", run: toggleComment },
    { key: "Mod-]", run: indentMore },
    { key: "Mod-[", run: indentLess },
  ];
  const view = new EditorView({ parent: host, state: EditorState.create({ doc: "" }) });
  host.classList.add("hidden");

  // ---- tabs
  function makeState(text: string, tab: Pick<Tab, "language" | "tabs" | "size" | "readOnly">): EditorState {
    return EditorState.create({
      doc: text,
      extensions: fileExtensions({ language: tab.language.id, tabs: tab.tabs, size: tab.size, wrap, readOnly: tab.readOnly }, onUpdate, keys),
    });
  }

  function onUpdate(update: ViewUpdate): void {
    const tab = active;
    if (!tab || update.view !== view) return;
    tab.state = update.state;
    if (update.docChanged) {
      tab.dirty = differs(tab, update.state);
      drawTab(tab);
      updateSave();
      keepDraft(tab);
    }
    if (update.docChanged || update.selectionSet) drawStatus();
  }

  /** The tab's text (or line endings) differ from the file's. */
  function differs(tab: Tab, state: EditorState = tab === active ? view.state : tab.state): boolean {
    if (tab.crlf !== tab.savedCrlf) return true;
    return state.doc.length !== tab.saved.length || state.doc.toString() !== tab.saved;
  }

  const draftTimers = new Map<Tab, number>();
  function keepDraft(tab: Tab): void {
    window.clearTimeout(draftTimers.get(tab));
    draftTimers.set(
      tab,
      window.setTimeout(() => {
        draftTimers.delete(tab);
        if (tab.dirty) writeDraft(owner, tab.path, tab.state.doc.toString(), tab.modifiedAt);
        else dropDraft(owner, tab.path);
      }, 600),
    );
  }

  function drawTab(tab: Tab): void {
    tab.button.classList.toggle("is-active", tab === active);
    tab.button.classList.toggle("is-dirty", tab.dirty);
    tab.button.setAttribute("aria-selected", String(tab === active));
    tab.button.title = tab.path + (tab.dirty ? " (unsaved)" : "");
  }

  function makeTabButton(path: string): HTMLElement {
    const close = h("span", { class: "ce-tab-close", role: "button", title: "Close", ariaLabel: `Close ${baseName(path)}` }, iconSvg("close", 12));
    const button = h(
      "div",
      { class: "ce-tab", role: "tab", tabIndex: 0, dataset: { path } },
      iconSvg("file", 13),
      h("span", { class: "ce-tab-name" }, baseName(path)),
      h("span", { class: "ce-tab-dot", ariaHidden: true }),
      close,
    );
    return button;
  }

  tabStrip.addEventListener("click", (event) => {
    const target = event.target as HTMLElement;
    const tabEl = target.closest<HTMLElement>(".ce-tab");
    const tab = tabs.find((t) => t.button === tabEl);
    if (!tab) return;
    if (target.closest(".ce-tab-close")) void closeTab(tab);
    else show(tab);
  });
  tabStrip.addEventListener("auxclick", (event) => {
    if (event.button !== 1) return;
    const tab = tabs.find((t) => t.button === (event.target as HTMLElement).closest(".ce-tab"));
    if (tab) {
      event.preventDefault();
      void closeTab(tab);
    }
  });
  tabStrip.addEventListener("keydown", (event) => {
    const tab = tabs.find((t) => t.button === document.activeElement);
    if (!tab) return;
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      show(tab);
    } else if (event.key === "ArrowRight" || event.key === "ArrowLeft") {
      const at = tabs.indexOf(tab) + (event.key === "ArrowRight" ? 1 : -1);
      tabs[(at + tabs.length) % tabs.length]?.button.focus();
    } else if (event.key === "Delete") {
      void closeTab(tab);
    }
  });
  tabStrip.addEventListener("contextmenu", (event) => {
    const tab = tabs.find((t) => t.button === (event.target as HTMLElement).closest(".ce-tab"));
    if (!tab) return;
    event.preventDefault();
    contextMenu(event.clientX, event.clientY, [
      { label: "Close", icon: "close", onClick: () => void closeTab(tab) },
      { label: "Close others", onClick: () => void closeTabs(tabs.filter((t) => t !== tab)) },
      { label: "Close all", onClick: () => void closeTabs([...tabs]) },
      "separator",
      { label: "Show in the folder", icon: "folder", onClick: () => void revealInTree(tab.path) },
      { label: "Show in File Station", icon: "files", onClick: () => ctx.openApp("files", { path: parentOf(tab.path) }) },
      { label: "Copy path", icon: "copy", onClick: () => void navigator.clipboard?.writeText(tab.path) },
    ]);
  });

  function show(tab: Tab | null): void {
    if (active && active !== tab) active.state = view.state;
    active = tab;
    for (const t of tabs) drawTab(t);
    if (tab) {
      view.setState(tab.state);
      host.classList.remove("hidden");
      empty.classList.add("hidden");
      tab.button.scrollIntoView({ block: "nearest", inline: "nearest" });
      tree.setActive(tab.path);
      ctx.setTitle(`${baseName(tab.path)} — Code Editor`);
      window.requestAnimationFrame(() => {
        if (active === tab && !disposed) view.focus();
      });
    } else {
      host.classList.add("hidden");
      empty.classList.remove("hidden");
      tree.setActive(null);
      ctx.setTitle("Code Editor");
    }
    drawNotice();
    drawStatus();
    updateSave();
    remember();
  }

  async function openFile(path: string, line?: number): Promise<void> {
    const open = tabs.find((t) => t.path === path);
    if (open) {
      show(open);
      if (line) goTo(line);
      return;
    }
    try {
      const entry = await ctx.api.get<FileEntry>("/api/files/stat", { path });
      if (entry.kind !== "file") {
        void revealInTree(path);
        return;
      }
      const text = await ctx.api.get<{ content: string; truncated: boolean; encoding: string }>("/api/files/text", { path, maxBytes: MAX_BYTES });
      if (text.encoding === "binary") {
        ctx.toast.info(`${baseName(path)} is not text. Open it from File Station.`);
        return;
      }
      if (tabs.some((t) => t.path === path)) {
        show(tabs.find((t) => t.path === path) ?? null);
        return;
      }
      const content = text.content.replace(/\r\n?/g, "\n");
      const indent = detectIndent(content);
      const readOnlyWhy = text.truncated
        ? `Only the first ${Math.round(MAX_BYTES / 1024 / 1024)} MB of this file is shown, so it opens read-only.`
        : !entry.writable
          ? "You can read this file but not change it."
          : "";
      const tab: Tab = {
        path,
        saved: content,
        modifiedAt: entry.modifiedAt,
        language: languageOf(path),
        tabs: indent.tabs,
        size: indent.size,
        crlf: lineEnding(text.content) === "CRLF",
        savedCrlf: lineEnding(text.content) === "CRLF",
        readOnly: Boolean(readOnlyWhy),
        readOnlyWhy,
        dirty: false,
        notice: null,
        button: makeTabButton(path),
        state: EditorState.create({ doc: "" }),
      };
      tab.state = makeState(content, tab);
      if (tab.readOnly) tab.notice = { kind: "info", text: readOnlyWhy, actions: [] };
      const draft = tab.readOnly ? null : readDraft(owner, path);
      if (draft && draft.text !== content) {
        tab.notice = {
          kind: "draft",
          text: `Unsaved changes from ${relativeTime(draft.at)}${draft.base && draft.base !== entry.modifiedAt ? ", made before the file last changed" : ""}.`,
          actions: [
            { label: "Restore", primary: true, run: () => restoreDraft(tab) },
            { label: "Discard", run: () => discardDraft(tab) },
          ],
        };
      } else if (draft) {
        dropDraft(owner, path);
      }
      tabs.push(tab);
      tabStrip.appendChild(tab.button);
      show(tab);
      if (line) goTo(line);
      void revealInTree(path);
    } catch (error) {
      ctx.toast.error(`${baseName(path)}: ${errorMessage(error)}`);
    }
  }

  function restoreDraft(tab: Tab): void {
    const draft = readDraft(owner, tab.path);
    tab.notice = null;
    if (draft) replaceText(tab, draft.text);
    drawNotice();
  }

  function discardDraft(tab: Tab): void {
    dropDraft(owner, tab.path);
    tab.notice = null;
    drawNotice();
  }

  /** Put `text` in the tab as an edit (undoable), keeping the cursor near where it was. */
  function replaceText(tab: Tab, text: string): void {
    const state = tab === active ? view.state : tab.state;
    const head = Math.min(state.selection.main.head, text.length);
    const transaction = state.update({ changes: { from: 0, to: state.doc.length, insert: text }, selection: { anchor: head } });
    if (tab === active) view.dispatch(transaction);
    else {
      tab.state = transaction.state;
      tab.dirty = differs(tab);
      drawTab(tab);
    }
  }

  /** Read the file again into a tab without unsaved changes. */
  async function reload(tab: Tab, quiet = false): Promise<void> {
    try {
      const [entry, text] = await Promise.all([
        ctx.api.get<FileEntry>("/api/files/stat", { path: tab.path }),
        ctx.api.get<{ content: string; truncated: boolean; encoding: string }>("/api/files/text", { path: tab.path, maxBytes: MAX_BYTES }),
      ]);
      if (!tabs.includes(tab)) return;
      const content = text.content.replace(/\r\n?/g, "\n");
      tab.modifiedAt = entry.modifiedAt;
      if (content !== tab.saved || content !== (tab === active ? view.state : tab.state).doc.toString()) {
        tab.saved = content;
        replaceText(tab, content);
      }
      tab.saved = content;
      tab.crlf = tab.savedCrlf = lineEnding(text.content) === "CRLF";
      tab.dirty = false;
      dropDraft(owner, tab.path);
      if (tab.notice?.kind === "changed" || tab.notice?.kind === "gone") tab.notice = null;
      drawTab(tab);
      drawNotice();
      updateSave();
      if (!quiet) ctx.toast.info(`${baseName(tab.path)} reloaded`);
    } catch (error) {
      if (!quiet) ctx.toast.error(errorMessage(error));
    }
  }

  async function closeTab(tab: Tab): Promise<boolean> {
    if (tab.dirty) {
      show(tab);
      const answer = await askSave(tab);
      if (answer === "cancel") return false;
      if (answer === "save") {
        if (!(await save_(tab))) return false;
      } else {
        dropDraft(owner, tab.path);
      }
    }
    const at = tabs.indexOf(tab);
    if (at < 0) return true;
    tabs.splice(at, 1);
    tab.button.remove();
    window.clearTimeout(draftTimers.get(tab));
    draftTimers.delete(tab);
    if (active === tab) {
      active = null;
      show(tabs[at] ?? tabs[at - 1] ?? null);
    } else {
      remember();
    }
    return true;
  }

  function askSave(tab: Tab): Promise<"save" | "discard" | "cancel"> {
    return new Promise((resolve) => {
      let answer: "save" | "discard" | "cancel" = "cancel";
      modal({
        within: ctx.root,
        title: `Save ${baseName(tab.path)}?`,
        body: h("p", {}, "It has changes that are not saved yet."),
        actions: [
          { label: "Don't save", variant: "danger", onClick: () => void (answer = "discard") },
          { label: "Cancel", variant: "ghost", onClick: () => undefined },
          { label: "Save", variant: "primary", onClick: () => void (answer = "save") },
        ],
        onClose: () => resolve(answer),
      });
    });
  }

  async function closeTabs(list: Tab[]): Promise<void> {
    for (const tab of list) {
      if (!(await closeTab(tab))) return;
    }
  }

  // ---- saving
  async function writeFile(path: string, text: string): Promise<FileEntry> {
    const query = new URLSearchParams({ dir: parentOf(path), name: baseName(path), overwrite: "true" });
    const response = await fetch(`/api/files/upload?${query}`, {
      method: "POST",
      credentials: "same-origin",
      headers: { "X-CSRF-Token": ctx.api.getCsrfToken(), "Content-Type": "application/octet-stream", Accept: "application/json" },
      body: new Blob([text]),
    });
    const body = (await response.json().catch(() => null)) as (FileEntry & { error?: { message?: string } }) | null;
    if (!response.ok) throw new Error(body?.error?.message ?? `Saving failed (${response.status})`);
    if (!body) throw new Error("The server answered without the saved file");
    return body;
  }

  async function save_(tab: Tab): Promise<boolean> {
    if (tab.readOnly) {
      ctx.toast.info(tab.readOnlyWhy || "This file opens read-only.");
      return false;
    }
    const state = tab === active ? view.state : tab.state;
    const text = state.doc.toString();
    try {
      const now = await ctx.api.get<FileEntry>("/api/files/stat", { path: tab.path }).catch(() => null);
      if (now && tab.modifiedAt && now.modifiedAt !== tab.modifiedAt && tab.notice?.kind !== "gone") {
        const overwrite = await confirmDialog({
          within: ctx.root,
          title: `${baseName(tab.path)} changed`,
          message: "Someone (or AI Agent) changed this file after you opened it. Save yours over it, or keep theirs and look again first?",
          confirmLabel: "Save mine",
          cancelLabel: "Keep theirs",
          danger: true,
        });
        if (!overwrite) return false;
      }
      const entry = await writeFile(tab.path, tab.crlf ? text.replace(/\n/g, "\r\n") : text);
      tab.saved = text;
      tab.savedCrlf = tab.crlf;
      tab.modifiedAt = entry.modifiedAt;
      tab.dirty = differs(tab);
      if (!tab.dirty) dropDraft(owner, tab.path);
      if (tab.notice?.kind === "changed" || tab.notice?.kind === "gone" || tab.notice?.kind === "draft") tab.notice = null;
      drawTab(tab);
      drawNotice();
      updateSave();
      return true;
    } catch (error) {
      ctx.toast.error(`${baseName(tab.path)}: ${errorMessage(error)}`);
      return false;
    }
  }

  async function saveActive(): Promise<void> {
    if (active) await save_(active);
  }

  async function saveAll(): Promise<void> {
    for (const tab of tabs.filter((t) => t.dirty)) await save_(tab);
  }

  function updateSave(): void {
    saveButton.classList.toggle("is-dirty", Boolean(active?.dirty));
    saveButton.classList.toggle("hidden", !active || active.readOnly);
    (saveButton as HTMLButtonElement).disabled = !active?.dirty;
  }

  saveButton.addEventListener("click", () => void saveActive());

  // ---- the notice above the editor
  function drawNotice(): void {
    const notice = active?.notice;
    noticeEl.classList.toggle("hidden", !notice);
    if (!notice) {
      noticeEl.replaceChildren();
      return;
    }
    noticeEl.className = `ce-notice is-${notice.kind}`;
    noticeEl.replaceChildren(
      iconSvg(notice.kind === "info" ? "lock" : notice.kind === "draft" ? "restore" : "warning", 14),
      h("span", { class: "ce-notice-text" }, notice.text),
      ...notice.actions.map((action) => {
        const b = h("button", { class: `btn btn-sm${action.primary ? " btn-primary" : ""}`, type: "button" }, action.label);
        b.addEventListener("click", () => {
          action.run();
          // The notice may be gone: typing goes on in the file.
          if (active && !disposed) view.focus();
        });
        return b;
      }),
    );
  }

  // ---- the status bar
  function drawStatus(): void {
    const tab = active;
    status.classList.toggle("is-empty", !tab);
    if (!tab) {
      statusPath.textContent = tree.root() === "/" ? "Home" : tree.root();
      return;
    }
    const state = view.state;
    const head = state.selection.main.head;
    const line = state.doc.lineAt(head);
    const selected = state.selection.ranges.reduce((n, r) => n + (r.to - r.from), 0);
    statusPath.textContent = relativeTo(tab.path, tree.root());
    statusPath.title = tab.path;
    statusPos.textContent = `Ln ${line.number}, Col ${head - line.from + 1}${selected ? ` (${selected} selected)` : ""}`;
    statusIndent.textContent = tab.tabs ? `Tabs: ${tab.size}` : `Spaces: ${tab.size}`;
    statusEol.textContent = tab.crlf ? "CRLF" : "LF";
    statusLang.textContent = tab.language.label;
  }

  statusPos.addEventListener("click", () => gotoLine(view));
  statusIndent.addEventListener("click", () => {
    const tab = active;
    if (!tab) return;
    const set = (tabsOn: boolean, size: number) => {
      tab.tabs = tabsOn;
      tab.size = size;
      view.dispatch({ effects: indentSlot.reconfigure(indentExtension(tabsOn, size)) });
      drawStatus();
    };
    contextMenu(0, 0, [
      ...[2, 4, 8].map((n) => ({ label: `Spaces: ${n}`, checked: !tab.tabs && tab.size === n, onClick: () => set(false, n) })),
      "separator",
      ...[2, 4, 8].map((n) => ({ label: `Tabs: ${n}`, checked: tab.tabs && tab.size === n, onClick: () => set(true, n) })),
    ] as ContextMenuEntry[], { anchor: statusIndent, ariaLabel: "Indentation" });
  });
  statusEol.addEventListener("click", () => {
    const tab = active;
    if (!tab || tab.readOnly) return;
    contextMenu(0, 0, [
      { label: "LF", hint: "Linux and macOS", checked: !tab.crlf, onClick: () => setEol(tab, false) },
      { label: "CRLF", hint: "Windows", checked: tab.crlf, onClick: () => setEol(tab, true) },
    ], { anchor: statusEol, ariaLabel: "Line endings" });
  });
  function setEol(tab: Tab, crlf: boolean): void {
    if (tab.crlf === crlf) return;
    tab.crlf = crlf;
    tab.dirty = differs(tab);
    drawTab(tab);
    updateSave();
    drawStatus();
  }
  statusLang.addEventListener("click", () => {
    const tab = active;
    if (!tab) return;
    contextMenu(0, 0, languageChoices().map((language) => ({
      label: language.label,
      checked: language.id === tab.language.id,
      onClick: () => {
        tab.language = language;
        view.dispatch({ effects: languageSlot.reconfigure(languageExtension(language.id)) });
        drawStatus();
      },
    })), { anchor: statusLang, ariaLabel: "Language" });
  });

  // ---- the folder
  async function setRoot(root: string): Promise<void> {
    await tree.setRoot(root);
    (rootButton.querySelector(".ce-root-name") as HTMLElement).textContent = root === "/" ? "Home" : baseName(root);
    rootButton.title = root === "/" ? "Your home folder" : root;
    if (active) void tree.reveal(active.path);
    drawStatus();
    remember();
  }

  async function revealInTree(path: string): Promise<void> {
    if (!isInside(path, tree.root())) await setRoot(parentOf(path));
    await tree.reveal(path);
  }

  rootButton.addEventListener("click", () => {
    const recent = recentRoots().filter((r) => r !== tree.root()).slice(0, 5);
    contextMenu(0, 0, [
      { label: "Open folder…", icon: "folder", onClick: () => void chooseFolder() },
      { label: "Home", icon: "home", checked: tree.root() === "/", onClick: () => void setRoot("/") },
      ...(tree.root() !== "/" ? [{ label: "Up one folder", icon: "arrowUp", onClick: () => void setRoot(parentOf(tree.root())) }] : []),
      ...(recent.length ? ["separator" as const, ...recent.map((r) => ({ label: baseName(r), hint: r, icon: "clock", onClick: () => void setRoot(r) }))] : []),
    ], { anchor: rootButton, ariaLabel: "Folder" });
  });

  const RECENT_KEY = `rast.code.recent.${owner}`;
  function recentRoots(): string[] {
    try {
      const list = JSON.parse(localStorage.getItem(RECENT_KEY) ?? "[]") as unknown;
      return Array.isArray(list) ? list.filter((x): x is string => typeof x === "string") : [];
    } catch {
      return [];
    }
  }
  function rememberRoot(root: string): void {
    if (root === "/") return;
    try {
      localStorage.setItem(RECENT_KEY, JSON.stringify([root, ...recentRoots().filter((r) => r !== root)].slice(0, 8)));
    } catch {
      // Recent folders are a convenience.
    }
  }

  async function chooseFolder(): Promise<void> {
    const picked = await pickFolder(ctx, tree.root());
    if (picked) {
      rememberRoot(picked);
      await setRoot(picked);
    }
  }

  newFileButton.addEventListener("click", () => void newEntry(tree.root(), "file"));
  newFolderButton.addEventListener("click", () => void newEntry(tree.root(), "dir"));
  refreshButton.addEventListener("click", () => void tree.refresh());
  sideToggle.addEventListener("click", () => {
    sideHidden = !sideHidden;
    ctx.root.classList.toggle("ce-side-hidden", sideHidden);
  });

  function treeMenu(entry: TreeEntry | null, x: number, y: number): void {
    const dir = entry ? (entry.kind === "dir" ? entry.path : parentOf(entry.path)) : tree.root();
    const items: ContextMenuEntry[] = [];
    if (entry?.kind === "file") items.push({ label: "Open", icon: "file", onClick: () => void openFile(entry.path) }, "separator");
    items.push(
      { label: "New file…", icon: "compose", onClick: () => void newEntry(dir, "file") },
      { label: "New folder…", icon: "plus", onClick: () => void newEntry(dir, "dir") },
    );
    if (entry) {
      items.push(
        "separator",
        ...(entry.kind === "dir" ? [{ label: "Open as the folder", icon: "folder", onClick: () => void setRoot(entry.path) }] : []),
        { label: "Rename…", icon: "rename", onClick: () => void renameEntry(entry) },
        { label: "Copy path", icon: "copy", onClick: () => void navigator.clipboard?.writeText(entry.path) },
        { label: "Show in File Station", icon: "files", onClick: () => ctx.openApp("files", { path: entry.kind === "dir" ? entry.path : parentOf(entry.path) }) },
        "separator",
        { label: "Move to recycle bin", icon: "trash", danger: true, onClick: () => void deleteEntry(entry) },
      );
    } else {
      items.push("separator", { label: "Refresh", icon: "refresh", onClick: () => void tree.refresh() });
    }
    contextMenu(x, y, items, { ariaLabel: entry ? entry.name : "Folder" });
  }

  async function newEntry(dir: string, kind: "file" | "dir"): Promise<void> {
    const name = await promptDialog({
      within: ctx.root,
      title: kind === "file" ? "New file" : "New folder",
      label: `Name, in ${dir === "/" ? "your home folder" : dir}`,
      placeholder: kind === "file" ? "index.ts" : "src",
      confirmLabel: "Create",
      validate: validName,
    });
    if (!name) return;
    const path = joinPath(dir, name.trim());
    try {
      if (kind === "dir") {
        await ctx.api.post("/api/files/mkdir", { path });
      } else {
        const exists = await ctx.api.get<FileEntry>("/api/files/stat", { path }).catch(() => null);
        if (exists) {
          ctx.toast.warning(`${name.trim()} is already there.`);
          void openFile(path);
          return;
        }
        await writeFile(path, "");
      }
      await tree.refresh(dir);
      if (kind === "file") await openFile(path);
      else await tree.reveal(path);
    } catch (error) {
      ctx.toast.error(errorMessage(error));
    }
  }

  async function renameEntry(entry: TreeEntry): Promise<void> {
    const name = await promptDialog({
      within: ctx.root,
      title: `Rename ${entry.name}`,
      label: "New name",
      value: entry.name,
      confirmLabel: "Rename",
      validate: validName,
    });
    if (!name || name.trim() === entry.name) return;
    const to = joinPath(parentOf(entry.path), name.trim());
    try {
      await ctx.api.post("/api/files/rename", { path: entry.path, newName: name.trim() });
      for (const tab of tabs) {
        if (!isInside(tab.path, entry.path)) continue;
        const moved = to + tab.path.slice(entry.path.length);
        moveDraft(owner, tab.path, moved);
        tab.path = moved;
        tab.language = languageOf(moved);
        tab.button.dataset.path = moved;
        (tab.button.querySelector(".ce-tab-name") as HTMLElement).textContent = baseName(moved);
        drawTab(tab);
      }
      await tree.refresh(parentOf(entry.path));
      if (active) show(active);
    } catch (error) {
      ctx.toast.error(errorMessage(error));
    }
  }

  async function deleteEntry(entry: TreeEntry): Promise<void> {
    const ok = await confirmDialog({
      within: ctx.root,
      title: `Move ${entry.name} to the recycle bin?`,
      message: "You can restore it from the recycle bin in File Station.",
      confirmLabel: "Move to recycle bin",
      danger: true,
    });
    if (!ok) return;
    try {
      await ctx.api.post("/api/files/delete", { paths: [entry.path] });
      for (const tab of tabs.filter((t) => isInside(t.path, entry.path))) {
        dropDraft(owner, tab.path);
        tab.dirty = false;
        await closeTab(tab);
      }
    } catch (error) {
      ctx.toast.error(errorMessage(error));
    }
  }

  // ---- find a file by name
  function quickOpen(): void {
    const input = h("input", { class: "input ce-qo-input", type: "search", placeholder: `Search ${tree.root() === "/" ? "your files" : baseName(tree.root())} by name`, autocomplete: "off", spellcheck: false }) as HTMLInputElement;
    const list = h("div", { class: "ce-qo-list", role: "listbox", ariaLabel: "Files" });
    const hint = h("p", { class: "small muted ce-qo-hint" }, "Type part of a file's name.");
    let picked = 0;
    let items: FileEntry[] = [];
    let token = 0;
    let close: () => void = () => undefined;
    const draw = () => {
      list.replaceChildren(
        ...items.map((item, i) => {
          const row = h(
            "button",
            { class: `ce-qo-item${i === picked ? " is-picked" : ""}`, type: "button", role: "option" },
            iconSvg("file", 14),
            h("span", { class: "ce-qo-name" }, item.name),
            h("span", { class: "ce-qo-path" }, relativeTo(parentOf(item.path), tree.root())),
          );
          row.setAttribute("aria-selected", String(i === picked));
          row.addEventListener("click", () => {
            close();
            void openFile(item.path);
          });
          return row;
        }),
      );
      list.querySelector(".is-picked")?.scrollIntoView({ block: "nearest" });
    };
    const searchNow = async () => {
      const q = input.value.trim();
      const mine = ++token;
      if (!q) {
        items = tabs.map((t) => ({ name: baseName(t.path), path: t.path }) as FileEntry);
        hint.textContent = items.length ? "Open files. Type to search by name." : "Type part of a file's name.";
        picked = 0;
        draw();
        return;
      }
      hint.textContent = "Searching…";
      try {
        const found = await ctx.api.get<{ items: FileEntry[]; truncated: boolean }>("/api/files/search", { q, path: tree.root(), type: "file", perPage: 50 });
        if (mine !== token) return;
        items = found.items.filter((e) => e.kind === "file");
        picked = 0;
        hint.textContent = items.length ? (found.truncated ? "The first matches." : "") : "No file has that in its name.";
        draw();
      } catch (error) {
        if (mine === token) hint.textContent = errorMessage(error);
      }
    };
    let timer = 0;
    input.addEventListener("input", () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => void searchNow(), 180);
    });
    input.addEventListener("keydown", (event) => {
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        if (!items.length) return;
        picked = (picked + (event.key === "ArrowDown" ? 1 : items.length - 1)) % items.length;
        draw();
      } else if (event.key === "Enter") {
        event.preventDefault();
        const item = items[picked];
        if (item) {
          close();
          void openFile(item.path);
        }
      }
    });
    close = modal({ within: ctx.root, title: "Find a file", body: h("div", { class: "ce-qo" }, input, hint, list) });
    void searchNow();
    input.focus();
  }
  quickOpenButton.addEventListener("click", quickOpen);

  moreButton.addEventListener("click", () => {
    const tab = active;
    contextMenu(0, 0, [
      { label: "Find a file", icon: "search", shortcut: "Ctrl+P", onClick: quickOpen },
      { label: "Search in this file", icon: "search", shortcut: "Ctrl+F", disabled: !tab, onClick: () => openSearchPanel(view) },
      { label: "Go to line", icon: "arrowRight", shortcut: "Ctrl+G", disabled: !tab, onClick: () => gotoLine(view) },
      "separator",
      { label: "Save", icon: "check", shortcut: "Ctrl+S", disabled: !tab?.dirty, onClick: () => void saveActive() },
      { label: "Save all", disabled: !tabs.some((t) => t.dirty), onClick: () => void saveAll() },
      { label: "Reload from disk", icon: "refresh", disabled: !tab, onClick: () => tab && void reloadAsked(tab) },
      "separator",
      { label: "Wrap long lines", checked: wrap, onClick: () => setWrap(!wrap) },
      { label: "Close all", disabled: !tabs.length, onClick: () => void closeTabs([...tabs]) },
    ], { anchor: moreButton, ariaLabel: "More" });
  });

  async function reloadAsked(tab: Tab): Promise<void> {
    if (tab.dirty) {
      const ok = await confirmDialog({
        within: ctx.root,
        title: `Reload ${baseName(tab.path)}?`,
        message: "Your unsaved changes to it are dropped.",
        confirmLabel: "Reload",
        danger: true,
      });
      if (!ok) return;
    }
    await reload(tab);
  }

  function setWrap(on: boolean): void {
    wrap = on;
    try {
      localStorage.setItem(WRAP_KEY, on ? "1" : "0");
    } catch {
      // A preference only.
    }
    const extension: Extension = on ? EditorView.lineWrapping : [];
    view.dispatch({ effects: wrapSlot.reconfigure(extension) });
    for (const tab of tabs) {
      if (tab !== active) tab.state = tab.state.update({ effects: wrapSlot.reconfigure(extension) }).state;
    }
  }

  function goTo(line: number): void {
    const doc = view.state.doc;
    const target = doc.line(Math.max(1, Math.min(doc.lines, Math.floor(line))));
    view.dispatch({ selection: { anchor: target.from }, effects: EditorView.scrollIntoView(target.from, { y: "center" }) });
    view.focus();
  }

  // ---- what changes elsewhere
  const changedDirs = new Set<string>();
  let dirTimer = 0;
  const stopChanges = realtime.on<FileChanged>("file.changed", (event) => {
    const paths = [event.path, ...(event.paths ?? [])].filter((p): p is string => typeof p === "string" && p.startsWith("/"));
    for (const path of paths) {
      const dir = parentOf(path);
      if (tree.shows(dir)) changedDirs.add(dir);
      for (const tab of tabs.filter((t) => t.path === path || isInside(t.path, path))) void noticeChange(tab);
    }
    if (changedDirs.size) {
      window.clearTimeout(dirTimer);
      dirTimer = window.setTimeout(() => {
        const dirs = [...changedDirs];
        changedDirs.clear();
        for (const dir of dirs) void tree.refresh(dir);
      }, 250);
    }
  });

  const checking = new Set<Tab>();
  async function noticeChange(tab: Tab): Promise<void> {
    if (checking.has(tab)) return;
    checking.add(tab);
    try {
      const entry = await ctx.api.get<FileEntry>("/api/files/stat", { path: tab.path }).catch(() => null);
      if (!tabs.includes(tab)) return;
      if (!entry) {
        tab.notice = {
          kind: "gone",
          text: "This file was moved or deleted. Saving writes it here again.",
          actions: [
            { label: "Save here", primary: true, run: () => void save_(tab) },
            { label: "Close", run: () => void closeTab(tab) },
          ],
        };
        tab.dirty = true;
        drawTab(tab);
        updateSave();
        if (tab === active) drawNotice();
        return;
      }
      if (entry.modifiedAt === tab.modifiedAt) return;
      if (!tab.dirty) {
        // Nothing of theirs to lose: show what is there now.
        await reload(tab, true);
        flash(tab);
        return;
      }
      tab.notice = {
        kind: "changed",
        text: "This file changed after you opened it, and you have unsaved changes.",
        actions: [
          { label: "Show theirs", run: () => void reload(tab) },
          { label: "Keep mine", primary: true, run: () => keepMine(tab, entry.modifiedAt) },
        ],
      };
      if (tab === active) drawNotice();
    } finally {
      checking.delete(tab);
    }
  }

  function keepMine(tab: Tab, modifiedAt: number): void {
    tab.modifiedAt = modifiedAt;
    tab.notice = null;
    drawNotice();
  }

  function flash(tab: Tab): void {
    tab.button.classList.remove("is-flash");
    void tab.button.offsetWidth;
    tab.button.classList.add("is-flash");
  }

  // ---- where the window is
  function remember(): void {
    if (disposed) return;
    ctx.saveState({ root: tree.root(), tabs: tabs.map((t) => t.path), active: active?.path ?? null } satisfies Place);
  }

  async function place(params: unknown, fromLauncher: boolean): Promise<void> {
    const p = (params && typeof params === "object" ? params : {}) as Place;
    if (typeof p.root === "string" && p.root.startsWith("/") && p.root !== tree.root()) await setRoot(p.root);
    if (Array.isArray(p.tabs)) {
      for (const path of p.tabs) {
        if (typeof path === "string" && !tabs.some((t) => t.path === path)) await openFile(path);
      }
      if (!fromLauncher) {
        const keep = new Set(p.tabs);
        for (const tab of tabs.filter((t) => !keep.has(t.path) && !t.dirty)) await closeTab(tab);
      }
      const want = tabs.find((t) => t.path === p.active);
      if (want && want !== active) show(want);
    }
    if (typeof p.path === "string" && p.path.startsWith("/")) await openFile(p.path, typeof p.line === "number" ? p.line : undefined);
  }

  ctx.onReopen((params) => void place(params, true));
  ctx.onPlace((params) => void place(params, false));
  ctx.onResize(() => {
    const narrow = ctx.root.clientWidth < 620;
    ctx.root.classList.toggle("ce-narrow", narrow);
    if (narrow && !sideHidden && tabs.length) {
      sideHidden = true;
      ctx.root.classList.add("ce-side-hidden");
    }
  });
  ctx.root.addEventListener("keydown", (event) => {
    if (event.defaultPrevented || !(event.ctrlKey || event.metaKey) || event.altKey) return;
    const key = event.key.toLowerCase();
    if (key === "s") {
      event.preventDefault();
      void (event.shiftKey ? saveAll() : saveActive());
    } else if (key === "p" && !event.shiftKey) {
      event.preventDefault();
      quickOpen();
    }
  });
  ctx.onDispose(() => {
    disposed = true;
    stopChanges();
    window.clearTimeout(dirTimer);
    for (const timer of draftTimers.values()) window.clearTimeout(timer);
    // Unsaved changes stay as drafts for next time.
    for (const tab of tabs) {
      const text = (tab === active ? view.state : tab.state).doc.toString();
      if (tab.dirty && text !== tab.saved) writeDraft(owner, tab.path, text, tab.modifiedAt);
    }
    view.destroy();
  });

  // ---- start
  void (async () => {
    await setRoot("/");
    await place(ctx.params, true);
    if (!tabs.length) show(null);
  })();

  return {
    destroy() {
      // ctx.onDispose cleans up.
    },
  };
}

/** A folder of the person's files, chosen in a dialog (home, then shared folders). */
function pickFolder(ctx: AppContext, start: string): Promise<string | null> {
  return new Promise((resolve) => {
    let chosen: string | null = null;
    let current = start;
    const crumbs = h("div", { class: "ce-pick-crumbs" });
    const list = h("div", { class: "ce-pick-list", role: "listbox", ariaLabel: "Folders" });
    const pathEl = h("p", { class: "small muted ce-pick-path" });
    let token = 0;
    async function open(path: string): Promise<void> {
      const mine = ++token;
      current = path;
      pathEl.textContent = path === "/" ? "Your home folder" : path;
      list.replaceChildren(h("p", { class: "small muted" }, "Loading folders…"));
      try {
        const listing = await ctx.api.get<{ items: FileEntry[]; breadcrumbs: Array<{ name: string; path: string }> }>("/api/files", { path, limit: 1000 });
        const roots = path === "/" ? await ctx.api.get<FileRoot[]>("/api/files/roots").catch(() => []) : [];
        if (mine !== token) return;
        crumbs.replaceChildren(
          ...listing.breadcrumbs.flatMap((crumb, i) => {
            const b = h("button", { class: "ce-pick-crumb", type: "button" }, i === 0 ? iconSvg("home", 13) : crumb.name);
            if (i === 0) b.title = "Home";
            b.addEventListener("click", () => void open(crumb.path));
            return i ? [h("span", { class: "ce-pick-sep" }, "/"), b] : [b];
          }),
        );
        const folders = listing.items.filter((e) => e.kind === "dir").map((e) => ({ name: e.name, path: e.path, icon: "folder" }));
        const shares = (Array.isArray(roots) ? roots : []).filter((r) => r.kind === "share").map((r) => ({ name: r.name, path: r.path, icon: "share" }));
        list.replaceChildren(
          ...[...folders, ...shares].map((f) => {
            const b = h("button", { class: "ce-pick-item", type: "button", role: "option" }, iconSvg(f.icon, 14), h("span", {}, f.name));
            b.addEventListener("click", () => void open(f.path));
            return b;
          }),
        );
        if (!list.children.length) list.appendChild(h("p", { class: "small muted" }, "No folders inside. Open this one, or go back up."));
      } catch (error) {
        if (mine === token) list.replaceChildren(h("p", { class: "small form-error" }, errorMessage(error)));
      }
    }
    modal({
      within: ctx.root,
      title: "Open folder",
      body: h("div", { class: "ce-pick" }, crumbs, list, pathEl),
      actions: [
        { label: "Cancel", variant: "ghost", onClick: () => undefined },
        {
          label: "Open",
          variant: "primary",
          onClick: () => {
            chosen = current;
          },
        },
      ],
      onClose: () => resolve(chosen),
    });
    void open(start);
  });
}
