#!/usr/bin/env python3
"""Discover, verify, assemble, sign and check the RAST official catalog.

Subcommands (all run in GitHub Actions; see .github/workflows/catalog.yml):

  previous   download the published catalog and verify its signature
  discover   find upstream versions; write work/plan.json and job matrices
  interim    publish-ready catalog that lists new versions as "detected"
  verify     download + verify + smoke-test upstream artifacts for one target
  php-result smoke-test a CI-built PHP archive and record the result
  assemble   final catalog: carried versions + newly verified ones
  sign       Ed25519 signature (openssl) -> catalog.json.sig
  check      validate every manifest and verify the signature

Standard library only (plus openssl, gpgv, gh and tar on the runner).
"""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rastpkg  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
UA = "rast-packages-catalog/1 (+https://github.com/Enricko/rast-packages)"
GO_JSON = "https://go.dev/dl/?mode=json&include=all"
NODE_INDEX = "https://nodejs.org/dist/index.json"
NODE_KEYRING = "https://raw.githubusercontent.com/nodejs/release-keys/HEAD/gpg/pubring.kbx"
PBS_REPO = "astral-sh/python-build-standalone"
PHP_JSON = "https://www.php.net/releases/index.php?json&version={channel}&max=1"
RUN_URL = os.environ.get("GITHUB_SERVER_URL", "https://github.com") + "/" + os.environ.get(
    "GITHUB_REPOSITORY", "Enricko/rast-packages"
) + "/actions/runs/" + os.environ.get("GITHUB_RUN_ID", "0")


# ------------------------------------------------------------------ helpers

def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def load_config() -> dict:
    return json.loads((ROOT / "catalog.config.json").read_text())


def now() -> dt.datetime:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0)


def iso(t: dt.datetime) -> str:
    return t.strftime("%Y-%m-%dT%H:%M:%SZ")


def http_get(url: str, token: str | None = None, accept: str | None = None) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    if token and url.startswith("https://api.github.com/"):
        req.add_header("Authorization", f"Bearer {token}")
    if accept:
        req.add_header("Accept", accept)
    with urllib.request.urlopen(req, timeout=120) as r:
        return r.read()


def http_json(url: str, token: str | None = None):
    return json.loads(http_get(url, token, "application/json"))


def download(url: str, dest: Path) -> tuple[str, int]:
    """Stream to dest; return (sha256, size). Redirects must stay on https."""
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    h = hashlib.sha256()
    size = 0
    with urllib.request.urlopen(req, timeout=600) as r, open(dest, "wb") as out:
        if not r.geturl().startswith("https://"):
            raise RuntimeError(f"{url} redirected to a non-https location")
        while True:
            chunk = r.read(1 << 20)
            if not chunk:
                break
            h.update(chunk)
            size += len(chunk)
            out.write(chunk)
    return h.hexdigest(), size


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def gh_token() -> str | None:
    return os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")


def withheld(cfg: dict, extra: str) -> set[str]:
    items = set(cfg.get("withheld", []))
    items.update(x.strip() for x in extra.split(",") if x.strip())
    return items


def channel_ids(pkg_cfg: dict) -> list[str]:
    return [c["id"] for c in pkg_cfg["channels"]]


def write_json(path: Path, data) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n")


def safe_extract(archive: Path, dest: Path, strip: int) -> None:
    """Extract like RAST: refuse absolute paths, '..', devices; strip prefix."""
    with tarfile.open(archive, "r:gz") as tar:
        for member in tar.getmembers():
            parts = [p for p in member.name.split("/") if p not in ("", ".")]
            if any(p == ".." for p in parts) or member.name.startswith("/"):
                raise RuntimeError(f"unsafe path in archive: {member.name}")
            if not (member.isfile() or member.isdir() or member.issym() or member.islnk()):
                raise RuntimeError(f"unsupported entry type in archive: {member.name}")
            if len(parts) <= strip:
                continue
            member.name = "/".join(parts[strip:])
            if member.islnk():
                lparts = [p for p in member.linkname.split("/") if p not in ("", ".")]
                member.linkname = "/".join(lparts[strip:])
            tar.extract(member, dest, filter="tar" if hasattr(tarfile, "data_filter") else None)


