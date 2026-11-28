// The CodeMirror language for each id of paths.ts `languageOf`.

import { cpp } from "@codemirror/lang-cpp";
import { css } from "@codemirror/lang-css";
import { go } from "@codemirror/lang-go";
import { html } from "@codemirror/lang-html";
import { java } from "@codemirror/lang-java";
import { javascript } from "@codemirror/lang-javascript";
import { json } from "@codemirror/lang-json";
import { markdown } from "@codemirror/lang-markdown";
import { php } from "@codemirror/lang-php";
import { python } from "@codemirror/lang-python";
import { rust } from "@codemirror/lang-rust";
import { sql } from "@codemirror/lang-sql";
import { xml } from "@codemirror/lang-xml";
import { yaml } from "@codemirror/lang-yaml";
import { StreamLanguage, type StreamParser } from "@codemirror/language";
import { csharp, dart, kotlin, scala, shader } from "@codemirror/legacy-modes/mode/clike";
import { clojure } from "@codemirror/legacy-modes/mode/clojure";
import { cmake } from "@codemirror/legacy-modes/mode/cmake";
import { commonLisp } from "@codemirror/legacy-modes/mode/commonlisp";
import { diff } from "@codemirror/legacy-modes/mode/diff";
import { dockerFile } from "@codemirror/legacy-modes/mode/dockerfile";
import { erlang } from "@codemirror/legacy-modes/mode/erlang";
import { groovy } from "@codemirror/legacy-modes/mode/groovy";
import { haskell } from "@codemirror/legacy-modes/mode/haskell";
import { julia } from "@codemirror/legacy-modes/mode/julia";
import { lua } from "@codemirror/legacy-modes/mode/lua";
import { fSharp, oCaml } from "@codemirror/legacy-modes/mode/mllike";
import { nginx } from "@codemirror/legacy-modes/mode/nginx";
import { perl } from "@codemirror/legacy-modes/mode/perl";
import { powerShell } from "@codemirror/legacy-modes/mode/powershell";
import { properties } from "@codemirror/legacy-modes/mode/properties";
import { protobuf } from "@codemirror/legacy-modes/mode/protobuf";
import { r } from "@codemirror/legacy-modes/mode/r";
import { ruby } from "@codemirror/legacy-modes/mode/ruby";
import { shell } from "@codemirror/legacy-modes/mode/shell";
import { swift } from "@codemirror/legacy-modes/mode/swift";
import { toml } from "@codemirror/legacy-modes/mode/toml";
import type { Extension } from "@codemirror/state";

const stream = (parser: StreamParser<unknown>) => () => StreamLanguage.define(parser);

const LANGUAGES: Record<string, () => Extension> = {
  javascript: () => javascript(),
  jsx: () => javascript({ jsx: true }),
  typescript: () => javascript({ typescript: true }),
  tsx: () => javascript({ jsx: true, typescript: true }),
  json: () => json(),
  html: () => html(),
  css: () => css(),
  python: () => python(),
  rust: () => rust(),
  markdown: () => markdown(),
  yaml: () => yaml(),
  xml: () => xml(),
  sql: () => sql(),
  go: () => go(),
  java: () => java(),
  cpp: () => cpp(),
  php: () => php(),
  shell: stream(shell as StreamParser<unknown>),
  toml: stream(toml as StreamParser<unknown>),
  dockerfile: stream(dockerFile as StreamParser<unknown>),
  nginx: stream(nginx as StreamParser<unknown>),
  properties: stream(properties as StreamParser<unknown>),
  lua: stream(lua as StreamParser<unknown>),
  ruby: stream(ruby as StreamParser<unknown>),
  powershell: stream(powerShell as StreamParser<unknown>),
  kotlin: stream(kotlin as StreamParser<unknown>),
  csharp: stream(csharp as StreamParser<unknown>),
  scala: stream(scala as StreamParser<unknown>),
  dart: stream(dart as StreamParser<unknown>),
  swift: stream(swift as StreamParser<unknown>),
  r: stream(r as StreamParser<unknown>),
  perl: stream(perl as StreamParser<unknown>),
  diff: stream(diff as StreamParser<unknown>),
  cmake: stream(cmake as StreamParser<unknown>),
  protobuf: stream(protobuf as StreamParser<unknown>),
  haskell: stream(haskell as StreamParser<unknown>),
  erlang: stream(erlang as StreamParser<unknown>),
  julia: stream(julia as StreamParser<unknown>),
  groovy: stream(groovy as StreamParser<unknown>),
  clojure: stream(clojure as StreamParser<unknown>),
  commonlisp: stream(commonLisp as StreamParser<unknown>),
  ocaml: stream(oCaml as StreamParser<unknown>),
  fsharp: stream(fSharp as StreamParser<unknown>),
  shader: stream(shader as StreamParser<unknown>),
};

/** The editor extension for a language id; plain text has none. */
export function languageExtension(id: string): Extension {
  const make = LANGUAGES[id];
  return make ? make() : [];
}
