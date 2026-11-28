// The model and effort under the message box: one chip ("GPT-5 Low ⌃")
// that opens a small menu (Model ›, Effort ›). Model opens a list to search,
// grouped by provider with its logo; Effort the levels with what each does.
// The choices come from the window's own <select> (agents, then each
// provider's models, and a chat's own choice that is no longer offered).

import { h } from "../../core/dom";
import { iconSvg } from "../../core/icons";
import { agentAvatar, providerMark } from "./marks";
import { openPopover, type Popover } from "./popover";
import { EFFORTS, effortLabel, type Effort, type PublicProvider } from "./model";

export interface ModelMenu {
  button: HTMLButtonElement;
  /** Draw the chip again (after the choice, effort or options changed). */
  update(): void;
  close(): void;
}

export interface ModelMenuSource {
  root: HTMLElement;
  picker: HTMLSelectElement;
  providers(): PublicProvider[];
  effort(): Effort;
  /** Who answers for a picker value: name and picture. */
  describe(choice: string): { name: string; picture: (size: number) => HTMLElement } | null;
  onPick(choice: string): void;
  onEffort(effort: Effort): void;
}

interface Group {
  label: string;
  mark: HTMLElement | null;
  items: Array<{ value: string; label: string; agent: boolean }>;
}

export function createModelMenu(source: ModelMenuSource): ModelMenu {
  const face = h("span", { class: "ai-mpick-face" });
  const button = h(
    "button",
    { class: "ai-mpick", type: "button", title: "Model and effort", ariaLabel: "Model and effort" },
    face,
    iconSvg("chevronUp", 12),
  ) as HTMLButtonElement;
  button.setAttribute("aria-haspopup", "dialog");
  button.setAttribute("aria-expanded", "false");
  let popover: Popover | null = null;

  function groups(): Group[] {
    const out: Group[] = [];
    const kinds = new Map(source.providers().map((p) => [p.id, p.kind]));
    for (const child of Array.from(source.picker.children)) {
      if (child instanceof HTMLOptGroupElement) {
        const options = Array.from(child.querySelectorAll("option"));
        const first = options[0]?.value ?? "";
        const providerId = first.startsWith("model:") ? first.slice(6, first.indexOf("|")) : "";
        out.push({
          label: child.label,
          mark: providerId ? providerMark(kinds.get(providerId) ?? "custom", 16) : null,
          items: options.map((o) => ({ value: o.value, label: o.textContent ?? "", agent: o.value.startsWith("agent:") })),
        });
      } else if (child instanceof HTMLOptionElement) {
        out.push({ label: "This chat", mark: null, items: [{ value: child.value, label: child.textContent ?? "", agent: false }] });
      }
    }
    return out;
  }

  function update(): void {
    const who = source.describe(source.picker.value);
    face.replaceChildren(
      ...(who ? [who.picture(16), h("span", { class: "ai-mpick-name" }, who.name)] : [h("span", { class: "ai-mpick-name" }, "Choose a model")]),
      h("span", { class: "ai-mpick-effort" }, effortLabel(source.effort())),
    );
    button.disabled = source.picker.disabled;
    button.classList.toggle("hidden", !source.picker.options.length);
  }

  function row(label: string, value: string, onClick: () => void): HTMLElement {
    const el = h(
      "button",
      { class: "ai-mm-row", type: "button" },
      h("span", { class: "ai-mm-row-label" }, label),
      h("span", { class: "ai-mm-row-value" }, value),
      iconSvg("chevronRight", 13),
    );
    el.addEventListener("click", onClick);
    return el;
  }

  /** Show a view of the menu: its search box, its choice or its first row gets the focus. */
  function show(view: HTMLElement): void {
    if (!popover) return;
    popover.el.replaceChildren(view);
    popover.place();
    (
      view.querySelector<HTMLElement>(".ai-mm-search") ??
      view.querySelector<HTMLElement>(".ai-mm-effort.is-picked") ??
      view.querySelector<HTMLElement>(".ai-mm-row, button")
    )?.focus();
  }

  function rootView(): HTMLElement {
    const who = source.describe(source.picker.value);
    const rows = [
      row("Model", who?.name ?? "Choose", () => show(modelView())),
      row("Effort", effortLabel(source.effort()), () => show(effortView())),
    ];
    const view = h("div", { class: "ai-mm-root" }, ...rows);
    view.addEventListener("keydown", (event) => {
      const at = rows.indexOf(document.activeElement as HTMLElement);
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        rows[(at + (event.key === "ArrowDown" ? 1 : rows.length - 1)) % rows.length]?.focus();
      } else if (event.key === "ArrowRight" && at >= 0) {
        event.preventDefault();
        rows[at]?.click();
      }
    });
    return view;
  }

  function back(title: string): HTMLElement {
    const el = h("button", { class: "ai-mm-back", type: "button", title: "Back", ariaLabel: "Back" }, iconSvg("chevronLeft", 14));
    el.addEventListener("click", () => show(rootView()));
    el.dataset.title = title;
    return el;
  }

  function modelView(): HTMLElement {
    const all = groups();
    const count = all.reduce((n, g) => n + g.items.length, 0);
    const search = h("input", {
      class: "ai-mm-search",
      type: "search",
      placeholder: count > 1 ? `Search ${count} models` : "Search models",
      ariaLabel: "Search models",
      autocomplete: "off",
      spellcheck: false,
    }) as HTMLInputElement;
    const list = h("div", { class: "ai-mm-list", role: "listbox", ariaLabel: "Models" });
    let items: HTMLElement[] = [];
    let active = -1;
    const activate = (index: number) => {
      items[active]?.classList.remove("is-active");
      active = Math.max(0, Math.min(items.length - 1, index));
      const item = items[active];
      if (!item) return;
      item.classList.add("is-active");
      item.scrollIntoView({ block: "nearest" });
    };
    const draw = () => {
      const query = search.value.trim().toLowerCase();
      const chosen = source.picker.value;
      list.replaceChildren();
      items = [];
      for (const group of all) {
        const inGroup = query && group.label.toLowerCase().includes(query);
        const shown = group.items.filter((i) => !query || inGroup || i.label.toLowerCase().includes(query));
        if (!shown.length) continue;
        list.appendChild(
          h(
            "div",
            { class: "ai-mm-group", role: "presentation" },
            group.mark ?? (group.items[0]?.agent ? iconSvg("sparkle", 13) : iconSvg("clock", 13)),
            h("span", {}, group.label),
          ),
        );
        for (const item of shown) {
          const picked = item.value === chosen;
          const el = h(
            "button",
            { class: `ai-mm-item${picked ? " is-picked" : ""}`, type: "button", role: "option", tabIndex: -1 },
            item.agent ? agentAvatar(item.label, 16) : null,
            h("span", { class: "ai-mm-item-name" }, item.label),
            picked ? iconSvg("check", 14) : null,
          );
          el.setAttribute("aria-selected", String(picked));
          if (item.label.length > 34) el.title = item.label;
          el.addEventListener("click", () => {
            popover?.close();
            if (item.value !== source.picker.value) source.onPick(item.value);
          });
          el.addEventListener("pointermove", () => {
            const at = items.indexOf(el);
            if (at !== active) activate(at);
          });
          list.appendChild(el);
          items.push(el);
        }
      }
      if (!items.length) list.appendChild(h("p", { class: "ai-mm-empty small muted" }, "No model matches."));
      active = -1;
      const picked = items.findIndex((el) => el.classList.contains("is-picked"));
      activate(query ? 0 : Math.max(0, picked));
    };
    search.addEventListener("input", draw);
    search.addEventListener("keydown", (event) => {
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        activate(active + (event.key === "ArrowDown" ? 1 : -1));
      } else if (event.key === "Enter") {
        event.preventDefault();
        items[active]?.click();
      } else if (event.key === "Backspace" && !search.value) {
        event.preventDefault();
        show(rootView());
      }
    });
    const view = h("div", { class: "ai-mm-models" }, h("div", { class: "ai-mm-head" }, back("Model"), search), list);
    draw();
    return view;
  }

  function effortView(): HTMLElement {
    const chosen = source.effort();
    const buttons = EFFORTS.map((e) => {
      const el = h(
        "button",
        { class: `ai-mm-item ai-mm-effort${e.id === chosen ? " is-picked" : ""}`, type: "button", role: "option" },
        h("span", { class: "ai-mm-item-name" }, h("span", {}, e.label), h("span", { class: "ai-mm-hint" }, e.hint)),
        e.id === chosen ? iconSvg("check", 14) : null,
      );
      el.setAttribute("aria-selected", String(e.id === chosen));
      el.addEventListener("click", () => {
        popover?.close();
        if (e.id !== chosen) source.onEffort(e.id);
      });
      return el;
    });
    const list = h("div", { class: "ai-mm-list is-short", role: "listbox", ariaLabel: "Effort" }, ...buttons);
    list.addEventListener("keydown", (event) => {
      const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        buttons[(at + (event.key === "ArrowDown" ? 1 : buttons.length - 1)) % buttons.length]?.focus();
      } else if (event.key === "ArrowLeft") {
        event.preventDefault();
        show(rootView());
      }
    });
    const view = h(
      "div",
      { class: "ai-mm-efforts" },
      h("div", { class: "ai-mm-head" }, back("Effort"), h("span", { class: "ai-mm-title" }, "Effort")),
      list,
    );
    return view;
  }

  button.addEventListener("click", () => {
    if (popover) {
      popover.close();
      return;
    }
    popover = openPopover(source.root, button, h("div", {}), {
      className: "ai-mm",
      label: "Model and effort",
      onClose: () => {
        popover = null;
      },
    });
    show(rootView());
  });

  return {
    button,
    update,
    close: () => popover?.close(),
  };
}
