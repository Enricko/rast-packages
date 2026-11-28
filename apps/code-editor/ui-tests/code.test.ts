import assert from "node:assert/strict";
import test from "node:test";

import {
  askText,
  baseName,
  detectIndent,
  isInside,
  joinPath,
  languageChoices,
  languageOf,
  lineEnding,
  parentOf,
  relativeTo,
  validName,
} from "../src/apps/code/paths";

test("paths of the person's files", () => {
  assert.equal(joinPath("/", "a.txt"), "/a.txt");
  assert.equal(joinPath("/site/", "src"), "/site/src");
  assert.equal(joinPath("/site", "/x"), "/site/x");
  assert.equal(parentOf("/site/src/main.rs"), "/site/src");
  assert.equal(parentOf("/a.txt"), "/");
  assert.equal(parentOf("/"), "/");
  assert.equal(baseName("/site/src/main.rs"), "main.rs");
  assert.equal(baseName("/site/"), "site");
  assert.equal(baseName("/"), "/");
  assert.ok(isInside("/site/src/a", "/site"));
  assert.ok(isInside("/site", "/site"));
  assert.ok(!isInside("/sites/a", "/site"));
  assert.ok(isInside("/anything", "/"));
  assert.equal(relativeTo("/site/src/a.ts", "/site"), "src/a.ts");
  assert.equal(relativeTo("/a.ts", "/"), "a.ts");
  assert.equal(relativeTo("/other/a.ts", "/site"), "/other/a.ts");
});

test("names a person may give", () => {
  assert.equal(validName("index.ts"), null);
  assert.equal(validName("  "), "Give it a name.");
  assert.equal(validName("a/b"), "A name has no slashes.");
  assert.equal(validName(".."), "Choose another name.");
});

test("the language of a file comes from its name", () => {
  const id = (path: string) => languageOf(path).id;
  assert.equal(id("/a/main.rs"), "rust");
  assert.equal(id("/a/App.TSX"), "tsx");
  assert.equal(id("/a/index.mjs"), "javascript");
  assert.equal(id("/a/Dockerfile"), "dockerfile");
  assert.equal(id("/a/Dockerfile.prod"), "dockerfile");
  assert.equal(id("/a/.bashrc"), "shell");
  assert.equal(id("/a/.env.local"), "properties");
  assert.equal(id("/a/Cargo.lock"), "toml");
  assert.equal(id("/a/CMakeLists.txt"), "cmake");
  assert.equal(id("/a/notes.txt"), "text");
  assert.equal(id("/a/README"), "text");
  assert.equal(languageOf("/a/Makefile").label, "Makefile");
  const choices = languageChoices();
  assert.equal(choices[0]?.id, "text");
  assert.equal(new Set(choices.map((c) => c.id)).size, choices.length);
  assert.ok(choices.some((c) => c.id === "python"));
});

test("indentation and line endings are read from the text", () => {
  assert.deepEqual(detectIndent("a\n  b\n    c\n  d\n"), { tabs: false, size: 2 });
  assert.deepEqual(detectIndent("a\n    b\n        c\n    d\n"), { tabs: false, size: 4 });
  assert.deepEqual(detectIndent("a\n\tb\n\t\tc\n\td\n"), { tabs: true, size: 4 });
  assert.deepEqual(detectIndent("one line"), { tabs: false, size: 4 });
  assert.equal(lineEnding("a\r\nb\r\nc"), "CRLF");
  assert.equal(lineEnding("a\nb\r\nc\n"), "LF");
  assert.equal(lineEnding(""), "LF");
});

test("Ask AI hands over the selected lines in a code block, else the path", () => {
  assert.equal(askText("/a/app.py", "python"), "About my file /a/app.py: ");
  assert.equal(askText("/a/app.py", "python", { text: "  ", from: 1, to: 1 }), "About my file /a/app.py: ");
  assert.equal(
    askText("/a/app.py", "python", { text: "x = 1\ny = 2\n", from: 3, to: 4 }),
    "About lines 3–4 of my file /a/app.py:\n```python\nx = 1\ny = 2\n```\n\n",
  );
  assert.equal(askText("/n.txt", "text", { text: "a ``` b", from: 2, to: 2 }), "About line 2 of my file /n.txt:\n````\na ``` b\n````\n\n");
  assert.equal(askText("/n.txt", "text", { text: "abcdef", from: 1, to: 1 }, 3), "About line 1 of my file /n.txt:\n```\nabc\n…\n```\n\n");
});
