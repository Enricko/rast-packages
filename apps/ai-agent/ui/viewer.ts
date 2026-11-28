// A file or web page in a tab of the panel beside the chat (panel.ts): a
// picture, video or PDF; a document (Word, spreadsheet, slides: AI Agent's
// server turns it into PDF pages, src/convert.rs); text and code (Markdown
// rendered, CSV as a table, HTML as a page, each also as source); a web
// page (in a frame where the site allows one, else a card); other files
// offer a download and File Station. view.ts says what opens and from
// where.

import type { AppContext } from "../../core/app";
import { h } from "../../core/dom";
import { iconSvg } from "../../core/icons";
import { copyText } from "../../core/ui";
import { renderMarkdown } from "./markdown";
import type { TabView } from "./panel";
import { badgeOf, folderOf, kindLabel, parseDelimited, viewKey, viewOfUrl, type ViewTarget } from "./view";

/** What a web page is (GET /api/ai/view/page). */
interface PageInfo {
  url: string;
  title: string;
  description: string;
  siteName: string;
  image: string;
  icon: string;
  contentType: string;
  frameable: boolean;
  refused: string;
}

const MAX_TEXT_BYTES = 4 * 1024 * 1024;
/** Lines numbered up to this; longer files show plain. */
const MAX_NUMBERED_LINES = 20_000;
const MAX_TABLE_ROWS = 2000;

/** Documents already turned into pages this session, by where they came from. */
const converted = new Map<string, string>();

const viewSrc = (url: string) => `/api/ai/view?url=${encodeURIComponent(url)}`;

