#!/usr/bin/env python3
"""Package ArtCraft's Craft apps (their upstream web builds) for RAST's App Center.

    python3 -I apps/craft-web/package.py build <package> <out dir>
    python3 -I apps/craft-web/package.py check
    python3 -I apps/craft-web/package.py catalog-entries <minRastVersion>

`build` downloads `<repo>-web-<version>.zip` from the upstream release
`v<version>` (crafts.json), and accepts it only when:

- SHA256SUMS.txt is signed by ArtCraft's release key (pinned here by file
  and fingerprint), checked with gpgv;
- the zip's SHA-256 is the one SHA256SUMS.txt lists, and the one GitHub
  reports for the release asset.

The unmodified web build becomes `<package>/web/` (RAST shows its
index.html as a web page window, sandboxed in an origin of its own), next to
NOTICE-RAST.md saying where it came from. One reproducible archive per
target (the same files: a web page runs anywhere), and SHA256SUMS.

`check` validates crafts.json, the pinned key and the icons. Needs: gh
(authenticated), gpg and gpgv.
"""

import gzip
import hashlib
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parent.parent
TARGETS = ("linux-x86_64", "linux-aarch64")
# Upper bound for one unpacked web build (the largest is about 200 MB).
MAX_UNPACKED = 1024 * 1024 * 1024
PACKAGE_ID = re.compile(r"^[a-z0-9][a-z0-9-]{0,39}$")
VERSION = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+$")


def fail(message: str) -> None:
    print(f"craft-web: {message}", file=sys.stderr)
    sys.exit(1)


def load() -> dict:
    return json.loads((HERE / "crafts.json").read_text())


def find(config: dict, package: str) -> dict:
    for app in config["apps"]:
        if app["package"] == package:
            return app
    fail(f"{package} is not in crafts.json")
    raise AssertionError