def run_check(root: Path, exe_path: str, args: list[str], expect: str, home: Path) -> str:
    env = {
        "PATH": f"{root}/bin:/usr/local/bin:/usr/bin:/bin",
        "HOME": str(home),
        "TMPDIR": str(home),
        "LANG": "C.UTF-8",
        # RAST runs under MemoryDenyWriteExecute; check the same way.
        "NODE_OPTIONS": "--jitless",
        "npm_config_cache": str(home / ".npm"),
        "npm_config_update_notifier": "false",
    }
    out = subprocess.run([str(root / exe_path), *args], capture_output=True, text=True, timeout=120, env=env, cwd=home)
    text = (out.stdout + out.stderr).strip()
    if out.returncode != 0:
        raise RuntimeError(f"{exe_path} {' '.join(args)} exited {out.returncode}: {text[:400]}")
    if expect not in text:
        raise RuntimeError(f"{exe_path} {' '.join(args)} printed {text[:200]!r}, expected it to contain {expect!r}")
    return text.splitlines()[0] if text else ""


# ------------------------------------------------------------- definitions

def runtime_spec(package: str, version: str, bundled: dict | None = None) -> dict:
    """Executables and version checks per runtime (the checks RAST re-runs)."""
    bundled = bundled or {}
    if package == "python":
        major_minor = ".".join(version.split(".")[:2])
        spec = {
            "executables": [
                {"name": "python3", "path": "bin/python3"},
                {"name": f"python{major_minor}", "path": f"bin/python{major_minor}"},
                {"name": "pip3", "path": "bin/pip3"},
            ],
            "checks": [{"executable": "python3", "args": ["--version"], "expect": f"Python {version}"}],
        }
        if "pip" in bundled:
            spec["checks"].append({"executable": "pip3", "args": ["--version"], "expect": f"pip {bundled['pip']}", "label": "pip"})
    elif package == "nodejs":
        spec = {
            "executables": [
                {"name": "node", "path": "bin/node"},
                {"name": "npm", "path": "bin/npm"},
                {"name": "npx", "path": "bin/npx"},
                {"name": "corepack", "path": "bin/corepack"},
            ],
            "checks": [{"executable": "node", "args": ["--version"], "expect": f"v{version}"}],
        }
        if "npm" in bundled:
            spec["checks"].append({"executable": "npm", "args": ["--version"], "expect": bundled["npm"], "label": "npm"})
    elif package == "go":
        spec = {
            "executables": [{"name": "go", "path": "bin/go"}, {"name": "gofmt", "path": "bin/gofmt"}],
            "checks": [{"executable": "go", "args": ["version"], "expect": f"go{version}"}],
        }
    elif package == "php":
        spec = {
            "executables": [{"name": "php", "path": "bin/php"}],
            "checks": [{"executable": "php", "args": ["-v"], "expect": f"PHP {version}"}],
        }
    else:
        raise ValueError(package)
    if bundled:
        spec["bundled"] = [{"name": k, "version": v} for k, v in sorted(bundled.items())]
    return spec


def channel_of(package: str, version: str) -> str | None:
    """Explicit channel for runtimes whose stream is the major version."""
    return version.split(".")[0] if package == "nodejs" else None


# ---------------------------------------------------------------- discover

def discover_go(cfg, pkg_cfg, held):
    data = http_json(GO_JSON)
    items = []
    for ch in channel_ids(pkg_cfg):
        found = []
        for rel in data:
            v = rel["version"].removeprefix("go")
            if not re.fullmatch(r"\d+\.\d+\.\d+", v) or rastpkg.default_channel(v) != ch or f"go@{v}" in held:
                continue
            targets = {}
            for target, tcfg in cfg["targets"].items():
                f = next((f for f in rel["files"] if f["os"] == "linux" and f["arch"] == tcfg["go"] and f["kind"] == "archive"), None)
                if f:
                    targets[target] = {
                        "url": f"https://dl.google.com/go/{f['filename']}",
                        "sha256": f["sha256"],
                        "size": f["size"],
                        "source": f"https://go.dev/dl/{f['filename']}",
                    }
            if targets:
                found.append({"package": "go", "version": v, "channel": ch, "kind": "upstream", "targets": targets})
        found.sort(key=lambda i: rastpkg.version_key(i["version"]), reverse=True)
        items += found[: pkg_cfg["keepPatches"]]
    return items


def discover_node(cfg, pkg_cfg, held):
    index = http_json(NODE_INDEX)
    items = []
    for ch in channel_ids(pkg_cfg):
        found = []
        for rel in index:
            v = rel["version"].removeprefix("v")
            if v.split(".")[0] != ch or not rel.get("lts") or f"nodejs@{v}" in held:
                continue
            targets = {}
            for target, tcfg in cfg["targets"].items():
                if f"linux-{tcfg['node']}" in rel.get("files", []):
                    name = f"node-v{v}-linux-{tcfg['node']}.tar.gz"
                    targets[target] = {"url": f"https://nodejs.org/dist/v{v}/{name}", "file": name, "source": f"https://nodejs.org/dist/v{v}/"}
            if targets:
                found.append({
                    "package": "nodejs", "version": v, "channel": ch, "kind": "upstream",
                    "releaseDate": rel.get("date"), "indexNpm": rel.get("npm"), "lts": rel.get("lts"), "targets": targets,
                })
        found.sort(key=lambda i: rastpkg.version_key(i["version"]), reverse=True)
        items += found[: pkg_cfg["keepPatches"]]
    return items


