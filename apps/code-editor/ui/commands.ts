// Pure text & command helpers for Code Editor's control channel (`ctx.onRequest`),
// letting AI Agent operate Code Editor directly as a live coding agent.

export interface CommandSpec {
  id: string;
  label: string;
  params?: Record<string, string>;
  enabled: boolean;
}

export const COMMANDS: Array<Omit<CommandSpec, "enabled">> = [
  { id: "file.open", label: "Open a file at an optional 1-based line", params: { path: "string", line: "number?" } },
  { id: "file.create", label: "Create a new file with optional initial text and open it", params: { path: "string", content: "string?" } },
  { id: "file.save", label: "Save the active file (or the file at path) back to RAST files", params: { path: "string?" } },
  { id: "file.saveAll", label: "Save every modified open tab" },
  { id: "file.close", label: "Close the active tab (or the tab at path)", params: { path: "string?" } },
  { id: "folder.open", label: "Open a folder in the file tree sidebar", params: { path: "string" } },
  { id: "editor.read", label: "Read lines from the active file (or path) with 1-based line numbers", params: { path: "string?", fromLine: "number?", toLine: "number?" } },
  { id: "editor.replace", label: "Replace exact text in the buffer (first match or all)", params: { search: "string", replace: "string", all: "boolean?", path: "string?" } },
  { id: "editor.replaceLines", label: "Replace a 1-based inclusive range of lines", params: { fromLine: "number", toLine: "number", text: "string", path: "string?" } },
  { id: "editor.setContent", label: "Replace the entire file content in the editor buffer", params: { text: "string", path: "string?" } },
  { id: "editor.insert", label: "Insert text at the cursor or before a 1-based line", params: { text: "string", line: "number?", path: "string?" } },
  { id: "editor.goto", label: "Move the cursor and scroll to a 1-based line", params: { line: "number", column: "number?" } },
  { id: "editor.find", label: "Find matching lines in the active file (or path)", params: { query: "string", path: "string?" } },
];

/** Filter the command catalog by an optional search string. */
export function listCommands(search: string | undefined, hasActiveTab: boolean): CommandSpec[] {
  const q = (search ?? "").trim().toLowerCase();
  return COMMANDS.filter((c) => !q || c.id.toLowerCase().includes(q) || c.label.toLowerCase().includes(q)).map((c) => ({
    ...c,
    enabled:
      hasActiveTab ||
      c.id === "file.open" ||
      c.id === "file.create" ||
      c.id === "file.saveAll" ||
      c.id === "folder.open",
  }));
}

/** Return numbered lines (`1: ...`) from `text` within 1-based inclusive bounds. */
export function formatLines(text: string, fromLine?: number, toLine?: number, maxChars = 24000): {
  totalLines: number;
  fromLine: number;
  toLine: number;
  truncated: boolean;
  text: string;
} {
  const lines = text.split("\n");
  const totalLines = lines.length;
  const start = Math.max(1, Math.min(totalLines, Math.floor(fromLine ?? 1)));
  const end = Math.max(start, Math.min(totalLines, Math.floor(toLine ?? totalLines)));
  const out: string[] = [];
  let chars = 0;
  let lastLine = start - 1;
  let truncated = false;
  for (let n = start; n <= end; n++) {
    const row = `${n}: ${lines[n - 1] ?? ""}`;
    if (chars + row.length + 1 > maxChars && out.length > 0) {
      truncated = true;
      break;
    }
    out.push(row);
    chars += row.length + 1;
    lastLine = n;
  }
  return {
    totalLines,
    fromLine: start,
    toLine: Math.max(start, lastLine),
    truncated,
    text: out.join("\n"),
  };
}

/** Exact string replacement (first occurrence or all occurrences). */
export function applyTextReplace(
  content: string,
  search: string,
  replace: string,
  all = false,
): { text: string; count: number } {
  if (!search) throw new Error("search must not be empty");
  if (all) {
    const parts = content.split(search);
    const count = parts.length - 1;
    return { text: parts.join(replace), count };
  }
  const idx = content.indexOf(search);
  if (idx === -1) return { text: content, count: 0 };
  return {
    text: content.slice(0, idx) + replace + content.slice(idx + search.length),
    count: 1,
  };
}

/** Replace 1-based inclusive line range `[fromLine, toLine]` with `replacement`. */
export function applyLineReplace(
  content: string,
  fromLine: number,
  toLine: number,
  replacement: string,
): { text: string; totalLines: number } {
  const lines = content.split("\n");
  if (fromLine < 1 || toLine < fromLine || fromLine > lines.length + 1) {
    throw new Error(`invalid line range ${fromLine}..${toLine} (file has ${lines.length} lines)`);
  }
  const clampedEnd = Math.min(toLine, lines.length);
  const newLines = replacement === "" ? [] : replacement.replace(/\r\n/g, "\n").split("\n");
  lines.splice(fromLine - 1, clampedEnd - fromLine + 1, ...newLines);
  const text = lines.join("\n");
  return { text, totalLines: lines.length };
}

/** Find lines containing `query` (case-insensitive) in `content`. */
export function findInText(content: string, query: string, limit = 50): Array<{ line: number; text: string }> {
  const needle = query.toLowerCase();
  if (!needle) return [];
  const matches: Array<{ line: number; text: string }> = [];
  const lines = content.split("\n");
  for (let i = 0; i < lines.length; i++) {
    if (lines[i]!.toLowerCase().includes(needle)) {
      matches.push({ line: i + 1, text: lines[i]! });
      if (matches.length >= limit) break;
    }
  }
  return matches;
}
