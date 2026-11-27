#!/usr/bin/env bash
# Build AI Agent's window (ui/app.js + ui/app.css) with RAST's frontend.
#
# The window is written against RAST's frontend tree (its window SDK in
# src/core, its helpers and types): this lays ui/ into a copy of a RAST
# checkout's frontend as src/apps/agent and ui-tests/ as its tests, then
# type-checks it, runs the tests and bundles it with RAST's build-app.mjs.
#
# Usage: scripts/build-ui.sh <RAST checkout> [out dir] [--check]
#   <RAST checkout>  RAST at the commit pinned in Cargo.toml, with
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
      sed -n '2,14p' "$0"
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
mkdir -p "$out"
out=$(cd "$out" && pwd)

work=$(mktemp -d "${TMPDIR:-/tmp}/ai-agent-ui.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir "$work/frontend"
(cd "$rast/frontend" && tar --exclude=./node_modules --exclude=./dist -cf - .) |
  (cd "$work/frontend" && tar -xf -)
ln -s "$rast/frontend/node_modules" "$work/frontend/node_modules"

# The window as src/apps/agent; only AI Agent's tests run here.
rm -rf "$work/frontend/src/apps/agent"
mkdir -p "$work/frontend/src/apps/agent"
cp -R "$here/ui/." "$work/frontend/src/apps/agent/"
find "$work/frontend/tests" -maxdepth 1 -name '*.test.ts' -delete
cp "$here"/ui-tests/*.test.ts "$work/frontend/tests/"

cd "$work/frontend"
npx tsc --noEmit -p .
node tests/run.mjs
if [[ "$check" == 1 ]]; then
  exit 0
fi
node scripts/build-app.mjs src/apps/agent/package.ts "$out"
test -s "$out/app.js"
test -s "$out/app.css"
echo "built $out/app.js and $out/app.css"
