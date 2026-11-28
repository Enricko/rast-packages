// Workspaces: a folder a chat focuses on (src/workspace.rs). The chat list
// shows one workspace's chats or all of them; a new chat starts in the one
// shown; the chip under the message box moves a chat to another. A
// workspace is a folder of the person's RAST files, or for administrators
// a folder on the server.

import type { AppContext } from "../../core/app";
import { h } from "../../core/dom";
import { iconSvg } from "../../core/icons";
import type { FileListing, FileRoot } from "../../core/types";
import { confirmDialog, errorMessage, modal } from "../../core/ui";
import { openPopover, type Popover } from "./popover";

export interface Workspace {
  id: string;
  name: string;
  place: "files" | "server";
  path: string;
  createdAt: number;
  updatedAt: number;
}

export interface Workspaces {
  all(): Workspace[];
  byId(id: string | null | undefined): Workspace | null;
  load(): Promise<void>;
  /** A menu of the workspaces under `anchor`: pick one, none (`null`) or make a new one. */
  menu(anchor: HTMLElement, options: MenuOptions): void;
  /** The dialog for a new workspace (or to change one); the saved workspace. */
  edit(workspace?: Workspace): Promise<Workspace | null>;
  close(): void;
}

interface MenuOptions {
  current: string | null;
  /** The label of the choice without a workspace. */
  none: string;
  noneHint: string;
  onPick(id: string | null): void;
}