def discover_python(cfg, pkg_cfg, held, token):
    rel = http_json(f"https://api.github.com/repos/{PBS_REPO}/releases/latest", token)
    tag = rel["tag_name"]
    items = []
    for ch in channel_ids(pkg_cfg):
        best = None
        for asset in rel["assets"]:
            m = re.fullmatch(rf"cpython-({re.escape(ch)}\.\d+)\+{re.escape(tag)}-(\w+)-unknown-linux-gnu-install_only\.tar\.gz", asset["name"])
            if m and f"python@{m.group(1)}" not in held:
                if best is None or rastpkg.version_key(m.group(1)) > rastpkg.version_key(best):
                    best = m.group(1)
        if not best:
            continue
        targets = {}
        for target, tcfg in cfg["targets"].items():
            name = f"cpython-{best}+{tag}-{tcfg['python']}-unknown-linux-gnu-install_only.tar.gz"
            asset = next((a for a in rel["assets"] if a["name"] == name), None)
            if asset:
                targets[target] = {
                    "url": asset["browser_download_url"],
                    "file": name,
                    "size": asset["size"],
                    "digest": (asset.get("digest") or "").removeprefix("sha256:"),
                    "source": rel["html_url"],
                }
        items.append({
            "package": "python", "version": best, "channel": ch, "kind": "upstream",
            "releaseDate": (rel.get("published_at") or "")[:10], "buildTag": tag, "targets": targets,
            "sumsUrl": f"https://github.com/{PBS_REPO}/releases/download/{tag}/SHA256SUMS",
        })
    return items


def discover_php(cfg, pkg_cfg, held):
    items = []
    for ch in channel_ids(pkg_cfg):
        data = http_json(PHP_JSON.format(channel=ch))
        # `max=1` returns {version: {...}}; older endpoint shape returns one release.
        releases = data if "version" not in data else {data["version"]: data}
        for v, rel in sorted(releases.items(), key=lambda kv: rastpkg.version_key(kv[0]), reverse=True)[: pkg_cfg["keepPatches"]]:
            if f"php@{v}" in held:
                continue
            sources = {s["filename"]: s.get("sha256") for s in rel.get("source", [])}
            xz = f"php-{v}.tar.xz"
            if not sources.get(xz):
                raise RuntimeError(f"php.net lists no sha256 for {xz}")
            items.append({
                "package": "php", "version": v, "channel": ch, "kind": "built",
                "releaseDate": rel.get("date"),
                "source": {"url": f"https://www.php.net/distributions/{xz}", "sha256": sources[xz], "all": sources},
                "targets": {t: {} for t in cfg["targets"]},
            })
    return items


def manifest_key(m: dict) -> str:
    return f"{m['id']}@{m['version']}"


def cmd_previous(args):
    """Fetch and verify the published catalog (absent on the first run)."""
    cfg = load_config()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    r = subprocess.run(
        ["gh", "release", "download", "catalog", "--pattern", "catalog.json", "--pattern", "catalog.json.sig", "--dir", str(out), "--clobber"],
        capture_output=True, text=True,
    )
    if r.returncode != 0:
        if "release not found" in (r.stderr + r.stdout).lower():
            log("no published catalog yet")
            return
        raise SystemExit(f"cannot download the published catalog: {r.stderr.strip()}")
    verify_signature(out / "catalog.json", out / "catalog.json.sig", ROOT / cfg["publicKey"], cfg["keyId"])
    log(f"previous catalog sequence {json.loads((out / 'catalog.json').read_text())['sequence']} verified")


