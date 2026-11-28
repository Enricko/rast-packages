// Paths of the person's RAST files ("/" is their home, "/shared/<name>" a
// shared folder) and the language each file is written in. Pure: the tests
// import this without a browser.

export function joinPath(dir: string, name: string): string {
  const clean = name.replace(/^\/+/, "");
  if (!dir || dir === "/") return `/${clean}`;
  return `${dir.replace(/\/+$/, "")}/${clean}`;
}

export function parentOf(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  const at = trimmed.lastIndexOf("/");
  return at <= 0 ? "/" : trimmed.slice(0, at);
}

export function baseName(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  return trimmed.slice(trimmed.lastIndexOf("/") + 1) || "/";
}

/** `path` is `root` or inside it. */
export function isInside(path: string, root: string): boolean {
  if (root === "/") return path.startsWith("/");
  return path === root || path.startsWith(`${root}/`);
}

/** The path relative to `root` ("src/main.rs"), or the path itself when outside. */
export function relativeTo(path: string, root: string): string {
  if (!isInside(path, root) || path === root) return path;
  return root === "/" ? path.slice(1) : path.slice(root.length + 1);
}

/** A file name a person may give (no slashes, not "." or ".."). */
export function validName(name: string): string | null {
  const trimmed = name.trim();
  if (!trimmed) return "Give it a name.";
  if (trimmed.includes("/")) return "A name has no slashes.";
  if (trimmed === "." || trimmed === "..") return "Choose another name.";
  if (trimmed.length > 255) return "That name is too long.";
  return null;
}

export interface LanguageId {
  /** Key of the language in languages.ts (or "text"). */
  id: string;
  label: string;
}

const BY_EXTENSION: Record<string, LanguageId> = {};
const BY_NAME: Record<string, LanguageId> = {};

function add(id: string, label: string, extensions: string[], names: string[] = []): void {
  for (const ext of extensions) BY_EXTENSION[ext] = { id, label };
  for (const name of names) BY_NAME[name.toLowerCase()] = { id, label };
}

add("javascript", "JavaScript", ["js", "mjs", "cjs"]);
add("jsx", "JavaScript JSX", ["jsx"]);
add("typescript", "TypeScript", ["ts", "mts", "cts"]);
add("tsx", "TypeScript JSX", ["tsx"]);
add("json", "JSON", ["json", "jsonc", "json5", "webmanifest", "babelrc", "eslintrc", "prettierrc"], [".babelrc", ".eslintrc", ".prettierrc"]);
add("html", "HTML", ["html", "htm", "xhtml", "vue", "svelte", "astro", "hbs", "ejs"]);
add("css", "CSS", ["css", "scss", "sass", "less", "pcss"]);
add("python", "Python", ["py", "pyw", "pyi"], ["SConstruct", "SConscript"]);
add("rust", "Rust", ["rs"]);
add("markdown", "Markdown", ["md", "markdown", "mdx", "mkd"]);
add("yaml", "YAML", ["yml", "yaml"], [".clang-format"]);
add("xml", "XML", ["xml", "svg", "xsd", "xsl", "xslt", "plist", "csproj", "fsproj", "vbproj", "props", "targets", "resx", "storyboard", "xaml", "kml", "gpx", "rss", "atom"]);
add("sql", "SQL", ["sql", "ddl", "dml"]);
add("go", "Go", ["go"], ["go.mod", "go.sum", "go.work"]);
add("java", "Java", ["java"]);
add("cpp", "C/C++", ["c", "h", "cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++", "ino", "m", "mm", "cu", "cuh"]);
add("php", "PHP", ["php", "phtml", "php3", "php4", "php5", "phps"]);
add("shell", "Shell", ["sh", "bash", "zsh", "ksh", "fish", "command"], [
  ".bashrc",
  ".bash_profile",
  ".bash_aliases",
  ".bash_logout",
  ".profile",
  ".zshrc",
  ".zprofile",
  ".zshenv",
  ".envrc",
  "PKGBUILD",
]);
add("toml", "TOML", ["toml"], ["Cargo.lock", "Pipfile", "poetry.lock"]);
add("dockerfile", "Dockerfile", ["dockerfile", "containerfile"], ["Dockerfile", "Containerfile"]);
add("nginx", "nginx", ["nginx"], ["nginx.conf"]);
add("properties", "Properties", ["ini", "cfg", "conf", "properties", "env", "editorconfig", "gitconfig", "service", "socket", "timer", "desktop", "npmrc"], [
  ".env",
  ".editorconfig",
  ".gitconfig",
  ".npmrc",
  ".gitattributes",
]);
add("lua", "Lua", ["lua"]);
add("ruby", "Ruby", ["rb", "rake", "gemspec", "ru"], ["Gemfile", "Rakefile", "Vagrantfile", "Podfile"]);
add("powershell", "PowerShell", ["ps1", "psm1", "psd1"]);
add("kotlin", "Kotlin", ["kt", "kts"]);
add("csharp", "C#", ["cs", "csx"]);
add("scala", "Scala", ["scala", "sc", "sbt"]);
add("dart", "Dart", ["dart"]);
add("swift", "Swift", ["swift"]);
add("r", "R", ["r", "rmd"]);
add("perl", "Perl", ["pl", "pm", "t"]);
add("diff", "Diff", ["diff", "patch"]);
add("cmake", "CMake", ["cmake"], ["CMakeLists.txt"]);
add("protobuf", "Protocol Buffers", ["proto"]);
add("haskell", "Haskell", ["hs", "lhs"]);
add("erlang", "Erlang", ["erl", "hrl"]);
add("julia", "Julia", ["jl"]);
add("groovy", "Groovy", ["groovy", "gradle", "gvy"], ["Jenkinsfile"]);
add("clojure", "Clojure", ["clj", "cljs", "cljc", "edn"]);
add("commonlisp", "Lisp", ["lisp", "lsp", "cl", "el"]);
add("ocaml", "OCaml", ["ml", "mli"]);
add("fsharp", "F#", ["fs", "fsi", "fsx"]);
add("shader", "Shader", ["glsl", "vert", "frag", "hlsl", "wgsl"]);

