// The CodeMirror setup each open file gets: RAST's colours (code.css styles
// the `tok-*` classes for light and dark), the usual editing keys, search,
// and settings that change while it is open (language, indent, wrapping,
// read-only) in compartments. Lines end in "\n" here; a CRLF file is
// written back with "\r\n".

import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { bracketMatching, foldGutter, foldKeymap, indentOnInput, indentUnit, syntaxHighlighting } from "@codemirror/language";
import { highlightSelectionMatches, search, searchKeymap } from "@codemirror/search";
import { Compartment, EditorState, type Extension } from "@codemirror/state";
import {
  crosshairCursor,
  drawSelection,
  dropCursor,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  rectangularSelection,
  type KeyBinding,
  type ViewUpdate,
} from "@codemirror/view";
import { classHighlighter } from "@lezer/highlight";
import { languageExtension } from "./languages";

export const languageSlot = new Compartment();
export const indentSlot = new Compartment();
export const wrapSlot = new Compartment();
export const readOnlySlot = new Compartment();

export interface FileSettings {
  language: string;
  tabs: boolean;
  size: number;
  wrap: boolean;
  readOnly: boolean;
}

export function indentExtension(tabs: boolean, size: number): Extension {
  return [indentUnit.of(tabs ? "\t" : " ".repeat(size)), EditorState.tabSize.of(size)];
}

export function readOnlyExtension(readOnly: boolean): Extension {
  return [EditorState.readOnly.of(readOnly), EditorView.editable.of(!readOnly)];
}

const theme = EditorView.theme({
  "&": {
    height: "100%",
    fontSize: "var(--ce-font-size, 13px)",
    color: "var(--text)",
    backgroundColor: "var(--surface)",
  },
  ".cm-scroller": {
    fontFamily: "var(--font-mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace)",
    lineHeight: "1.55",
  },
  ".cm-content": { caretColor: "var(--accent)", padding: "6px 0" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--accent)", borderLeftWidth: "2px" },
  "&.cm-focused": { outline: "none" },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, ::selection": {
    backgroundColor: "var(--ce-selection) !important",
  },
  ".cm-activeLine": { backgroundColor: "var(--ce-active-line)" },
  ".cm-gutters": {
    backgroundColor: "var(--surface)",
    color: "var(--text-faint)",
    border: "none",
    borderRight: "1px solid var(--border-subtle, var(--border))",
  },
  ".cm-activeLineGutter": { backgroundColor: "var(--ce-active-line)", color: "var(--text)" },
  ".cm-lineNumbers .cm-gutterElement": { padding: "0 10px 0 14px" },
  ".cm-foldGutter .cm-gutterElement": { padding: "0 4px", color: "var(--text-faint)" },
  ".cm-foldPlaceholder": {
    backgroundColor: "var(--surface-2)",
    border: "1px solid var(--border)",
    color: "var(--text-dim)",
    borderRadius: "4px",
    padding: "0 6px",
  },
  ".cm-matchingBracket, &.cm-focused .cm-matchingBracket": {
    backgroundColor: "color-mix(in srgb, var(--accent) 22%, transparent)",
    outline: "1px solid color-mix(in srgb, var(--accent) 45%, transparent)",
  },
  ".cm-nonmatchingBracket": { color: "var(--danger)" },
  ".cm-selectionMatch": { backgroundColor: "color-mix(in srgb, var(--accent) 14%, transparent)" },
  ".cm-searchMatch": {
    backgroundColor: "color-mix(in srgb, var(--warning) 28%, transparent)",
    outline: "1px solid color-mix(in srgb, var(--warning) 55%, transparent)",
  },
  ".cm-searchMatch.cm-searchMatch-selected": { backgroundColor: "color-mix(in srgb, var(--warning) 55%, transparent)" },
  ".cm-panels": { backgroundColor: "var(--surface-2)", color: "var(--text)" },
  ".cm-panels.cm-panels-top": { borderBottom: "1px solid var(--border)" },
  ".cm-panels.cm-panels-bottom": { borderTop: "1px solid var(--border)" },
  ".cm-panel.cm-search": { padding: "6px 10px", fontFamily: "inherit" },
  ".cm-panel.cm-search input, .cm-panel.cm-gotoLine input": {
    background: "var(--surface)",
    color: "var(--text)",
    border: "1px solid var(--border)",
    borderRadius: "6px",
    padding: "3px 7px",
    fontSize: "12.5px",
  },
  ".cm-panel.cm-search button, .cm-panel.cm-gotoLine button": {
    background: "var(--surface)",
    color: "var(--text)",
    border: "1px solid var(--border)",
    borderRadius: "6px",
    padding: "2px 9px",
    fontSize: "12.5px",
    backgroundImage: "none",
  },
  ".cm-panel.cm-search label": { fontSize: "12.5px", color: "var(--text-dim)" },
  ".cm-panel button[name=close]": { border: "none", background: "transparent", color: "var(--text-dim)", fontSize: "16px" },
  ".cm-tooltip": {
    backgroundColor: "var(--surface-solid, var(--surface))",
    color: "var(--text)",
    border: "1px solid var(--border)",
    borderRadius: "8px",
    boxShadow: "0 10px 30px -12px rgb(0 0 0 / 0.35)",
  },
  ".cm-tooltip-autocomplete > ul > li[aria-selected]": {
    backgroundColor: "var(--accent)",
    color: "var(--text-on-accent, #fff)",
  },
  ".cm-specialChar": { color: "var(--danger)" },
});

/** Everything an open file's editor has. */
export function fileExtensions(settings: FileSettings, onUpdate: (update: ViewUpdate) => void, keys: readonly KeyBinding[]): Extension {
  return [
    lineNumbers(),
    highlightActiveLineGutter(),
    highlightSpecialChars(),
    history(),
    foldGutter({ openText: "⌄", closedText: "›" }),
    drawSelection(),
    dropCursor(),
    EditorState.allowMultipleSelections.of(true),
    indentOnInput(),
    syntaxHighlighting(classHighlighter),
    bracketMatching(),
    closeBrackets(),
    autocompletion({ activateOnTyping: true }),
    rectangularSelection(),
    crosshairCursor(),
    highlightActiveLine(),
    highlightSelectionMatches(),
    search({ top: true }),
    keymap.of([...keys, ...closeBracketsKeymap, ...defaultKeymap, ...searchKeymap, ...historyKeymap, ...foldKeymap, ...completionKeymap, indentWithTab]),
    theme,
    languageSlot.of(languageExtension(settings.language)),
    indentSlot.of(indentExtension(settings.tabs, settings.size)),
    wrapSlot.of(settings.wrap ? EditorView.lineWrapping : []),
    readOnlySlot.of(readOnlyExtension(settings.readOnly)),
    EditorView.updateListener.of(onUpdate),
  ];
}