def cmd_discover(args):
    cfg = load_config()
    held = withheld(cfg, args.withhold)
    token = gh_token()
    prev = load_previous(args.previous)
    items = []
    items += discover_python(cfg, cfg["packages"]["python"], held, token)
    items += discover_php(cfg, cfg["packages"]["php"], held)
    items += discover_go(cfg, cfg["packages"]["go"], held)
    items += discover_node(cfg, cfg["packages"]["nodejs"], held)

    prev_versions = {}
    for pkg in (prev or {}).get("packages", []):
        for m in pkg.get("versions", []):
            prev_versions[manifest_key(m)] = m
    for item in items:
        key = f"{item['package']}@{item['version']}"
        revision = cfg.get("revisions", {}).get(key, 1)
        item["revision"] = revision
        old = prev_versions.get(key)
        targets_done = old and {a["target"] for a in old["artifacts"]} >= set(item["targets"])
        item["status"] = "carry" if (old and old["revision"] >= revision and targets_done and not args.force) else "new"
    new = [i for i in items if i["status"] == "new"]
    prev_seq = (prev or {}).get("sequence", 0)
    expires = (prev or {}).get("expiresAt")
    refresh = bool(prev) and expires is not None and dt.datetime.fromisoformat(expires.replace("Z", "+00:00")) - now() < dt.timedelta(days=cfg["refreshBeforeDays"])
    dropped = sorted(set(prev_versions) - {f"{i['package']}@{i['version']}" for i in items})
    plan = {"generatedAt": iso(now()), "previousSequence": prev_seq, "withheld": sorted(held), "items": items, "dropped": dropped}
    write_json(Path(args.out), plan)

    verify_targets = sorted({t for i in new if i["kind"] == "upstream" for t in i["targets"]})
    verify_matrix = {"include": [{"target": t, "runner": cfg["targets"][t]["runner"]} for t in verify_targets]}
    php_matrix = {"include": [
        {"version": i["version"], "target": t, "runner": cfg["targets"][t]["runner"]}
        for i in new if i["package"] == "php" for t in i["targets"]
    ]}
    changed = bool(new) or bool(dropped) or refresh or not prev
    log(f"discovered {len(items)} versions; new: {[f'{i['package']} {i['version']}' for i in new]}; dropped: {dropped}; refresh: {refresh}")
    if args.github_output:
        with open(args.github_output, "a") as f:
            f.write(f"changed={'true' if changed else 'false'}\n")
            f.write(f"has_new={'true' if new else 'false'}\n")
            f.write(f"verify_matrix={json.dumps(verify_matrix)}\n")
            f.write(f"php_matrix={json.dumps(php_matrix)}\n")
            f.write(f"has_verify={'true' if verify_targets else 'false'}\n")
            f.write(f"has_php={'true' if php_matrix['include'] else 'false'}\n")


def load_previous(path: str | None):
    if not path:
        return None
    p = Path(path) / "catalog.json"
    return json.loads(p.read_text()) if p.exists() else None


# ------------------------------------------------------------------ verify

def node_shasums(version: str, work: Path) -> dict[str, str]:
    """SHASUMS256.txt for a Node release, verified with the Node release keys."""
    base = f"https://nodejs.org/dist/v{version}/"
    sums = work / f"node-{version}-SHASUMS256.txt"
    sig = work / f"node-{version}-SHASUMS256.txt.sig"
    keyring = work / "node-release-keys.kbx"
    sums.write_bytes(http_get(base + "SHASUMS256.txt"))
    sig.write_bytes(http_get(base + "SHASUMS256.txt.sig"))
    if not keyring.exists():
        keyring.write_bytes(http_get(NODE_KEYRING))
    r = subprocess.run(["gpgv", "--keyring", str(keyring), str(sig), str(sums)], capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"SHASUMS256.txt signature does not verify: {r.stderr.strip()[:300]}")
    out = {}
    for line in sums.read_text().splitlines():
        parts = line.split()
        if len(parts) == 2:
            out[parts[1]] = parts[0]
    return out


def pbs_sums(url: str, cache: dict) -> dict[str, str]:
    if url not in cache:
        text = http_get(url).decode()
        cache[url] = {p[1]: p[0] for p in (line.split() for line in text.splitlines()) if len(p) == 2}
    return cache[url]