export function createFileView(ctx: AppContext, start: ViewTarget): TabView {
  let target = start;
  let text: string | null = null;
  let truncated = false;
  let mode: "rendered" | "source" = "rendered";
  let loaded = false;
  let generation = 0;
  let pdfFor = "";

  const button = (icon: string, label: string, onClick: () => void) =>
    h("button", { class: "btn btn-ghost btn-icon btn-sm", type: "button", title: label, ariaLabel: label, onClick }, iconSvg(icon, 15));
  const link = (icon: string, label: string) => h("a", { class: "btn btn-ghost btn-icon btn-sm", title: label, ariaLabel: label }, iconSvg(icon, 15));

  const copy = button("copy", "Copy", () => {
    if (text !== null) void copyText(text).then((ok) => (ok ? ctx.toast.success("Copied.") : ctx.toast.error("The browser kept the clipboard closed.")));
  });
  const download = link("download", "Download");
  const openTab = link("external", "Open in a new tab");
  openTab.setAttribute("target", "_blank");
  openTab.setAttribute("rel", "noopener noreferrer");
  const inFiles = button("folder", "Show in File Station", () => {
    if (target.path) ctx.openApp("files", { path: folderOf(target.path) });
  });
  const renderedTab = h("button", { class: "segment", type: "button", onClick: () => setMode("rendered") }, "Preview");
  const sourceTab = h("button", { class: "segment", type: "button", onClick: () => setMode("source") }, "Source");
  const modes = h("div", { class: "segmented ai-artifact-modes", role: "group", ariaLabel: "Show" }, renderedTab, sourceTab);
  const body = h("div", { class: "ai-artifact-body ai-view-body" });

  const view: TabView = {
    key: viewKey(start),
    title: start.title,
    badge: badgeOf(start),
    where: start.from,
    label: kindLabel(start),
    tools: modes,
    actions: [copy, download, openTab, inFiles],
    body,
    spec: () => ({ t: "view", target: start }),
    shown() {
      if (!loaded) {
        loaded = true;
        void load();
      }
    },
    refresh() {
      text = null;
      pdfFor = "";
      if (start.kind === "document") converted.delete(start.src);
      target = start;
      loaded = true;
      void load();
    },
    dispose() {
      generation += 1;
      // Stops a video or page and lets go of what was loaded.
      body.replaceChildren();
    },
  };

  function head(): void {
    view.title = target.title;
    view.badge = badgeOf(target);
    view.where = target.from;
    view.label = kindLabel(target);
    copy.classList.toggle("hidden", target.kind !== "text");
    download.classList.toggle("hidden", !target.download);
    if (target.download) {
      download.setAttribute("href", target.download);
      download.setAttribute("download", target.title);
    }
    const tab = openAddress();
    openTab.classList.toggle("hidden", !tab);
    if (tab) openTab.setAttribute("href", tab);
    inFiles.classList.toggle("hidden", !target.path);
    const twoWays =
      target.kind === "text" &&
      (target.format === "markdown" || target.format === "csv" || target.format === "tsv" || (target.format === "html" && !!target.path));
    modes.classList.toggle("hidden", !twoWays);
    renderedTab.textContent = target.format === "csv" || target.format === "tsv" ? "Table" : "Preview";
    renderedTab.classList.toggle("is-active", mode === "rendered");
    renderedTab.setAttribute("aria-pressed", String(mode === "rendered"));
    sourceTab.classList.toggle("is-active", mode === "source");
    sourceTab.setAttribute("aria-pressed", String(mode === "source"));
    view.changed?.();
  }

  /** Where Open in a new tab goes. */
  function openAddress(): string | null {
    if (target.external) return target.external;
    if (target.kind === "document") return pdfFor ? `/api/ai/view/converted/${pdfFor}` : null;
    if (target.path && (target.kind === "text" || target.kind === "image" || target.kind === "pdf")) {
      return `/api/files/preview?path=${encodeURIComponent(target.path)}`;
    }
    if (target.kind === "other") return null;
    return target.src;
  }

  function setMode(next: "rendered" | "source"): void {
    mode = next;
    head();
    if (text !== null) showText();
  }

  function note(reason: string, hint?: string): void {
    body.replaceChildren(
      h(
        "div",
        { class: "ai-view-note" },
        iconSvg("warning", 18),
        h("p", {}, reason),
        h("p", { class: "small muted" }, hint ?? (target.external ? "Open it in a new tab, or download it." : "Download it to open it on this device.")),
      ),
    );
  }

  function loading(what: string): void {
    body.replaceChildren(h("div", { class: "ai-view-loading small muted" }, h("span", { class: "spinner", ariaHidden: true }), h("span", {}, what)));
  }

  /** A file in the person's files: there, and theirs to open? */
  async function reachable(): Promise<boolean> {
    if (!target.path) return true;
    try {
      await ctx.api.get(`/api/files/stat?path=${encodeURIComponent(target.path)}`);
      return true;
    } catch (error) {
      note(missing(error));
      return false;
    }
  }

  function missing(error: unknown): string {
    const status = (error as { status?: number }).status;
    return status === 404
      ? `${target.path} is not in your files (it may have been moved or deleted).`
      : status === 403
        ? `Your account has no access to ${target.path}.`
        : error instanceof Error && error.message
          ? error.message
          : `${target.title} could not be opened.`;
  }

  /** An error answer's message (JSON `{error:{message}}` or text). */
  async function said(response: Response): Promise<string> {
    const raw = await response.text().catch(() => "");
    try {
      const parsed = JSON.parse(raw) as { error?: { message?: string } | string; message?: string };
      if (typeof parsed.error === "object" && parsed.error?.message) return parsed.error.message;
      if (typeof parsed.error === "string") return parsed.error;
      if (parsed.message) return parsed.message;
    } catch {
      /* not JSON */
    }
    return raw.trim().slice(0, 300) || `HTTP ${response.status}`;
  }

  async function load(): Promise<void> {
    const mine = ++generation;
    const current = () => mine === generation;
    head();
    loading(target.kind === "document" ? "Turning it into pages…" : target.kind === "website" ? "Opening the page…" : "Loading…");
    switch (target.kind) {
      case "website":
        await loadPage(current);
        return;
      case "text":
        await loadText(current);
        return;
      case "document":
        if (!(await reachable()) || !current()) return;
        await loadDocument(current);
        return;
      default:
        if (!(await reachable()) || !current()) return;
        body.replaceChildren(target.kind === "other" ? otherCard() : media());
    }
  }

  // ------------------------------------------------------------ media

  function media(): HTMLElement {
    body.classList.remove("is-actual-size");
    if (target.kind === "image") {
      const image = document.createElement("img");
      image.className = "ai-view-image";
      image.alt = target.title;
      image.decoding = "async";
      image.addEventListener("error", () => note(`${target.title} could not be shown as a picture.`));
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
      video.addEventListener("error", () => note(`${target.title} could not be played here.`));
      video.src = target.src;
      return h("div", { class: "ai-view-stage" }, video);
    }
    return pdfFrame(target.src);
  }

  function pdfFrame(src: string): HTMLElement {
    const frame = document.createElement("iframe");
    frame.className = "ai-view-frame";
    frame.title = target.title;
    // The browser's own PDF viewer. Each PDF comes with a `sandbox` CSP, a
    // PDF type and nosniff from the server (RAST's previews, chat files,
    // AI Agent's viewer and converter), so nothing in it runs as this page.
    // A `sandbox` attribute here would stop Chrome from showing it at all.
    frame.setAttribute("referrerpolicy", "no-referrer");
    frame.src = src;
    return frame;
  }

  function otherCard(): HTMLElement {
    const badge = badgeOf(target);
    return h(
      "div",
      { class: "ai-view-note" },
      h("span", { class: `ai-badge is-big is-${badge.tone}`, ariaHidden: true }, badge.text),
      h("p", {}, target.title),
      h("p", { class: "small muted" }, "The panel shows pictures, videos, PDFs, documents, text and code. This file downloads, or opens in File Station."),
      h(
        "div",
        { class: "ai-view-buttons" },
        target.download ? h("a", { class: "btn btn-sm", href: target.download, download: target.title }, iconSvg("download", 14), "Download") : null,
        target.path
          ? h("button", { class: "btn btn-sm", type: "button", onClick: () => ctx.openApp("files", { path: folderOf(target.path!) }) }, iconSvg("folder", 14), "Show in File Station")
          : null,
      ),
    );
  }

  // ------------------------------------------------------------- text

  async function loadText(current: () => boolean): Promise<void> {
    try {
      if (target.path) {
        const preview = await ctx.api.get<{ content: string; truncated: boolean }>(
          `/api/files/text?path=${encodeURIComponent(target.path)}&maxBytes=${MAX_TEXT_BYTES}`,
        );
        if (!current()) return;
        text = preview.content;
        truncated = preview.truncated;
      } else {
        const response = await fetch(target.src, { credentials: "same-origin" });
        if (!response.ok) throw new Error(await said(response));
        const raw = await response.text();
        if (!current()) return;
        truncated = raw.length >= MAX_TEXT_BYTES;
        text = raw;
      }
    } catch (error) {
      if (current()) note(target.path ? missing(error) : error instanceof Error ? error.message : String(error));
      return;
    }
    showText();
  }

  function showText(): void {
    if (text === null) return;
    const content = text;
    const rendered = mode === "rendered";
    const parts: HTMLElement[] = [];
    if (target.format === "markdown" && rendered) {
      const doc = h("div", { class: "ai-md ai-artifact-doc" });
      renderMarkdown(doc, content, {
        fullHeadings: true,
        onCopy: (code) => void copyText(code).then((ok) => ok && ctx.toast.success("Code copied.")),
      });
      parts.push(doc);
    } else if ((target.format === "csv" || target.format === "tsv") && rendered) {
      parts.push(table(content, target.format === "tsv" ? "\t" : ","));
    } else if (target.format === "html" && target.path && rendered) {
      const frame = document.createElement("iframe");
      frame.className = "ai-artifact-frame";
      frame.title = target.title;
      // Like an artifact: its scripts run in an opaque origin, without the
      // session or RAST's API (the server's CSP says the same).
      frame.setAttribute("sandbox", "allow-scripts allow-forms allow-modals allow-popups allow-downloads");
      frame.setAttribute("referrerpolicy", "no-referrer");
      frame.src = `/api/ai/view/html?path=${encodeURIComponent(target.path)}`;
      body.replaceChildren(frame);
      return;
    } else {
      let shown = content;
      if (target.format === "json") {
        try {
          shown = JSON.stringify(JSON.parse(content), null, 2);
        } catch {
          /* JSON Lines or unfinished: as written */
        }
      }
      parts.push(codeBlock(shown));
    }
    if (truncated) parts.push(h("p", { class: "ai-view-more small muted" }, "This shows the first 4 MB. Download the file for the rest."));
    body.replaceChildren(...parts);
  }

  function codeBlock(content: string): HTMLElement {
    const lines = content.split("\n");
    if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
    const code = h("code", {});
    if (lines.length <= MAX_NUMBERED_LINES) {
      const numbers = h("div", { class: "ai-code-lines", ariaHidden: true });
      numbers.textContent = lines.map((_, i) => String(i + 1)).join("\n");
      code.textContent = lines.join("\n");
      return h("div", { class: "ai-code-view" }, numbers, h("pre", { class: "ai-artifact-code" }, code));
    }
    code.textContent = content;
    return h("pre", { class: "ai-artifact-code" }, code);
  }

  function table(content: string, separator: "," | "\t"): HTMLElement {
    const rows = parseDelimited(content, separator, MAX_TABLE_ROWS + 1);
    const more = rows.length > MAX_TABLE_ROWS;
    const [first, ...rest] = rows.slice(0, MAX_TABLE_ROWS);
    const cells = (row: string[], tag: "th" | "td") => h("tr", {}, ...row.map((cell) => h(tag, {}, cell)));
    return h(
      "div",
      { class: "ai-view-table-wrap" },
      h("table", { class: "ai-view-table" }, first ? h("thead", {}, cells(first, "th")) : null, h("tbody", {}, ...rest.map((row) => cells(row, "td")))),
      more ? h("p", { class: "ai-view-more small muted" }, "This shows the first 2,000 rows; Source has them all.") : null,
    );
  }

  // --------------------------------------------------------- documents

  async function loadDocument(current: () => boolean): Promise<void> {
    let id = converted.get(target.src);
    if (!id) {
      try {
        const response = await fetch(target.src, { credentials: "same-origin" });
        if (!response.ok) throw new Error(await said(response));
        const bytes = await response.blob();
        if (!current()) return;
        const answer = await ctx.api.uploadRaw<{ id: string }>(`/api/ai/view/convert?name=${encodeURIComponent(target.title)}`, bytes, {
          contentType: "application/octet-stream",
        });
        id = answer.id;
        converted.set(target.src, id);
      } catch (error) {
        if (!current()) return;
        note((error instanceof Error && error.message) || `${target.title} could not be turned into pages.`);
        return;
      }
    }
    if (!current()) return;
    pdfFor = id;
    head();
    body.replaceChildren(pdfFrame(`/api/ai/view/converted/${id}`));
  }

  // ------------------------------------------------------------- pages

  async function loadPage(current: () => boolean): Promise<void> {
    const address = target.external ?? target.src;
    let info: PageInfo;
    try {
      info = await ctx.api.get<PageInfo>(`/api/ai/view/page?url=${encodeURIComponent(address)}`);
    } catch (error) {
      if (current()) note(error instanceof Error ? error.message : String(error), "Open it in a new tab.");
      return;
    }
    if (!current()) return;
    // An address that turns out to be a picture, video, PDF, text or a
    // document opens as one.
    const type = info.contentType;
    if (type && type !== "text/html" && type !== "application/xhtml+xml") {
      const as = mediaTargetOf(info);
      if (as) {
        target = as;
        await load();
        return;
      }
    }
    const page = info.url || address;
    let host = page;
    try {
      host = new URL(page).host;
    } catch {
      /* as given */
    }
    target = { ...target, title: info.title || host, from: page, external: page };
    view.badgeImage = info.icon ? viewSrc(info.icon) : undefined;
    head();
    const sameSite = (() => {
      try {
        return new URL(page).origin === window.location.origin;
      } catch {
        return true;
      }
    })();
    const mixed = window.location.protocol === "https:" && page.startsWith("http:");
    if (info.frameable && !sameSite && !mixed) {
      const frame = document.createElement("iframe");
      frame.className = "ai-view-frame ai-view-site";
      frame.title = target.title;
      // The site keeps its own origin (another site's, so none of RAST's)
      // and may not move this page.
      frame.setAttribute(
        "sandbox",
        "allow-scripts allow-same-origin allow-forms allow-popups allow-popups-to-escape-sandbox allow-downloads allow-modals",
      );
      frame.setAttribute("referrerpolicy", "no-referrer");
      frame.setAttribute("allow", "fullscreen");
      frame.src = page;
      body.replaceChildren(frame);
      return;
    }
    const why = sameSite
      ? "this address is RAST itself, which opens in its own tab"
      : mixed
        ? "the page is served without HTTPS, so it opens in its own tab"
        : info.refused;
    body.replaceChildren(pageCard(info, host, why));
  }

  function mediaTargetOf(info: PageInfo): ViewTarget | null {
    const url = info.url;
    const type = info.contentType;
    let name = url;
    try {
      const parsed = new URL(url);
      name = decodeURIComponent(parsed.pathname.split("/").filter(Boolean).pop() ?? "") || parsed.host;
    } catch {
      /* as given */
    }
    const base = { title: name, from: url, external: url };
    if (type.startsWith("image/")) return { ...base, kind: "image", src: viewSrc(url), download: viewSrc(url) };
    if (type.startsWith("video/")) return { ...base, kind: "video", src: viewSrc(url), download: viewSrc(url) };
    if (type === "application/pdf") return { ...base, kind: "pdf", src: viewSrc(url), download: viewSrc(url) };
    const known = viewOfUrl(url);
    if (type.startsWith("text/") || type === "application/json") {
      const format = known?.kind === "text" ? known.format : type === "application/json" ? "json" : type === "text/markdown" ? "markdown" : "plain";
      return { ...base, kind: "text", format, src: `${viewSrc(url)}&as=text`, download: `${viewSrc(url)}&as=text` };
    }
    return known?.kind === "document" ? known : null;
  }

  function pageCard(info: PageInfo, host: string, why: string): HTMLElement {
    let picture: HTMLElement | null = null;
    if (info.image) {
      const img = document.createElement("img");
      img.className = "ai-page-image";
      img.alt = "";
      img.decoding = "async";
      img.addEventListener("error", () => img.remove());
      img.src = viewSrc(info.image);
      picture = img;
    }
    let icon: Element = iconSvg("browser", 14);
    if (info.icon) {
      const img = document.createElement("img");
      img.className = "ai-page-icon";
      img.alt = "";
      img.addEventListener("error", () => img.replaceWith(iconSvg("browser", 14)));
      img.src = viewSrc(info.icon);
      icon = img;
    }
    return h(
      "div",
      { class: "ai-page-card-wrap" },
      h(
        "article",
        { class: "ai-page-card" },
        picture,
        h(
          "div",
          { class: "ai-page-text" },
          h("div", { class: "ai-page-site small muted" }, icon, h("span", {}, info.siteName || host)),
          h("h3", { class: "ai-page-title" }, info.title || host),
          info.description ? h("p", { class: "ai-page-description" }, info.description) : null,
          h(
            "a",
            { class: "btn btn-primary btn-sm ai-page-open", href: target.external, target: "_blank", rel: "noopener noreferrer" },
            iconSvg("external", 14),
            "Open in a new tab",
          ),
          why ? h("p", { class: "ai-page-why small faint" }, `Shown as a card: ${why}.`) : null,
        ),
      ),
    );
  }

  head();
  return view;
}
