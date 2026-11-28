// What the panel beside the chat opens (panel.ts, viewer.ts), and from
// where: pictures, videos, PDFs, documents, text and code, and web pages.
//
// * A path in the person's files opens with their own access through RAST
//   (its previews and downloads; a document's bytes go to AI Agent's
//   converter, an HTML file shows through /api/ai/view/html).
// * An address on the web comes through AI Agent's server
//   (`/api/ai/view?url=&as=`), public addresses only; a web page opens in a
//   frame where the site allows it, else as a card.
// * Files sent in the chat come from /api/ai/files/{id}.

import { fileUrl, type FileBlock } from "./model";

export type ViewKind = "image" | "video" | "pdf" | "document" | "text" | "website" | "other";

/** How text shows: rendered (Markdown, a table, a page) or as code. */
export type TextFormat = "markdown" | "csv" | "tsv" | "json" | "html" | "code" | "plain";

/** The LibreOffice part a document needs (for its badge). */
export type DocumentFamily = "word" | "sheet" | "slides";

interface KindInfo {
  kind: ViewKind;
  format?: TextFormat;
  family?: DocumentFamily;
}

const KINDS: Record<string, KindInfo> = {};
const add = (info: KindInfo, ...extensions: string[]) => extensions.forEach((e) => (KINDS[e] = info));
add({ kind: "image" }, "png", "jpg", "jpeg", "gif", "webp", "avif", "bmp", "svg", "ico");
add({ kind: "video" }, "mp4", "m4v", "webm", "ogv", "mov");
add({ kind: "pdf" }, "pdf");
add({ kind: "document", family: "word" }, "docx", "docm", "dotx", "doc", "dot", "odt", "ott", "rtf");
add({ kind: "document", family: "sheet" }, "xlsx", "xlsm", "xls", "ods", "ots");
add({ kind: "document", family: "slides" }, "pptx", "pptm", "ppsx", "ppt", "pps", "odp", "otp");
add({ kind: "text", format: "markdown" }, "md", "markdown", "mdx");
add({ kind: "text", format: "csv" }, "csv");
add({ kind: "text", format: "tsv" }, "tsv");
add({ kind: "text", format: "json" }, "json", "jsonl", "ndjson", "geojson");
add({ kind: "text", format: "html" }, "html", "htm");
add({ kind: "text", format: "plain" }, "txt", "text", "log");
add(
  { kind: "text", format: "code" },
  "xml", "yaml", "yml", "toml", "ini", "cfg", "conf", "properties",
  "css", "scss", "sass", "less",
  "js", "mjs", "cjs", "jsx", "ts", "tsx", "vue", "svelte",
  "py", "rb", "php", "pl", "lua", "r", "jl",
  "rs", "go", "java", "kt", "kts", "scala", "swift", "dart", "c", "h", "cc", "cpp", "hpp", "cs", "m",
  "sh", "bash", "zsh", "fish", "ps1", "bat",
  "sql", "graphql", "proto", "gradle", "cmake", "dockerfile", "tf", "nix", "diff", "patch",
);

export interface ViewTarget {
  kind: ViewKind;
  /** The file's or page's name. */
  title: string;
  /** Where it is from (the address or path), for the path bar. */
  from: string;
  /** What the viewer loads (for text and documents: where the bytes are). */
  src: string;
  /** How text shows. */
  format?: TextFormat;
  family?: DocumentFamily;
  /** A download of it, when there is one. */
  download?: string;
  /** The page on the web it is (Open in a new tab). */
  external?: string;
  /** A path in the person's files (Show in File Station). */
  path?: string;
  /** A file sent in the chat. */
  file?: string;
}