def cmd_verify(args):
    """Verify every new upstream artifact for one target on a native runner."""
    plan = json.loads(Path(args.plan).read_text())
    target = args.target
    work = Path(tempfile.mkdtemp(prefix="rast-verify-"))
    results = []
    sums_cache: dict = {}
    for item in plan["items"]:
        if item["status"] != "new" or item["kind"] != "upstream" or target not in item["targets"]:
            continue
        t = item["targets"][target]
        label = f"{item['package']} {item['version']} {target}"
        log(f"== {label}")
        res = {"package": item["package"], "version": item["version"], "target": target, "ok": False}
        try:
            archive = work / "artifact.tar.gz"
            verification = ""
            if item["package"] == "go":
                expected = t["sha256"]
                verification = "SHA-256 published in the go.dev/dl release index; downloaded and re-hashed in CI"
            elif item["package"] == "nodejs":
                sums = node_shasums(item["version"], work)
                expected = sums.get(t["file"])
                if not expected:
                    raise RuntimeError(f"{t['file']} is not listed in SHASUMS256.txt")
                verification = "SHA-256 from SHASUMS256.txt, whose signature was verified with gpgv against the nodejs/release-keys keyring"
            elif item["package"] == "python":
                sums = pbs_sums(item["sumsUrl"], sums_cache)
                expected = sums.get(t["file"])
                if not expected:
                    raise RuntimeError(f"{t['file']} is not listed in SHA256SUMS")
                if t.get("digest") and t["digest"] != expected:
                    raise RuntimeError("GitHub asset digest differs from SHA256SUMS")
                verification = "SHA-256 from the release SHA256SUMS, equal to the GitHub asset digest; GitHub build-provenance attestation verified with gh attestation verify"
            else:
                raise RuntimeError("not an upstream package")
            sha, size = download(t["url"], archive)
            if sha != expected:
                raise RuntimeError(f"SHA-256 mismatch: expected {expected}, got {sha}")
            if t.get("size") and t["size"] != size:
                raise RuntimeError(f"size mismatch: expected {t['size']}, got {size}")
            if item["package"] == "python":
                r = subprocess.run(["gh", "attestation", "verify", str(archive), "--repo", PBS_REPO], capture_output=True, text=True)
                if r.returncode != 0:
                    raise RuntimeError(f"attestation verification failed: {(r.stderr or r.stdout).strip()[:300]}")
            root = work / "root"
            if root.exists():
                shutil.rmtree(root)
            root.mkdir()
            safe_extract(archive, root, 1)
            home = work / "home"
            home.mkdir(exist_ok=True)
            bundled = {}
            if item["package"] == "nodejs":
                npm = run_check(root, "bin/npm", ["--version"], "", home)
                bundled["npm"] = npm.strip()
                if item.get("indexNpm") and item["indexNpm"] != bundled["npm"]:
                    log(f"note: index.json says npm {item['indexNpm']}, the archive ships {bundled['npm']}")
            if item["package"] == "python":
                pip = run_check(root, "bin/pip3", ["--version"], "pip ", home)
                bundled["pip"] = pip.split()[1]
            spec = runtime_spec(item["package"], item["version"], bundled)
            for exe in spec["executables"]:
                p = root / exe["path"]
                if not (p.exists() and os.access(p, os.X_OK)):
                    raise RuntimeError(f"executable {exe['path']} is missing")
            outputs = [run_check(root, next(e["path"] for e in spec["executables"] if e["name"] == c["executable"]), c.get("args", []), c["expect"], home) for c in spec["checks"]]
            res.update({"ok": True, "sha256": sha, "size": size, "bundled": bundled, "checks": outputs, "verification": verification})
            log(f"   ok: {outputs}")
        except Exception as e:  # noqa: BLE001 - every failure is recorded
            res["error"] = str(e)[:500]
            log(f"   FAILED: {res['error']}")
        results.append(res)
        shutil.rmtree(work / "root", ignore_errors=True)
        (work / "artifact.tar.gz").unlink(missing_ok=True)
    write_json(Path(args.out), results)


def cmd_php_result(args):
    """Smoke-test a CI-built PHP archive (native runner) and record it."""
    archive = Path(args.archive)
    res = {"package": "php", "version": args.version, "target": args.target, "ok": False}
    work = Path(tempfile.mkdtemp(prefix="rast-php-"))
    try:
        root = work / "root"
        root.mkdir()
        safe_extract(archive, root, 1)
        home = work / "home"
        home.mkdir()
        first = run_check(root, "bin/php", ["-v"], f"PHP {args.version}", home)
        modules = subprocess.run([str(root / "bin/php"), "-m"], capture_output=True, text=True, timeout=60, check=True).stdout
        exts = sorted({line.strip().lower() for line in modules.splitlines() if line.strip() and not line.startswith("[")})
        info = json.loads((root / "BUILDINFO.json").read_text())
        res.update({
            "ok": True, "sha256": sha256_file(archive), "size": archive.stat().st_size, "checks": [first],
            "extensions": exts, "attestationUrl": args.attestation_url or None, "buildUrl": RUN_URL, "buildinfo": info,
            "file": archive.name,
        })
    except Exception as e:  # noqa: BLE001
        res["error"] = str(e)[:500]
    write_json(Path(args.out), res)
    if not res["ok"]:
        raise SystemExit(f"PHP smoke test failed: {res['error']}")


# ---------------------------------------------------------------- assemble

