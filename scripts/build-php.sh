#!/usr/bin/env bash
# Build a static PHP CLI for one target with static-php-cli inside Alpine.
#
#   build-php.sh <php-version> <target> <source-sha256(tar.xz)> <extensions> <spc-version> <builder-image> <out-dir>
#
# Trust chain: the static-php-cli release binary must match its GitHub asset
# digest; the PHP source archive static-php-cli uses must match the SHA-256
# that php.net publishes; the result must report the requested version.
set -euo pipefail

VERSION=$1 TARGET=$2 SRC_SHA=$3 EXTENSIONS=$4 SPC_VERSION=$5 BUILDER=$6 OUT=$7
case "$TARGET" in
  linux-x86_64) ARCH=x86_64 ;;
  linux-aarch64) ARCH=aarch64 ;;
  *) echo "unknown target $TARGET" >&2; exit 1 ;;
esac
[ "$(uname -m)" = "$ARCH" ] || { echo "build $TARGET on a native $ARCH runner (got $(uname -m))" >&2; exit 1; }

WORK=$(mktemp -d)
mkdir -p "$OUT"
SPC_ASSET="spc-linux-$ARCH.tar.gz"

echo "::group::static-php-cli $SPC_VERSION"
gh release download "$SPC_VERSION" -R crazywhalecc/static-php-cli -p "$SPC_ASSET" -D "$WORK"
EXPECTED=$(gh api "repos/crazywhalecc/static-php-cli/releases/tags/$SPC_VERSION" \
  --jq ".assets[] | select(.name == \"$SPC_ASSET\") | .digest")
ACTUAL="sha256:$(sha256sum "$WORK/$SPC_ASSET" | cut -d' ' -f1)"
if [ -z "$EXPECTED" ] || [ "$EXPECTED" != "$ACTUAL" ]; then
  echo "static-php-cli digest mismatch: expected '$EXPECTED', got '$ACTUAL'" >&2
  exit 1
fi
tar -xzf "$WORK/$SPC_ASSET" -C "$WORK"
chmod +x "$WORK/spc"
echo "::endgroup::"

cat > "$WORK/inside.sh" <<'INSIDE'
#!/bin/sh
set -eux
apk add --no-cache bash file wget cmake gcc g++ jq autoconf git libstdc++ linux-headers make m4 libgcc \
  binutils bison flex pkgconf automake libtool curl xz patch perl gettext-dev re2c
cd /build
./spc doctor --auto-fix
./spc download --with-php="$VERSION" --for-extensions="$EXTENSIONS" \
  --custom-url="php-src:https://www.php.net/distributions/php-$VERSION.tar.xz"
# The PHP source used for the build must be the php.net release archive.
found=""
for f in downloads/php-"$VERSION".tar.* downloads/php-src*; do
  [ -f "$f" ] || continue
  sum=$(sha256sum "$f" | cut -d' ' -f1)
  echo "source archive $f sha256 $sum"
  if [ "$sum" = "$SRC_SHA" ]; then found="$f"; fi
done
[ -n "$found" ] || { echo "no downloaded PHP source matches the php.net SHA-256 $SRC_SHA" >&2; ls -la downloads >&2; exit 1; }
./spc build "$EXTENSIONS" --build-cli
./spc dump-license --for-extensions="$EXTENSIONS" --dump-dir=buildroot/license || true
./buildroot/bin/php -v
chown -R "$HOST_UID:$HOST_GID" /build
INSIDE
chmod +x "$WORK/inside.sh"

echo "::group::build PHP $VERSION ($TARGET) in $BUILDER"
docker run --rm -v "$WORK:/build" -w /build \
  -e VERSION="$VERSION" -e EXTENSIONS="$EXTENSIONS" -e SRC_SHA="$SRC_SHA" \
  -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
  "$BUILDER" /build/inside.sh
echo "::endgroup::"

PKG="php-$VERSION"
STAGE="$WORK/stage/$PKG"
mkdir -p "$STAGE/bin"
install -m 0755 "$WORK/buildroot/bin/php" "$STAGE/bin/php"
if [ -d "$WORK/buildroot/license" ]; then cp -r "$WORK/buildroot/license" "$STAGE/licenses"; fi
"$STAGE/bin/php" -v | head -1 | grep -F "PHP $VERSION " >/dev/null
cat > "$STAGE/BUILDINFO.json" <<EOF
{
  "php": "$VERSION",
  "target": "$TARGET",
  "source": "https://www.php.net/distributions/php-$VERSION.tar.xz",
  "sourceSha256": "$SRC_SHA",
  "spcVersion": "$SPC_VERSION",
  "spcAssetDigest": "$EXPECTED",
  "builder": "$BUILDER",
  "extensions": "$EXTENSIONS",
  "workflowRun": "${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:-local}/actions/runs/${GITHUB_RUN_ID:-0}"
}
EOF
ARCHIVE="$OUT/$PKG-$TARGET.tar.gz"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime=@0 -C "$WORK/stage" -cf - "$PKG" | gzip -n -9 > "$ARCHIVE"
sha256sum "$ARCHIVE"
echo "archive=$ARCHIVE" >> "${GITHUB_OUTPUT:-/dev/null}"
