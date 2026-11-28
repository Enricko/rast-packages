// AI Agent artifacts: what the model showed with the `artifact` tool, as a
// card in the chat and a tab of the panel beside it (panel.ts). Web pages and SVG pictures show in
// a sandboxed frame the server serves (scripts run, but in an opaque origin
// without RAST's session or network: see src/api/ai.rs `get_artifact`);
// documents render as Markdown; code shows as text. The panel has every
// version of the artifact, Preview / Code, Copy, Download and (pages) Open
// in a new tab.

import type { AppContext } from "../../core/app";
import { h } from "../../core/dom";
import { iconSvg } from "../../core/icons";
import { copyText } from "../../core/ui";
import { renderMarkdown } from "./markdown";
import { ARTIFACT_KINDS, artifactFileName, artifactMime, type ArtifactVersion, type ToolBlock } from "./model";
import type { TabView } from "./panel";

type Mode = "preview" | "code";

export function artifactUrl(conversationId: string, callId: string): string {
  return `/api/ai/conversations/${encodeURIComponent(conversationId)}/artifacts/${encodeURIComponent(callId)}`;
}

/** Whether the kind shows as a page in a frame (else in the app). */
const framed = (artifact: ArtifactVersion) => artifact.kind === "html" || artifact.kind === "svg";

/** The tab of an artifact: its versions share it. */
export const artifactKey = (artifact: ArtifactVersion) => `a:${artifact.key}`;

export interface ArtifactView extends TabView {
  /** The chat's artifacts changed: show `callId` (or keep the shown one current). */
  update(list: ArtifactVersion[], callId?: string): void;
}

/** An artifact in a tab of the panel: every version of it, Preview /
 * Code, Copy, Download and (pages) Open in a new tab. */
export function createArtifactView(ctx: AppContext, all: ArtifactVersion[], callId: string, conversationId: string): ArtifactView | null {
  let list = all;
  let shown = list.find((a) => a.callId === callId) ?? null;
  if (!shown) return null;
  let mode: Mode = "preview";
  let frameFor = "";

  const button = (icon: string, label: string, onClick: () => void) =>
    h("button", { class: "btn btn-ghost btn-icon btn-sm", type: "button", title: label, ariaLabel: label, onClick }, iconSvg(icon, 15));
  const older = button("chevronLeft", "Earlier version", () => step(-1));
  const newer = button("chevronRight", "Later version", () => step(1));
  const versions = h("div", { class: "ai-artifact-versions" }, older, newer);
  const previewTab = h("button", { class: "segment", type: "button", onClick: () => setMode("preview") }, "Preview");
  const codeTab = h("button", { class: "segment", type: "button", onClick: () => setMode("code") }, "Code");
  const modes = h("div", { class: "segmented ai-artifact-modes", role: "group", ariaLabel: "Show" }, previewTab, codeTab);
  const openTab = h(
    "a",
    { class: "btn btn-ghost btn-icon btn-sm", target: "_blank", rel: "noopener", title: "Open in a new tab", ariaLabel: "Open in a new tab" },
    iconSvg("external", 15),
  );
  const body = h("div", { class: "ai-artifact-body" });

  const view: ArtifactView = {
    key: artifactKey(shown),
    title: shown.title,
    badge: { text: "", tone: "violet", icon: ARTIFACT_KINDS[shown.kind].icon },
    where: shown.title,
    label: "",
    tools: h("div", { class: "ai-panel-tools-row" }, versions, modes),
    actions: [button("copy", "Copy", () => void copy()), button("download", "Download", () => download()), openTab],
    body,
    spec: () => ({ t: "artifact", callId: shown!.callId }),
    refresh: () => {
      frameFor = "";
      render();
    },
    dispose: () => {
      body.replaceChildren();
    },
    update(next, call) {
      list = next;
      const found = call ? list.find((a) => a.callId === call) : list.find((a) => a.callId === shown?.callId);
      if (found) shown = found;
      render();
    },
  };

  function siblings(): ArtifactVersion[] {
    return shown ? list.filter((a) => a.key === shown!.key) : [];
  }

  function step(by: number): void {
    const versionsOf = siblings();
    const at = versionsOf.findIndex((a) => a.callId === shown?.callId);
    const next = versionsOf[at + by];
    if (next) {
      shown = next;
      render();
    }
  }

  function setMode(next: Mode): void {
    mode = next;
    render();
  }

  async function copy(): Promise<void> {
    if (!shown) return;
    if (await copyText(shown.content)) ctx.toast.success("Copied.");
    else ctx.toast.error("The browser kept the clipboard closed; use Download instead.");
  }

  function download(): void {
    if (!shown) return;
    const url = URL.createObjectURL(new Blob([shown.content], { type: `${artifactMime(shown.kind)};charset=utf-8` }));
    const link = h("a", { href: url, download: artifactFileName(shown) });
    document.body.appendChild(link);
    link.click();
    link.remove();
    window.setTimeout(() => URL.revokeObjectURL(url), 30_000);
  }

  function render(): void {
    if (!shown) return;
    const artifact = shown;
    const versionsOf = siblings();
    const at = versionsOf.findIndex((a) => a.callId === artifact.callId);
    const kind = ARTIFACT_KINDS[artifact.kind];
    const language = artifact.kind === "code" && artifact.language ? ` · ${artifact.language}` : "";
    view.title = artifact.title;
    view.where = artifact.title;
    view.label = versionsOf.length > 1 ? `${kind.label}${language} · Version ${at + 1} of ${versionsOf.length}` : `${kind.label}${language}`;
    view.badge = { text: "", tone: "violet", icon: kind.icon };
    view.changed?.();
    versions.classList.toggle("hidden", versionsOf.length < 2);
    older.toggleAttribute("disabled", at <= 0);
    newer.toggleAttribute("disabled", at >= versionsOf.length - 1);
    const hasPreview = artifact.kind !== "code";
    modes.classList.toggle("hidden", !hasPreview);
    const show: Mode = hasPreview ? mode : "code";
    previewTab.classList.toggle("is-active", show === "preview");
    previewTab.setAttribute("aria-pressed", String(show === "preview"));
    codeTab.classList.toggle("is-active", show === "code");
    codeTab.setAttribute("aria-pressed", String(show === "code"));
    openTab.classList.toggle("hidden", !framed(artifact));
    if (framed(artifact)) openTab.setAttribute("href", artifactUrl(conversationId, artifact.callId));
    if (show === "preview" && framed(artifact)) {
      // The same version keeps its frame (and what was done in it).
      if (frameFor === artifact.callId && body.querySelector("iframe")) return;
      const frame = document.createElement("iframe");
      frame.className = "ai-artifact-frame";
      frame.title = artifact.title;
      frame.setAttribute("sandbox", "allow-scripts allow-forms allow-modals allow-popups allow-downloads");
      frame.setAttribute("referrerpolicy", "no-referrer");
      frame.src = artifactUrl(conversationId, artifact.callId);
      frameFor = artifact.callId;
      body.replaceChildren(frame);
      return;
    }
    frameFor = "";
    if (show === "preview" && artifact.kind === "markdown") {
      const doc = h("div", { class: "ai-md ai-artifact-doc" });
      renderMarkdown(doc, artifact.content, {
        fullHeadings: true,
        onCopy: (code) => void copyText(code).then((ok) => ok && ctx.toast.success("Code copied.")),
      });
      body.replaceChildren(doc);
      return;
    }
    const code = h("code", {});
    code.textContent = artifact.content;
    body.replaceChildren(h("pre", { class: "ai-artifact-code" }, code));
  }

  render();
  return view;
}