def base_manifest(cfg, item) -> dict:
    pkg_cfg = cfg["packages"][item["package"]]
    m = {
        "schemaVersion": 1,
        "kind": "rast-package",
        "id": item["package"],
        "type": "runtime",
        "name": pkg_cfg["name"],
        "description": pkg_cfg["description"],
        "homepage": pkg_cfg["homepage"],
        "license": pkg_cfg["license"],
        "publisher": cfg["publisher"],
        "version": item["version"],
        "revision": item["revision"],
        "minRastVersion": cfg["minRastVersion"],
    }
    ch = channel_of(item["package"], item["version"])
    if ch:
        m["channel"] = ch
    if item.get("releaseDate"):
        m["releaseDate"] = str(item["releaseDate"])[:32]
    return m


def build_manifest(cfg, item, results: list[dict], php_release: str | None) -> tuple[dict | None, str]:
    ok = [r for r in results if r["ok"]]
    failed = [r for r in results if not r["ok"]]
    if not ok:
        detail = "; ".join(f"{r['target']}: {r.get('error', 'no result')}" for r in failed) or "no target produced a verified artifact"
        return None, detail
    bundled = {}
    for r in ok:
        bundled.update(r.get("bundled") or {})
    m = base_manifest(cfg, item)
    m["runtime"] = runtime_spec(item["package"], item["version"], bundled)
    artifacts = []
    for r in sorted(ok, key=lambda r: r["target"]):
        t = item["targets"][r["target"]]
        if item["package"] == "php":
            url = f"{cfg['repository']}/releases/download/{php_release}/{r['file']}"
            prov = {
                "kind": "built",
                "source": item["source"]["url"],
                "sourceSha256": item["source"]["sha256"],
                "verification": f"php.net source SHA-256 checked before the build; static-php-cli {r['buildinfo'].get('spcVersion')} verified by GitHub asset digest; built in {r['buildinfo'].get('builder')}",
                "buildUrl": r["buildUrl"],
            }
            if r.get("attestationUrl"):
                prov["attestationUrl"] = r["attestationUrl"]
            requires = {"notes": "Static musl binary: no host libraries needed. TLS uses the host CA bundle (/etc/ssl/certs)."}
        else:
            url = t["url"]
            prov = {"kind": "upstream", "source": t["source"], "verification": r["verification"], "buildUrl": RUN_URL}
            requires = {}
            if item["package"] == "python":
                requires = {"glibc": "2.17", "notes": f"python-build-standalone {item['buildTag']} (x86_64/aarch64 gnu install_only)."}
            elif item["package"] == "nodejs":
                requires = {"glibc": "2.28"}
            elif item["package"] == "go":
                requires = {"notes": "Statically linked toolchain; building cgo programs needs a host C compiler."}
        a = {
            "target": r["target"], "url": url, "sha256": r["sha256"], "size": r["size"], "format": "tar.gz",
            "stripComponents": 1, "provenance": prov,
        }
        if requires:
            a["requires"] = requires
        artifacts.append(a)
    m["artifacts"] = artifacts
    exts = sorted({e for r in ok for e in r.get("extensions") or []})
    if exts:
        m["runtime"]["extensions"] = exts
    notes = []
    if failed:
        notes.append("Not available for: " + ", ".join(f"{r['target']} ({r.get('error', 'failed')[:80]})" for r in failed) + ".")
    if item["package"] == "nodejs":
        notes.append(f"Bundled npm {bundled.get('npm', '?')} as found in the archive.")
    if item["package"] == "php":
        notes.append("Extensions: see the list; php.ini is not shipped (PHP defaults apply).")
    if notes:
        m["notes"] = " ".join(notes)[:1000]
    rastpkg.validate_manifest(m)
    return m, ""


def package_entries(cfg) -> dict:
    out = {}
    for pid, p in cfg["packages"].items():
        out[pid] = {
            "id": pid, "type": "runtime", "name": p["name"], "description": p["description"],
            "homepage": p["homepage"], "license": p["license"], "channels": p["channels"],
            "versions": [], "detected": [],
        }
    return out


def catalog_doc(cfg, sequence: int, packages: dict) -> dict:
    t = now()
    for p in packages.values():
        p["versions"].sort(key=lambda m: (rastpkg.version_key(m["version"]), m["revision"]), reverse=True)
    return {
        "schemaVersion": 1,
        "kind": "rast-catalog",
        "name": cfg["name"],
        "publisher": cfg["publisher"],
        "repository": cfg["repository"],
        "sequence": sequence,
        "generatedAt": iso(t),
        "expiresAt": iso(t + dt.timedelta(days=cfg["expiresDays"])),
        "allowedHosts": cfg["allowedHosts"],
        "packages": list(packages.values()),
    }


