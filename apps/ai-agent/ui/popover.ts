// A popover over AI Agent's own window (never the whole desktop): above
// its button when there is room (the message box sits at the bottom), else
// below; kept inside the window. Escape or a click elsewhere closes it.

import { h } from "../../core/dom";

export interface Popover {
  el: HTMLElement;
  close(): void;
  /** Place it again (after its content changed size). */
  place(): void;
}

let open: Popover | null = null;

export function openPopover(
  root: HTMLElement,
  anchor: HTMLElement,
  content: HTMLElement,
  options: { className: string; label: string; onClose?: () => void },
): Popover {
  open?.close();
  const el = h("div", { class: `ai-popover ${options.className}`, role: "dialog", ariaLabel: options.label }, content);
  let closed = false;
  const place = () => {
    const box = root.getBoundingClientRect();
    const at = anchor.getBoundingClientRect();
    const margin = 8;
    el.style.maxHeight = "";
    const height = el.offsetHeight;
    const width = el.offsetWidth;
    const above = at.top - box.top - margin;
    const below = box.bottom - at.bottom - margin;
    const up = above >= height || above >= below;
    const room = Math.max(120, (up ? above : below) - 6);
    if (height > room) el.style.maxHeight = `${room}px`;
    const shown = Math.min(height, room);
    const top = up ? at.top - box.top - 6 - shown : at.bottom - box.top + 6;
    const left = Math.min(Math.max(margin, at.left - box.left), Math.max(margin, box.width - width - margin));
    el.style.top = `${Math.max(margin, top)}px`;
    el.style.left = `${left}px`;
  };
  const outside = (event: PointerEvent) => {
    const target = event.target as Node;
    if (!el.contains(target) && !anchor.contains(target)) popover.close();
  };
  // Escape from the popover or from its button (focus may stay there).
  const onKey = (event: KeyboardEvent) => {
    if (event.key !== "Escape") return;
    const target = event.target as Node | null;
    if (target && !el.contains(target) && !anchor.contains(target) && target !== document.body) return;
    event.preventDefault();
    event.stopPropagation();
    popover.close();
    anchor.focus();
  };
  const popover: Popover = {
    el,
    place,
    close() {
      if (closed) return;
      closed = true;
      if (open === popover) open = null;
      document.removeEventListener("pointerdown", outside, true);
      document.removeEventListener("keydown", onKey, true);
      window.removeEventListener("resize", onResize);
      anchor.setAttribute("aria-expanded", "false");
      el.remove();
      options.onClose?.();
    },
  };
  const onResize = () => popover.close();
  root.appendChild(el);
  place();
  anchor.setAttribute("aria-expanded", "true");
  document.addEventListener("pointerdown", outside, true);
  document.addEventListener("keydown", onKey, true);
  window.addEventListener("resize", onResize);
  open = popover;
  return popover;
}
