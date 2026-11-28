#!/usr/bin/env bash
# Build Code Editor's window (ui/app.js + ui/app.css) with RAST's frontend.
#
# The window is written against RAST's frontend tree (its window SDK in
# src/core, its helpers and types): this lays ui/ into a copy of a RAST
# checkout's frontend as src/apps/code and ui-tests/ as its tests, gives it
# RAST's packages plus this app's own (CodeMirror, from package-lock.json),
# then type-checks it, runs the tests and bundles it with RAST's
# build-app.mjs.
#
# Usage: scripts/build-ui.sh <RAST checkout> [out dir] [--check]
#   <RAST checkout>  RAST at the commit named in rast-commit.txt, with
#                    `npm ci` run in its frontend/
#   [out dir]        where app.js and app.css go (default: ui-build)
#   --check          type-check and test only
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
rast=""
out="$here/ui-build"
check=0
for arg in "$@"; do
  case "$arg" in
    --check) check=1 ;;
    -h | --help)
      sed -n '2,15p' "$0"
      exit 0
      ;;
    *)
      if [[ -z "$rast" ]]; then
        rast="$arg"
      else
        out="$arg"
      fi
      ;;
  esac
done
if [[ -z "$rast" ]]; then
  echo "usage: scripts/build-ui.sh <RAST checkout> [out dir] [--check]" >&2
  exit 2
fi
rast=$(cd "$rast" && pwd)
if [[ ! -f "$rast/frontend/scripts/build-app.mjs" ]]; then
  echo "$rast is not a RAST checkout with frontend/scripts/build-app.mjs" >&2
  exit 1
fi
if [[ ! -d "$rast/frontend/node_modules" ]]; then
  echo "run \`npm ci\` in $rast/frontend first" >&2
  exit 1
fi
if [[ ! -d "$here/node_modules/@codemirror/view" ]]; then
  echo "run \`npm ci\` in $here first" >&2
  exit 1
fi
mkdir -p "$out"
out=$(cd "$out" && pwd)

work=$(mktemp -d "${TMPDIR:-/tmp}/code-editor-ui.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir "$work/frontend"
(cd "$rast/frontend" && tar --exclude=./node_modules --exclude=./dist -cf - .) |
  (cd "$work/frontend" && tar -xf -)

# RAST's packages and this app's, side by side (a scope both have, such as
# @types, gets both sets).
modules="$work/frontend/node_modules"
mkdir "$modules"
link_all() {
  local from="$1" to="$2" entry name
  for entry in "$from"/* "$from"/.bin; do
    [[ -e "$entry" ]] || continue
    name=$(basename "$entry")
    if [[ -e "$to/$name" ]]; then
      if [[ "$name" == @* && -d "$entry" && -d "$to/$name" && ! -L "$to/$name" ]]; then
        link_all "$entry" "$to/$name"
      fi
      continue
    fi
    if [[ "$name" == @* && -d "$entry" ]]; then
      mkdir "$to/$name"
      link_all "$entry" "$to/$name"
    else
      ln -s "$entry" "$to/$name"
    fi
  done
}
link_all "$rast/frontend/node_modules" "$modules"
link_all "$here/node_modules" "$modules"

rm -rf "$work/frontend/src/apps/code"
mkdir -p "$work/frontend/src/apps/code"
cp -R "$here/ui/." "$work/frontend/src/apps/code/"
find "$work/frontend/tests" -maxdepth 1 -name '*.test.ts' -delete
cp "$here"/ui-tests/*.test.ts "$work/frontend/tests/"

cd "$work/frontend"
npx tsc --noEmit -p .
node tests/run.mjs
if [[ "$check" == 1 ]]; then
  exit 0
fi
node scripts/build-app.mjs src/apps/code/package.ts "$out"
test -s "$out/app.js"
test -s "$out/app.css"
echo "built $out/app.js and $out/app.css"