def carried(prev, plan) -> dict[str, dict]:
    keep = {f"{i['package']}@{i['version']}" for i in plan["items"] if i["status"] == "carry"}
    out = {}
    for pkg in (prev or {}).get("packages", []):
        for m in pkg.get("versions", []):
            if manifest_key(m) in keep:
                out[manifest_key(m)] = m
    return out


def cmd_interim(args):
    """Catalog that announces new upstream versions while CI verifies them."""
    cfg = load_config()
    plan = json.loads(Path(args.plan).read_text())
    prev = load_previous(args.previous)
    packages = package_entries(cfg)
    for m in carried(prev, plan).values():
        packages[m["id"]]["versions"].append(m)
    # Versions that are no longer selected stay until the final catalog.
    for pkg in (prev or {}).get("packages", []):
        for m in pkg.get("versions", []):
            if manifest_key(m) in plan.get("dropped", []):
                packages[m["id"]]["versions"].append(m)
    stamp = iso(now())
    for i in plan["items"]:
        if i["status"] == "new":
            packages[i["package"]]["detected"].append({
                "version": i["version"],
                "channel": channel_of(i["package"], i["version"]) or i["channel"],
                "detectedAt": stamp,
                "state": "building" if i["kind"] == "built" else "verifying",
                "detail": (
                    f"New upstream release; the catalog workflow is building and testing it ({RUN_URL})."
                    if i["kind"] == "built"
                    else f"New upstream release; the catalog workflow is verifying checksums and running it ({RUN_URL})."
                ),
            })
    doc = catalog_doc(cfg, plan["previousSequence"] + 1, packages)
    write_json(Path(args.out), doc)


def cmd_assemble(args):
    cfg = load_config()
    plan = json.loads(Path(args.plan).read_text())
    prev = load_previous(args.previous)
    results: list[dict] = []
    for f in sorted(Path(args.results).glob("**/*.json")):
        data = json.loads(f.read_text())
        results += data if isinstance(data, list) else [data]
    php_releases = json.loads(args.php_releases) if args.php_releases else {}
    packages = package_entries(cfg)
    for m in carried(prev, plan).values():
        packages[m["id"]]["versions"].append(m)
    stamp = iso(now())
    summary = []
    for i in plan["items"]:
        if i["status"] != "new":
            continue
        mine = [r for r in results if r["package"] == i["package"] and r["version"] == i["version"]]
        for target in i["targets"]:
            if not any(r["target"] == target for r in mine):
                mine.append({"package": i["package"], "version": i["version"], "target": target, "ok": False, "error": "no result (job failed or was skipped)"})
        m, detail = build_manifest(cfg, i, mine, php_releases.get(i["version"]))
        if m:
            packages[i["package"]]["versions"].append(m)
            summary.append(f"verified {i['package']} {i['version']} ({', '.join(a['target'] for a in m['artifacts'])})")
        else:
            packages[i["package"]]["detected"].append({
                "version": i["version"], "channel": channel_of(i["package"], i["version"]) or i["channel"],
                "detectedAt": stamp, "state": "failed", "detail": f"Verification failed: {detail}"[:500],
            })
            summary.append(f"FAILED {i['package']} {i['version']}: {detail}")
    # The interim catalog (published when there were new versions) used +1.
    has_new = any(i["status"] == "new" for i in plan["items"])
    doc = catalog_doc(cfg, plan["previousSequence"] + (2 if has_new else 1), packages)
    problems = rastpkg.validate_catalog(doc)
    if problems:
        raise SystemExit("catalog has invalid versions:\n" + "\n".join(problems))
    write_json(Path(args.out), doc)
    log("\n".join(summary) or "no new versions")
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as f:
            f.write(f"### Catalog sequence {doc['sequence']}\n\n" + "\n".join(f"- {s}" for s in summary) + "\n")


# -------------------------------------------------------------- sign/check

def cmd_sign(args):
    cfg = load_config()
    key_pem = os.environ.get("CATALOG_SIGNING_KEY", "")
    if "PRIVATE KEY" not in key_pem:
        raise SystemExit("CATALOG_SIGNING_KEY is not set")
    with tempfile.TemporaryDirectory() as d:
        key = Path(d) / "key.pem"
        key.touch(mode=0o600)
        key.write_text(key_pem)
        raw = Path(d) / "sig.bin"
        subprocess.run(["openssl", "pkeyutl", "-sign", "-inkey", str(key), "-rawin", "-in", args.catalog, "-out", str(raw)], check=True)
        sig = {"keyId": cfg["keyId"], "algorithm": "ed25519", "signature": base64.b64encode(raw.read_bytes()).decode()}
    Path(args.catalog + ".sig").write_text(json.dumps(sig) + "\n")
    verify_signature(Path(args.catalog), Path(args.catalog + ".sig"), ROOT / cfg["publicKey"], cfg["keyId"])
    log(f"signed {args.catalog} with {cfg['keyId']}")


