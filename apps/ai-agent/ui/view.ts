// What the viewer beside the chat opens (viewer.ts): images, videos and PDFs
// from links in answers (through the server, `GET /api/ai/view`), from the
// person's files (RAST's File Station preview, with their own access), and
// files sent in the chat.

import { fileUrl, type FileBlock } from "./model";

export type ViewKind = "image" | "video" | "pdf";

const KINDS: Record<string, ViewKind> = {
  png: "image",
  jpg: "image",
  jpeg: "image",
  gif: "image",
  webp: "image",
  avif: "image",
  bmp: "image",
  svg: "image",
  ico: "image",
  mp4: "video",
  m4v: "video",
  webm: "video",
  ogv: "video",
  mov: "video",
  pdf: "pdf",
};

export interface ViewTarget {
  kind: ViewKind;
  /** The file's name. */
  title: string;
  /** Where it is from (the address or path), under the title. */
  from: string;
  /** What the viewer loads. */
  src: string;
  /** A download of it, when there is one. */
  download?: string;
  /** The page on the web it is (Open in a new tab). */
  external?: string;
  /** A path in the person's files (Show in File Station). */
  path?: string;
}

/** The viewer's kind of a file name, path or address (by its extension). */
export function viewKindOf(name: string): ViewKind | null {
  const bare = name.split(/[?#]/)[0] ?? "";
  const last = bare.slice(bare.lastIndexOf("/") + 1);
  const dot = last.lastIndexOf(".");
  if (dot < 1) return null;
  return KINDS[last.slice(dot + 1).toLowerCase()] ?? null;
}

function lastSegment(path: string): string {
  const bare = (path.split(/[?#]/)[0] ?? "").replace(/\/+$/, "");
  const last = bare.slice(bare.lastIndexOf("/") + 1);
  try {
    return decodeURIComponent(last) || path;
  } catch {
    return last || path;
  }
}

/** A path in the person's files: `/` is their home, `/shared/<name>` a shared folder. */
export function looksLikePath(text: string): boolean {
  return /^\/(?!\/)[^\0\n<>"|]*[^\s/]$/.test(text) && !text.startsWith("/api/");
}

/** An image, video or PDF on the web. */
export function viewOfUrl(url: string): ViewTarget | null {
  if (!/^https?:\/\/[^\s/]+/i.test(url)) return null;
  const kind = viewKindOf(url);
  if (!kind) return null;
  const src = `/api/ai/view?url=${encodeURIComponent(url)}`;
  return { kind, title: lastSegment(url), from: url, src, download: src, external: url };
}

/** An image, video or PDF in the person's files. */
export function viewOfPath(path: string): ViewTarget | null {
  if (!looksLikePath(path)) return null;
  const kind = viewKindOf(path);
  if (!kind) return null;
  const download = `/api/files/download?path=${encodeURIComponent(path)}`;
  // RAST previews pictures and PDFs inline; a video plays from its download.
  const src = kind === "video" ? download : `/api/files/preview?path=${encodeURIComponent(path)}`;
  return { kind, title: lastSegment(path), from: path, src, download, path };
}

/** A picture or PDF sent in the chat. */
export function viewOfChatFile(file: Pick<FileBlock, "id" | "name" | "kind">): ViewTarget | null {
  const kind: ViewKind | null = file.kind === "image" ? "image" : file.kind === "pdf" ? "pdf" : null;
  if (!kind) return null;
  const src = fileUrl(file.id);
  return { kind, title: file.name, from: "Sent in this chat", src, download: src };
}

/** The folder a path is in ("/" for the home folder's files). */
export function folderOf(path: string): string {
  const at = path.replace(/\/+$/, "").lastIndexOf("/");
  return at <= 0 ? "/" : path.slice(0, at);
}