/** The extension of a name, path or address, lower case ("" for none). */
export function extensionOf(name: string): string {
  const bare = name.split(/[?#]/)[0] ?? "";
  const last = bare.slice(bare.lastIndexOf("/") + 1);
  const dot = last.lastIndexOf(".");
  if (dot < 1) return last.toLowerCase() === "dockerfile" ? "dockerfile" : "";
  return last.slice(dot + 1).toLowerCase();
}

function infoOf(name: string): KindInfo | null {
  return KINDS[extensionOf(name)] ?? null;
}

/** The viewer's kind of a file name, path or address (by its extension):
 * the kinds it shows inside the panel, or null. */
export function viewKindOf(name: string): ViewKind | null {
  return infoOf(name)?.kind ?? null;
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

/** A web address (http or https, with a host). */
export function isWebUrl(text: string): boolean {
  return /^https?:\/\/[^\s/?#]+/i.test(text);
}

const viewUrl = (url: string, as?: "text" | "document") =>
  `/api/ai/view?url=${encodeURIComponent(url)}${as ? `&as=${as}` : ""}`;

/** Something on the web: a picture, video, PDF, document or text file by
 * its extension, else a web page. */
export function viewOfUrl(url: string): ViewTarget | null {
  if (!isWebUrl(url)) return null;
  const info = infoOf(url);
  const name = lastSegment(url);
  if (!info || info.format === "html") {
    let host = url;
    try {
      host = new URL(url).host;
    } catch {
      return null;
    }
    return { kind: "website", title: host, from: url, src: url, external: url };
  }
  const base = { title: name, from: url, external: url };
  switch (info.kind) {
    case "text":
      return { ...base, kind: "text", format: info.format, src: viewUrl(url, "text"), download: viewUrl(url, "text") };
    case "document":
      return { ...base, kind: "document", family: info.family, src: viewUrl(url, "document"), download: viewUrl(url, "document") };
    default:
      return { ...base, kind: info.kind, src: viewUrl(url), download: viewUrl(url) };
  }
}

/** A file in the person's files (any kind: those the panel cannot show
 * offer a download and File Station). */
export function viewOfPath(path: string): ViewTarget | null {
  if (!looksLikePath(path)) return null;
  const info = infoOf(path);
  const encoded = encodeURIComponent(path);
  const download = `/api/files/download?path=${encoded}`;
  const base = { title: lastSegment(path), from: path, download, path };
  if (!info) return { ...base, kind: "other", src: download };
  switch (info.kind) {
    case "image":
    case "pdf":
      // RAST previews pictures and PDFs inline.
      return { ...base, kind: info.kind, src: `/api/files/preview?path=${encoded}` };
    case "text":
      return { ...base, kind: "text", format: info.format, src: `/api/files/text?path=${encoded}` };
    case "document":
      return { ...base, kind: "document", family: info.family, src: download };
    default:
      // A video plays from its download (ranges, so it seeks).
      return { ...base, kind: info.kind, src: download };
  }
}

/** A file sent in the chat: pictures, PDFs and text. */
export function viewOfChatFile(file: Pick<FileBlock, "id" | "name" | "kind">): ViewTarget | null {
  const src = fileUrl(file.id);
  const base = { title: file.name, from: "Sent in this chat", src, download: src, file: file.id };
  if (file.kind === "image") return { ...base, kind: "image" };
  if (file.kind === "pdf") return { ...base, kind: "pdf" };
  if (file.kind === "text") {
    const format = infoOf(file.name)?.format;
    return { ...base, kind: "text", format: format && format !== "html" ? format : "plain" };
  }
  return null;
}

/** A link's target: a web address or a path in the person's files. */
export function viewOfLink(href: string): ViewTarget | null {
  return isWebUrl(href) ? viewOfUrl(href) : viewOfPath(href);
}

/** The folder a path is in ("/" for the home folder's files). */
export function folderOf(path: string): string {
  const at = path.replace(/\/+$/, "").lastIndexOf("/");
  return at <= 0 ? "/" : path.slice(0, at);
}

/** The same thing opens in the same tab. */
export function viewKey(target: ViewTarget): string {
  return `v:${target.path ?? target.external ?? target.file ?? target.src}`;
}

/** What a tab shows as its badge: a short type name and its colour. */
export interface Badge {
  text: string;
  tone: "red" | "blue" | "green" | "orange" | "violet" | "gray";
  /** An icon instead of text. */
  icon?: string;
}

export function badgeOf(target: Pick<ViewTarget, "kind" | "format" | "family" | "title" | "from">): Badge {
  switch (target.kind) {
    case "image":
      return { text: "", tone: "violet", icon: "image" };
    case "video":
      return { text: "", tone: "violet", icon: "play" };
    case "pdf":
      return { text: "PDF", tone: "red" };
    case "document":
      return target.family === "sheet"
        ? { text: "XLS", tone: "green" }
        : target.family === "slides"
          ? { text: "PPT", tone: "orange" }
          : { text: "W", tone: "blue" };
    case "website":
      return { text: "", tone: "gray", icon: "browser" };
    case "text": {
      if (target.format === "markdown") return { text: "MD", tone: "blue" };
      if (target.format === "csv" || target.format === "tsv") return { text: "CSV", tone: "green" };
      if (target.format === "json") return { text: "{ }", tone: "orange" };
      if (target.format === "html") return { text: "</>", tone: "orange" };
      const ext = extensionOf(target.from || target.title).toUpperCase().slice(0, 4);
      return { text: ext || "TXT", tone: "gray" };
    }
    default:
      return { text: extensionOf(target.from || target.title).toUpperCase().slice(0, 4) || "FILE", tone: "gray" };
  }
}

/** What a kind is called (the path bar). */
export function kindLabel(target: Pick<ViewTarget, "kind" | "format" | "family">): string {
  switch (target.kind) {
    case "image":
      return "Image";
    case "video":
      return "Video";
    case "pdf":
      return "PDF";
    case "document":
      return target.family === "sheet" ? "Spreadsheet" : target.family === "slides" ? "Slides" : "Document";
    case "website":
      return "Web page";
    case "text":
      switch (target.format) {
        case "markdown":
          return "Markdown";
        case "csv":
          return "CSV";
        case "tsv":
          return "TSV";
        case "json":
          return "JSON";
        case "html":
          return "HTML";
        case "code":
          return "Code";
        default:
          return "Text";
      }
    default:
      return "File";
  }
}

/** Rows of a CSV or TSV (quotes, doubled quotes, line breaks inside quotes). */
export function parseDelimited(text: string, separator: "," | "\t", maxRows = 2000): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let cell = "";
  let quoted = false;
  for (let i = 0; i < text.length; i += 1) {
    const c = text[i];
    if (quoted) {
      if (c === '"') {
        if (text[i + 1] === '"') {
          cell += '"';
          i += 1;
        } else quoted = false;
      } else cell += c;
      continue;
    }
    if (c === '"' && cell === "") quoted = true;
    else if (c === separator) {
      row.push(cell);
      cell = "";
    } else if (c === "\n" || c === "\r") {
      if (c === "\r" && text[i + 1] === "\n") i += 1;
      row.push(cell);
      rows.push(row);
      row = [];
      cell = "";
      if (rows.length >= maxRows) return rows;
    } else cell += c;
  }
  if (cell !== "" || row.length) {
    row.push(cell);
    rows.push(row);
  }
  return rows;
}