/** The card an artifact call shows in the chat. */
export function artifactCard(block: ToolBlock, artifact: ArtifactVersion | null, onOpen: (callId: string) => void): HTMLElement {
  const input = typeof block.input === "object" && block.input ? block.input : {};
  const kindName = typeof input.kind === "string" ? input.kind.toLowerCase() : "";
  const kind = ARTIFACT_KINDS[kindName as keyof typeof ARTIFACT_KINDS] ?? { label: "Artifact", icon: "compose" };
  const name = artifact?.title ?? (typeof input.title === "string" && input.title.trim() ? input.title.trim() : "Artifact");
  if (block.status === "error" || !artifact) {
    const note = block.status === "error" ? block.error || "It could not be shown." : "Writing…";
    return h(
      "div",
      { class: `ai-artifact-card is-${block.status}`, dataset: { call: block.id } },
      h("span", { class: "ai-artifact-card-icon" }, iconSvg(kind.icon, 18)),
      h("span", { class: "ai-artifact-card-text" }, h("span", { class: "ai-artifact-card-title" }, name), h("span", { class: "ai-artifact-card-sub small" }, note)),
    );
  }
  const detail = artifact.version > 1 ? `${kind.label} · Version ${artifact.version}` : kind.label;
  return h(
    "button",
    { class: "ai-artifact-card", type: "button", dataset: { call: block.id }, title: `Open ${artifact.title}`, onClick: () => onOpen(artifact.callId) },
    h("span", { class: "ai-artifact-card-icon" }, iconSvg(kind.icon, 18)),
    h("span", { class: "ai-artifact-card-text" }, h("span", { class: "ai-artifact-card-title" }, artifact.title), h("span", { class: "ai-artifact-card-sub small" }, detail)),
    h("span", { class: "ai-artifact-card-open small" }, "Open"),
  );
}
