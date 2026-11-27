// The viewer beside the chat: an image, video or PDF from the chat (a link
// in an answer, a file the agent named or wrote, a file sent in the chat),
// shown without leaving the conversation. It takes the artifact panel's
// place (view.ts says what opens and from where).

import type { AppContext } from "../../core/app";
import { h } from "../../core/dom";
import { iconSvg } from "../../core/icons";
import { folderOf, type ViewTarget } from "./view";

export interface ViewerPane {
  el: HTMLElement;
  show(target: ViewTarget): void;
  close(): void;
  /** What is shown, or null when closed. */
  shown(): ViewTarget | null;
}

const KIND_LABEL: Record<ViewTarget["kind"], string> = { image: "Image", video: "Video", pdf: "PDF" };

export function createViewerPane(ctx: AppContext, onClose: () => void): ViewerPane {
  let shown: ViewTarget | null = null;

  const title = h("div", { class: "ai-artifact-title" });
  const sub = h("div", { class: "ai-artifact-sub small muted" });
  const button = (icon: string, label: string, onClick: () => void) =>
    h("button", { class: "btn btn-ghost btn-icon btn-sm", type: "button", title: label, ariaLabel: label, onClick }, iconSvg(icon, 15));
  const link = (icon: string, label: string) =>
    h("a", { class: "btn btn-ghost btn-icon btn-sm", title: label, ariaLabel: label }, iconSvg(icon, 15));
  const download = link("download", "Download");
  const openTab = link("external", "Open in a new tab");
  openTab.setAttribute("target", "_blank");
  openTab.setAttribute("rel", "noopener noreferrer");
  const inFiles = button("folder", "Show in File Station", () => {
    if (shown?.path) ctx.openApp("files", { path: folderOf(shown.path) });
  });
  const actions = h("div", { class: "ai-artifact-actions" }, download, openTab, inFiles, button("close", "Close", () => onClose()));
  const head = h("header", { class: "ai-artifact-head" }, h("div", { class: "ai-artifact-titles" }, title, sub), actions);
  const body = h("div", { class: "ai-artifact-body ai-view-body" });
  const el = h("aside", { class: "ai-artifact ai-viewer", ariaLabel: "Viewer" }, head, body);
  el.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && shown) {
      event.preventDefault();
      event.stopPropagation();
      onClose();
    }
  });

  function failed(target: ViewTarget, reason: string): void {
    if (shown !== target) return;
    body.replaceChildren(
      h(
        "div",
        { class: "ai-view-note" },
        iconSvg("warning", 18),
        h("p", {}, reason),
        h("p", { class: "small muted" }, target.external ? "Open it in a new tab, or download it." : "Download it to open it on this device."),
      ),
    );
  }

  /** A file in the person's files: there, and theirs to open? */
  async function reachable(target: ViewTarget): Promise<boolean> {
    if (!target.path) return true;
    try {
      await ctx.api.get(`/api/files/stat?path=${encodeURIComponent(target.path)}`);
      return true;
    } catch (error) {
      const status = (error as { status?: number }).status;
      failed(
        target,
        status === 404
          ? `${target.path} is not in your files (it may have been moved or deleted).`
          : status === 403
            ? `Your account has no access to ${target.path}.`
            : `${target.path} could not be opened.`,
      );
      return false;
    }
  }

  function render(target: ViewTarget): void {
    title.textContent = target.title;
    title.title = target.title;
    sub.textContent = `${KIND_LABEL[target.kind]} · ${target.from}`;
    sub.title = target.from;
    download.classList.toggle("hidden", !target.download);
    if (target.download) {
      download.setAttribute("href", target.download);
      download.setAttribute("download", target.title);
    }
    const tab = target.external ?? target.src;
    openTab.setAttribute("href", tab);
    inFiles.classList.toggle("hidden", !target.path);
    body.replaceChildren(h("div", { class: "ai-view-loading small muted" }, "Loading…"));
    void reachable(target).then((ok) => {
      if (!ok || shown !== target) return;
      body.replaceChildren(media(target));
    });
  }

  function media(target: ViewTarget): HTMLElement {
    if (target.kind === "image") {
      const image = document.createElement("img");
      image.className = "ai-view-image";
      image.alt = target.title;
      image.decoding = "async";
      image.addEventListener("error", () => failed(target, `${target.title} could not be shown as a picture.`));
      // A click shows it at full size (scrolling), another fits it again.
      image.addEventListener("click", () => body.classList.toggle("is-actual-size"));
      image.src = target.src;
      return h("div", { class: "ai-view-stage" }, image);
    }
    if (target.kind === "video") {
      const video = document.createElement("video");
      video.className = "ai-view-video";
      video.controls = true;
      video.preload = "metadata";
      video.addEventListener("error", () => failed(target, `${target.title} could not be played here.`));
      video.src = target.src;
      return h("div", { class: "ai-view-stage" }, video);
    }
    const frame = document.createElement("iframe");
    frame.className = "ai-view-frame";
    frame.title = target.title;
    // The browser's own PDF viewer. Each PDF comes with a `sandbox` CSP, a
    // PDF type and nosniff from the server (RAST's previews, chat files,
    // /api/ai/view), so nothing in it runs as this page. A `sandbox`
    // attribute here would stop Chrome from showing it at all.
    frame.setAttribute("referrerpolicy", "no-referrer");
    frame.src = target.src;
    return frame;
  }

  return {
    el,
    show(target) {
      shown = target;
      body.classList.remove("is-actual-size");
      render(target);
    },
    close() {
      shown = null;
      // Stops a video and lets go of what was loaded.
      body.replaceChildren();
    },
    shown: () => shown,
  };
}