def verify_signature(catalog: Path, sig_file: Path, pubkey: Path, key_id: str) -> None:
    sig = json.loads(sig_file.read_text())
    if sig.get("algorithm") != "ed25519" or sig.get("keyId") != key_id:
        raise SystemExit(f"unexpected signature header: {sig.get('keyId')} {sig.get('algorithm')}")
    with tempfile.TemporaryDirectory() as d:
        raw = Path(d) / "sig.bin"
        raw.write_bytes(base64.b64decode(sig["signature"]))
        r = subprocess.run(
            ["openssl", "pkeyutl", "-verify", "-pubin", "-inkey", str(pubkey), "-rawin", "-in", str(catalog), "-sigfile", str(raw)],
            capture_output=True, text=True,
        )
    if r.returncode != 0:
        raise SystemExit(f"catalog signature does not verify: {r.stdout.strip()} {r.stderr.strip()}")


def cmd_check(args):
    cfg = load_config()
    doc = json.loads(Path(args.catalog).read_text())
    problems = rastpkg.validate_catalog(doc)
    if problems:
        raise SystemExit("\n".join(problems))
    if Path(args.catalog + ".sig").exists():
        verify_signature(Path(args.catalog), Path(args.catalog + ".sig"), ROOT / cfg["publicKey"], cfg["keyId"])
    prev = load_previous(args.previous)
    if prev and doc["sequence"] <= prev["sequence"]:
        raise SystemExit(f"sequence {doc['sequence']} must be greater than the published {prev['sequence']}")
    if args.download:
        # Every artifact URL must resolve (redirects inside allowedHosts) to the pinned bytes.
        hosts = set(doc["allowedHosts"])
        with tempfile.TemporaryDirectory() as d:
            for pkg in doc["packages"]:
                for m in pkg["versions"]:
                    for a in m["artifacts"]:
                        if args.download == "new" and not a["url"].startswith(cfg["repository"]):
                            continue
                        final = urllib.request.urlopen(urllib.request.Request(a["url"], method="HEAD", headers={"User-Agent": UA}), timeout=60).geturl()
                        if rastpkg.https_host(final) not in hosts:
                            raise SystemExit(f"{a['url']} redirects to {final}, outside allowedHosts")
                        sha, size = download(a["url"], Path(d) / "a")
                        if sha != a["sha256"] or size != a["size"]:
                            raise SystemExit(f"{a['url']}: published bytes differ from the manifest")
                        log(f"ok {a['url']}")
    log(f"catalog sequence {doc['sequence']}: {sum(len(p['versions']) for p in doc['packages'])} versions valid")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("previous")
    p.add_argument("--out", default="work/previous")
    p.set_defaults(fn=cmd_previous)
    p = sub.add_parser("discover")
    p.add_argument("--previous", default="work/previous")
    p.add_argument("--out", default="work/plan.json")
    p.add_argument("--withhold", default="")
    p.add_argument("--force", action="store_true")
    p.add_argument("--github-output")
    p.set_defaults(fn=cmd_discover)
    p = sub.add_parser("interim")
    p.add_argument("--plan", default="work/plan.json")
    p.add_argument("--previous", default="work/previous")
    p.add_argument("--out", default="work/out/catalog.json")
    p.set_defaults(fn=cmd_interim)
    p = sub.add_parser("verify")
    p.add_argument("--plan", default="work/plan.json")
    p.add_argument("--target", required=True)
    p.add_argument("--out", required=True)
    p.set_defaults(fn=cmd_verify)
    p = sub.add_parser("php-result")
    p.add_argument("--archive", required=True)
    p.add_argument("--version", required=True)
    p.add_argument("--target", required=True)
    p.add_argument("--attestation-url", default="")
    p.add_argument("--out", required=True)
    p.set_defaults(fn=cmd_php_result)
    p = sub.add_parser("assemble")
    p.add_argument("--plan", default="work/plan.json")
    p.add_argument("--previous", default="work/previous")
    p.add_argument("--results", default="work/results")
    p.add_argument("--php-releases", default="")
    p.add_argument("--out", default="work/out/catalog.json")
    p.set_defaults(fn=cmd_assemble)
    p = sub.add_parser("sign")
    p.add_argument("catalog")
    p.set_defaults(fn=cmd_sign)
    p = sub.add_parser("check")
    p.add_argument("catalog")
    p.add_argument("--previous", default="")
    p.add_argument("--download", choices=["new", "all"], default=None)
    p.set_defaults(fn=cmd_check)
    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