export function createWorkspaces(ctx: AppContext, options: { root: HTMLElement; isAdmin: boolean; onChange(): void }): Workspaces {
  let items: Workspace[] = [];
  let popover: Popover | null = null;

  async function load(): Promise<void> {
    try {
      const body = await ctx.api.get<{ items: Workspace[] }>("/api/ai/workspaces");
      items = body.items;
    } catch {
      // The chat works without them; the list stays as it was.
    }
    options.onChange();
  }

  function row(label: string, hint: string, icon: string, picked: boolean, onClick: () => void, extra?: HTMLElement): HTMLElement {
    const button = h(
      "button",
      { class: `ai-ws-item${picked ? " is-picked" : ""}`, type: "button", role: "option", title: hint },
      iconSvg(icon, 15),
      h("span", { class: "ai-ws-item-text" }, h("span", { class: "ai-ws-item-name" }, label), hint ? h("span", { class: "ai-ws-item-path" }, hint) : null),
      picked ? iconSvg("check", 14) : null,
    );
    button.setAttribute("aria-selected", String(picked));
    button.addEventListener("click", onClick);
    return extra ? h("div", { class: "ai-ws-row" }, button, extra) : button;
  }

  function menu(anchor: HTMLElement, menuOptions: MenuOptions): void {
    if (popover) {
      popover.close();
      return;
    }
    const pick = (id: string | null) => {
      popover?.close();
      menuOptions.onPick(id);
    };
    const list = h(
      "div",
      { class: "ai-ws-list", role: "listbox", ariaLabel: "Workspaces" },
      row(menuOptions.none, menuOptions.noneHint, "layers", menuOptions.current === null, () => pick(null)),
      ...items.map((w) => {
        const change = h("button", { class: "ai-ws-edit", type: "button", title: `Change ${w.name}`, ariaLabel: `Change ${w.name}` }, iconSvg("edit", 13));
        change.addEventListener("click", () => {
          popover?.close();
          void edit(w);
        });
        return row(w.name, placeText(w), w.place === "server" ? "terminal" : "folder", menuOptions.current === w.id, () => pick(w.id), change);
      }),
    );
    const add = h("button", { class: "ai-ws-new", type: "button" }, iconSvg("plus", 14), h("span", {}, "New workspace"));
    add.addEventListener("click", () => {
      popover?.close();
      void edit().then((made) => {
        if (made) menuOptions.onPick(made.id);
      });
    });
    popover = openPopover(options.root, anchor, h("div", { class: "ai-ws-menu" }, list, add), {
      className: "ai-ws-pop",
      label: "Workspaces",
      onClose: () => {
        popover = null;
      },
    });
    (list.querySelector<HTMLElement>(".ai-ws-item.is-picked") ?? list.querySelector<HTMLElement>(".ai-ws-item"))?.focus();
    list.addEventListener("keydown", (event) => {
      const buttons = Array.from(list.querySelectorAll<HTMLElement>(".ai-ws-item"));
      const at = buttons.indexOf(document.activeElement as HTMLElement);
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        buttons[(at + (event.key === "ArrowDown" ? 1 : buttons.length - 1)) % buttons.length]?.focus();
      }
    });
  }

  function edit(workspace?: Workspace): Promise<Workspace | null> {
    return new Promise((resolve) => {
      let saved: Workspace | null = null;
      let place: Workspace["place"] = workspace?.place ?? "files";
      const name = h("input", { class: "input", type: "text", placeholder: "The folder's name", value: workspace?.name ?? "" }) as HTMLInputElement;
      name.maxLength = 80;
      const path = h("input", { class: "input ai-ws-path", type: "text", spellcheck: false, autocomplete: "off", value: workspace?.path ?? "/" }) as HTMLInputElement;
      const error = h("div", { class: "form-error", role: "alert" });
      const browser = folderBrowser(ctx, (chosen) => {
        path.value = chosen;
        error.textContent = "";
      });
      const placeButtons = (["files", "server"] as const).map((p) => {
        const b = h("button", { class: "ai-ws-place", type: "button" }, iconSvg(p === "files" ? "folder" : "terminal", 14), p === "files" ? "My files" : "Server") as HTMLButtonElement;
        b.addEventListener("click", () => setPlace(p));
        return b;
      });
      const placeRow = h("div", { class: "ai-ws-places", role: "group", ariaLabel: "Where the folder is" }, ...placeButtons);
      const hint = h("p", { class: "small muted ai-ws-hint" });
      function setPlace(next: Workspace["place"]): void {
        const changed = next !== place;
        place = next;
        placeButtons.forEach((b, i) => b.classList.toggle("is-picked", (i === 0 ? "files" : "server") === place));
        browser.el.classList.toggle("hidden", place !== "files");
        path.placeholder = place === "files" ? "/projects/site" : "/srv/app";
        hint.textContent =
          place === "files"
            ? "A folder of your RAST files. The chat's file tools stay inside it, and commands start in it."
            : "A folder on the server. Commands start in it, and server paths in the chat are read from it.";
        if (changed) path.value = place === "files" ? "/" : "";
        if (place === "files") void browser.open(path.value.startsWith("/") ? path.value : "/");
      }
      const body = h(
        "div",
        { class: "col ai-ws-form" },
        h("label", { class: "field" }, h("span", { class: "label" }, "Name"), name),
        options.isAdmin ? placeRow : null,
        h("label", { class: "field" }, h("span", { class: "label" }, "Folder"), path),
        browser.el,
        hint,
        error,
      );
      const actions: Parameters<typeof modal>[0]["actions"] = [];
      if (workspace) {
        actions.push({
          label: "Remove",
          variant: "danger",
          keepOpen: true,
          onClick: async (close) => {
            if (await remove(workspace)) {
              close();
            }
          },
        });
      }
      actions.push(
        { label: "Cancel", variant: "ghost", onClick: () => undefined },
        {
          label: workspace ? "Save" : "Create",
          variant: "primary",
          keepOpen: true,
          onClick: async (close) => {
            error.textContent = "";
            try {
              const payload = { name: name.value.trim(), place, path: path.value.trim() };
              const res = workspace
                ? await ctx.api.patch<{ workspace: Workspace }>(`/api/ai/workspaces/${encodeURIComponent(workspace.id)}`, payload)
                : await ctx.api.post<{ workspace: Workspace }>("/api/ai/workspaces", payload);
              saved = res.workspace;
              items = [...items.filter((w) => w.id !== saved?.id), res.workspace].sort((a, b) => a.name.localeCompare(b.name));
              options.onChange();
              close();
            } catch (e) {
              error.textContent = errorMessage(e);
            }
          },
        },
      );
      modal({
        within: options.root,
        title: workspace ? "Change workspace" : "New workspace",
        body,
        actions,
        onClose: () => resolve(saved),
      });
      setPlace(place);
      if (workspace) path.value = workspace.path;
      name.focus();
    });
  }

  async function remove(workspace: Workspace): Promise<boolean> {
    const ok = await confirmDialog({
      within: options.root,
      title: `Remove ${workspace.name}?`,
      message: "Its chats stay, in All chats. The folder and its files stay as they are.",
      confirmLabel: "Remove",
      danger: true,
    });
    if (!ok) return false;
    try {
      await ctx.api.del(`/api/ai/workspaces/${encodeURIComponent(workspace.id)}`);
      items = items.filter((w) => w.id !== workspace.id);
      options.onChange();
      return true;
    } catch (e) {
      ctx.toast.error(errorMessage(e));
      return false;
    }
  }

  return {
    all: () => items,
    byId: (id) => (id ? (items.find((w) => w.id === id) ?? null) : null),
    load,
    menu,
    edit,
    close: () => popover?.close(),
  };
}