const TEXT: LanguageId = { id: "text", label: "Plain text" };

/** The language a file is written in, from its name. */
export function languageOf(path: string): LanguageId {
  const name = baseName(path);
  const lower = name.toLowerCase();
  const byName = BY_NAME[lower];
  if (byName) return byName;
  if (lower.startsWith("dockerfile.") || lower.endsWith(".dockerfile")) return BY_NAME["dockerfile"] ?? TEXT;
  if (lower === "makefile" || lower === "gnumakefile") return { id: "text", label: "Makefile" };
  const dot = lower.lastIndexOf(".");
  if (dot < 0) return TEXT;
  const ext = lower.slice(dot + 1);
  if (lower.startsWith(".env.")) return BY_EXTENSION["env"] ?? TEXT;
  return BY_EXTENSION[ext] ?? TEXT;
}

/** Every language a person can pick for a file (the status bar's menu). */
export function languageChoices(): LanguageId[] {
  const seen = new Map<string, LanguageId>();
  for (const language of [...Object.values(BY_EXTENSION), ...Object.values(BY_NAME)]) {
    if (!seen.has(language.id)) seen.set(language.id, language);
  }
  return [TEXT, ...[...seen.values()].sort((a, b) => a.label.localeCompare(b.label))];
}

/** How the file indents: tabs, or the most common run of spaces (2, 4 or 8). */
export function detectIndent(text: string): { tabs: boolean; size: number } {
  let tabs = 0;
  const widths = new Map<number, number>();
  let previous = 0;
  let lines = 0;
  for (const line of text.split("\n", 4000)) {
    if (!line.trim()) continue;
    lines += 1;
    if (line.startsWith("\t")) {
      tabs += 1;
      continue;
    }
    const spaces = line.length - line.trimStart().length;
    const step = Math.abs(spaces - previous);
    if (step >= 2 && step <= 8) widths.set(step, (widths.get(step) ?? 0) + 1);
    previous = spaces;
  }
  if (lines && tabs > lines / 4) return { tabs: true, size: 4 };
  let best = 4;
  let count = 0;
  for (const [width, seen] of widths) {
    if (seen > count || (seen === count && width < best)) {
      best = width;
      count = seen;
    }
  }
  return { tabs: false, size: count ? best : 4 };
}

/** "LF" or "CRLF": the line ending the file mostly uses. */
export function lineEnding(text: string): "LF" | "CRLF" {
  const crlf = (text.match(/\r\n/g) ?? []).length;
  const lf = (text.match(/\n/g) ?? []).length - crlf;
  return crlf > lf ? "CRLF" : "LF";
}
