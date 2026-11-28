// Unsaved changes kept in this browser, so a closed window or a reload
// loses nothing: the file offers them back when it opens again.

export interface Draft {
  text: string;
  /** The file's modification time the changes were made on. */
  base: number;
  at: number;
}

const PREFIX = "rast.code.draft.v1:";
/** Larger drafts stay only in the open window. */
const MAX_CHARS = 2_000_000;

function key(owner: string, path: string): string {
  return `${PREFIX}${owner}:${path}`;
}

export function readDraft(owner: string, path: string): Draft | null {
  try {
    const raw = localStorage.getItem(key(owner, path));
    if (!raw) return null;
    const draft = JSON.parse(raw) as Draft;
    return typeof draft.text === "string" && typeof draft.at === "number" ? draft : null;
  } catch {
    return null;
  }
}

export function writeDraft(owner: string, path: string, text: string, base: number): void {
  if (text.length > MAX_CHARS) return;
  try {
    localStorage.setItem(key(owner, path), JSON.stringify({ text, base, at: Date.now() } satisfies Draft));
  } catch {
    // The browser's storage is full: the window still has the changes.
  }
}

export function dropDraft(owner: string, path: string): void {
  try {
    localStorage.removeItem(key(owner, path));
  } catch {
    // Nothing kept.
  }
}

/** A draft moves with its file (renamed). */
export function moveDraft(owner: string, from: string, to: string): void {
  const draft = readDraft(owner, from);
  if (!draft) return;
  dropDraft(owner, from);
  writeDraft(owner, to, draft.text, draft.base);
}