def run(args: list[str], **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run(args, check=True, **kwargs)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def key_fingerprint(key_file: Path) -> str:
    shown = run(
        ["gpg", "--show-keys", "--with-colons", "--with-fingerprint", str(key_file)],
        capture_output=True,
        text=True,
    ).stdout
    for line in shown.splitlines():
        if line.startswith("fpr:"):
            return line.split(":")[9]
    fail(f"{key_file.name} holds no key")
    raise AssertionError


def check(config: dict) -> None:
    upstream = config["upstream"]
    key_file = HERE / upstream["signingKey"]
    if key_fingerprint(key_file) != upstream["signingKeyFingerprint"]:
        fail(f"{key_file.name} is not the key {upstream['signingKeyFingerprint']}")
    seen = set()
    for app in config["apps"]:
        package = app["package"]
        if not PACKAGE_ID.match(package) or package in seen:
            fail(f"bad or repeated package id {package!r}")
        seen.add(package)
        if not VERSION.match(app["version"]):
            fail(f"{package}: version {app['version']!r} is not x.y.z")
        if not re.match(r"^[A-Za-z0-9_.-]+$", app["repo"]):
            fail(f"{package}: bad repo {app['repo']!r}")
        icon = REPO_ROOT / "icons" / f"{package}.png"
        data = icon.read_bytes() if icon.is_file() else b""
        if not data.startswith(b"\x89PNG\r\n\x1a\n") or len(data) > 64 * 1024:
            fail(f"{package}: icons/{package}.png must be a PNG of at most 64 KiB")
        size = app["defaultSize"]
        if not (200 <= size["w"] <= 4000 and 150 <= size["h"] <= 3000):
            fail(f"{package}: defaultSize out of range")
    print(f"crafts.json: {len(seen)} apps, key {upstream['signingKeyFingerprint']}")


def unpack(archive: Path, prefix: str, dest: Path) -> None:
    """Extract the zip's `prefix/` folder into dest, refusing anything odd."""
    total = 0
    with zipfile.ZipFile(archive) as bundle:
        for info in bundle.infolist():
            name = info.filename
            if not name.startswith(prefix + "/"):
                fail(f"{archive.name}: {name} is outside {prefix}/")
            relative = name[len(prefix) + 1 :]
            if not relative:
                continue
            parts = relative.rstrip("/").split("/")
            if any(part in ("", ".", "..") for part in parts) or "\\" in relative:
                fail(f"{archive.name}: unsafe path {name}")
            mode = (info.external_attr >> 16) & 0o170000
            if mode not in (0, 0o100000, 0o040000):
                fail(f"{archive.name}: {name} is not a plain file or folder")
            target = dest.joinpath(*parts)
            if info.is_dir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            total += info.file_size
            if total > MAX_UNPACKED:
                fail(f"{archive.name}: unpacks to more than {MAX_UNPACKED} bytes")
            target.parent.mkdir(parents=True, exist_ok=True)
            with bundle.open(info) as source, target.open("wb") as out:
                shutil.copyfileobj(source, out)
            target.chmod(0o644)


def tar_gz(source: Path, top: str, out: Path) -> None:
    """A reproducible .tar.gz of source as `top/` (sorted, root-owned, fixed times)."""
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", format=tarfile.PAX_FORMAT) as archive:
        paths = sorted(source.rglob("*"), key=lambda p: p.relative_to(source).as_posix())
        for path in [source, *paths]:
            relative = path.relative_to(source).as_posix()
            arcname = top if relative == "." else f"{top}/{relative}"
            info = tarfile.TarInfo(arcname)
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            info.mtime = 946684800
            if path.is_dir():
                info.type = tarfile.DIRTYPE
                info.mode = 0o755
                archive.addfile(info)
            else:
                info.mode = 0o644
                info.size = path.stat().st_size
                with path.open("rb") as handle:
                    archive.addfile(info, handle)
    with out.open("wb") as raw, gzip.GzipFile(fileobj=raw, mode="wb", mtime=0, filename="") as packed:
        packed.write(buffer.getvalue())


def build(config: dict, package: str, out_dir: Path) -> None:
    check(config)
    app = find(config, package)
    upstream = config["upstream"]
    version, repo = app["version"], app["repo"]
    owner = upstream["owner"]
    tag = f"v{version}"
    asset = f"{repo}-web-{version}.zip"
    out_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=f"craft-{package}-") as scratch:
        work = Path(scratch)
        downloads = work / "downloads"
        downloads.mkdir()
        for name in (asset, "SHA256SUMS.txt", "SHA256SUMS.txt.asc"):
            run(["gh", "release", "download", tag, "-R", f"{owner}/{repo}", "-p", name, "-D", str(downloads)])
        # The pinned key signs the checksums.
        keyring = work / "artcraft.gpg"
        with (HERE / upstream["signingKey"]).open("rb") as armored, keyring.open("wb") as out:
            run(["gpg", "--dearmor"], stdin=armored, stdout=out)
        run(["gpgv", "--keyring", str(keyring), str(downloads / "SHA256SUMS.txt.asc"), str(downloads / "SHA256SUMS.txt")])
        listed = {}
        for line in (downloads / "SHA256SUMS.txt").read_text().splitlines():
            match = re.match(r"^([0-9a-f]{64})\s+\*?(\S+)$", line.strip())
            if match:
                listed[match.group(2)] = match.group(1)
        digest = sha256(downloads / asset)
        if listed.get(asset) != digest:
            fail(f"{asset}: SHA-256 {digest} is not the one in the signed SHA256SUMS.txt")
        release = json.loads(
            run(["gh", "api", f"repos/{owner}/{repo}/releases/tags/{tag}"], capture_output=True, text=True).stdout
        )
        github = next((a.get("digest") for a in release["assets"] if a["name"] == asset), None)
        if github != f"sha256:{digest}":
            fail(f"{asset}: GitHub reports {github}, the file is sha256:{digest}")
        if release.get("prerelease") or release.get("draft"):
            fail(f"{owner}/{repo} {tag} is not a published stable release")

        stage = work / "stage" / package
        web = stage / "web"
        web.mkdir(parents=True)
        unpack(downloads / asset, f"{repo}-web-{version}", web)
        if not (web / "index.html").is_file() or not any(web.glob("*_bg.wasm")):
            fail(f"{asset}: no index.html and WebAssembly at its top")
        (stage / "NOTICE-RAST.md").write_text(
            f"# {app['name']} {version} for RAST\n\n"
            f"`web/` is the unmodified web build of {app['name']} {version} by ArtCraft\n"
            f"(https://github.com/{owner}/{repo}, {upstream['license']}; its own LICENSE and NOTICE\n"
            f"files are inside `web/`): `{asset}` from\n"
            f"https://github.com/{owner}/{repo}/releases/tag/{tag}, SHA-256 `{digest}`,\n"
            f"listed in SHA256SUMS.txt signed by ArtCraft's release key\n"
            f"{upstream['signingKeyFingerprint']}.\n\n"
            f"Packaged for RAST's App Center by https://github.com/Enricko/rast-packages\n"
            f"(apps/craft-web). RAST shows `web/index.html` in a sandboxed window of its own\n"
            f"origin: it reaches nothing of RAST and keeps no storage between visits.\n"
        )
        sums = []
        for target in TARGETS:
            name = f"{package}-{version}-{target}.tar.gz"
            tar_gz(stage, package, out_dir / name)
            sums.append(f"{sha256(out_dir / name)}  {name}")
        (out_dir / "SHA256SUMS").write_text("\n".join(sums) + "\n")
    print("\n".join(sums))


def catalog_entries(config: dict, min_rast: str) -> None:
    upstream = config["upstream"]
    entries = {}
    for app in config["apps"]:
        package = app["package"]
        entries[package] = {
            "name": app["name"],
            "description": (
                f"{app['summary']} ArtCraft's {app['name']} (open source, {upstream['license']}), "
                f"its web version in a sandboxed window: open files from your device and save them as downloads. "
                f"Packaged from upstream's signed release by this repository's craft-web workflow."
            ),
            "homepage": app["homepage"],
            "license": upstream["license"],
            "icon": f"icons/{package}.png",
            "keepPatches": 1,
            "githubAttestations": True,
            "releaseRepos": ["Enricko/rast-packages"],
            "minRastVersion": min_rast,
            "app": {
                "id": package,
                "window": {"entry": "web/index.html", "sdk": 1, "singleton": True, "defaultSize": app["defaultSize"]},
            },
            "channels": [{"id": "stable", "label": app["name"], "status": "stable"}],
        }
    print(json.dumps(entries, indent=2))


def main() -> None:
    if len(sys.argv) >= 2 and sys.argv[1] == "check":
        check(load())
    elif len(sys.argv) == 4 and sys.argv[1] == "build":
        build(load(), sys.argv[2], Path(sys.argv[3]).resolve())
    elif len(sys.argv) == 3 and sys.argv[1] == "catalog-entries":
        catalog_entries(load(), sys.argv[2])
    else:
        print(__doc__, file=sys.stderr)
        sys.exit(2)


if __name__ == "__main__":
    os.umask(0o022)
    main()