export function placeText(workspace: Workspace): string {
  return workspace.place === "server" ? `Server: ${workspace.path}` : workspace.path;
}

/** Folders of the person's files to choose from (home, then shared ones). */
function folderBrowser(ctx: AppContext, onChoose: (path: string) => void): { el: HTMLElement; open(path: string): Promise<void> } {
  const crumbs = h("div", { class: "ai-ws-crumbs" });
  const list = h("div", { class: "ai-ws-folders", role: "listbox", ariaLabel: "Folders" });
  const el = h("div", { class: "ai-ws-browser" }, crumbs, list);
  let token = 0;

  async function open(path: string): Promise<void> {
    const mine = ++token;
    list.replaceChildren(h("p", { class: "small muted" }, "Loading folders…"));
    try {
      const listing = await ctx.api.get<FileListing>(`/api/files?path=${encodeURIComponent(path)}&limit=500`);
      const roots = path === "/" ? await ctx.api.get<FileRoot[]>("/api/files/roots").catch(() => null) : null;
      if (mine !== token) return;
      crumbs.replaceChildren(
        ...listing.breadcrumbs.flatMap((crumb, i) => {
          const b = h("button", { class: "ai-ws-crumb", type: "button" }, i === 0 ? iconSvg("home", 13) : null, i === 0 ? "" : crumb.name);
          if (i === 0) b.title = "Home";
          b.addEventListener("click", () => {
            onChoose(crumb.path);
            void open(crumb.path);
          });
          return i ? [h("span", { class: "ai-ws-crumb-sep" }, "/"), b] : [b];
        }),
      );
      const folders = listing.items.filter((e) => e.kind === "dir");
      const shares = (Array.isArray(roots) ? roots : []).filter((r) => r.kind === "share");
      list.replaceChildren();
      for (const entry of [...folders.map((f) => ({ name: f.name, path: f.path, icon: "folder" })), ...shares.map((s) => ({ name: s.name, path: s.path, icon: "share" }))]) {
        const b = h("button", { class: "ai-ws-folder", type: "button", role: "option" }, iconSvg(entry.icon, 14), h("span", {}, entry.name));
        b.addEventListener("click", () => {
          onChoose(entry.path);
          void open(entry.path);
        });
        list.appendChild(b);
      }
      if (!list.children.length) list.appendChild(h("p", { class: "small muted" }, "This folder has no folders inside. Choose it, or go back up."));
    } catch (e) {
      if (mine === token) list.replaceChildren(h("p", { class: "small form-error" }, errorMessage(e)));
    }
  }

  return { el, open };
}
