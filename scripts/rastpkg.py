"""Validation rules of the RAST package manifest, schema version 1.

This is a faithful port of ``src/packages/manifest.rs`` in Enricko/rast so a
catalog or release is validated with the same rules *before* it is signed or
published. Standard library only.
"""

from __future__ import annotations

import re

SCHEMA_VERSION = 1
MANIFEST_KIND = "rast-package"
CATALOG_KIND = "rast-catalog"
MAX_MANIFEST_BYTES = 64 * 1024
MAX_ARTIFACT_BYTES = 2 * 1024 * 1024 * 1024
KNOWN_TARGETS = ("linux-x86_64", "linux-aarch64")
SUPPORTED_TYPES = ("runtime",)
PLANNED_TYPES = ("app", "service")
RESERVED_IDS = ("check", "consumers", "github", "host", "catalog", "bin")

MANIFEST_KEYS = {
    "schemaVersion", "kind", "id", "type", "name", "description", "homepage", "license",
    "publisher", "version", "revision", "channel", "minRastVersion", "releaseDate",
    "runtime", "artifacts", "notes",
}
REQUIRED_MANIFEST_KEYS = {"schemaVersion", "kind", "id", "type", "name", "publisher", "version", "revision", "runtime", "artifacts"}
PUBLISHER_KEYS = {"id", "name", "url"}
RUNTIME_KEYS = {"executables", "checks", "bundled", "extensions"}
EXECUTABLE_KEYS = {"name", "path"}
CHECK_KEYS = {"executable", "args", "expect", "label"}
BUNDLED_KEYS = {"name", "version"}
ARTIFACT_KEYS = {"target", "url", "sha256", "size", "format", "stripComponents", "requires", "provenance"}
REQUIRES_KEYS = {"glibc", "notes"}
PROVENANCE_KEYS = {"kind", "source", "sourceSha256", "verification", "buildUrl", "attestationUrl"}


class Invalid(ValueError):
    """A manifest or catalog violates the schema."""


# ----------------------------------------------------------------- versions

_CORE = re.compile(r"^(\d+(?:\.\d+){0,3})(.*)$")


def parse_version(raw: str):
    """(numbers, prerelease) like RAST's Version::parse; None when invalid."""
    raw = raw.strip().lstrip("v")
    if not raw or len(raw) > 32:
        return None
    raw = raw.split("+", 1)[0]
    m = _CORE.match(raw)
    if not m:
        return None
    numbers = [int(x) for x in m.group(1).split(".")]
    rest = m.group(2).lstrip("-.")
    if rest and not re.fullmatch(r"[0-9A-Za-z.\-]{1,32}", rest):
        return None
    return numbers, rest


def version_key(raw: str):
    parsed = parse_version(raw)
    if parsed is None:
        return ((), 0, "")
    numbers, pre = parsed
    padded = tuple(numbers + [0] * (4 - len(numbers)))
    # A release sorts after its pre-releases.
    return (padded, 0 if pre else 1, pre)


def default_channel(raw: str) -> str:
    parsed = parse_version(raw)
    if parsed is None:
        return ""
    numbers = parsed[0]
    return ".".join(str(n) for n in numbers[:2])


# -------------------------------------------------------------- primitives

def _is_id(value, max_len=40) -> bool:
    return (
        isinstance(value, str)
        and 0 < len(value) <= max_len
        and re.fullmatch(r"[a-z0-9][a-z0-9-]*", value) is not None
        and not value.endswith("-")
    )


def _text(field, value, lo, hi):
    if not isinstance(value, str):
        raise Invalid(f"{field} must be a string")
    if not lo <= len(value) <= hi:
        raise Invalid(f"{field} must be {lo}-{hi} characters")
    if any((ord(c) < 32 and c != "\n") or ord(c) == 127 for c in value):
        raise Invalid(f"{field} must not contain control characters")


def is_https_url(value) -> bool:
    return (
        isinstance(value, str)
        and len(value) <= 2048
        and value.startswith("https://")
        and len(value) > len("https://")
        and not any(c.isspace() or ord(c) < 32 for c in value)
    )


def _optional_url(field, value):
    if value is not None and not is_https_url(value):
        raise Invalid(f"{field} must be an https:// URL")


def is_safe_relative_path(path) -> bool:
    return (
        isinstance(path, str)
        and 0 < len(path) <= 255
        and not path.startswith("/")
        and "\\" not in path
        and not any(ord(c) < 32 for c in path)
        and all(part not in ("", ".", "..") for part in path.split("/"))
    )


def is_safe_version(value) -> bool:
    return (
        isinstance(value, str)
        and 0 < len(value) <= 32
        and value[0].isdigit()
        and re.fullmatch(r"[0-9A-Za-z.+\-]+", value) is not None
        and ".." not in value
        and parse_version(value) is not None
    )


def _is_sha256(value) -> bool:
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def _keys(obj, allowed, where, required=()):
    if not isinstance(obj, dict):
        raise Invalid(f"{where} must be an object")
    unknown = set(obj) - set(allowed)
    if unknown:
        raise Invalid(f"{where}: unknown field(s) {', '.join(sorted(unknown))}")
    missing = set(required) - set(obj)
    if missing:
        raise Invalid(f"{where}: missing field(s) {', '.join(sorted(missing))}")


def _uint(field, value, lo, hi):
    if not isinstance(value, int) or isinstance(value, bool) or not lo <= value <= hi:
        raise Invalid(f"{field} must be an integer between {lo} and {hi}")


# ---------------------------------------------------------------- manifest

def validate_manifest(m: dict) -> dict:
    """Raise Invalid with the same messages RAST uses; return the manifest."""
    if not isinstance(m, dict):
        raise Invalid("manifest must be a JSON object")
    if m.get("kind") != MANIFEST_KIND:
        raise Invalid(f'kind must be "{MANIFEST_KIND}"')
    if m.get("schemaVersion") != SCHEMA_VERSION:
        raise Invalid(f"manifest schema version {m.get('schemaVersion')!r} is not supported (supports {SCHEMA_VERSION})")
    ptype = m.get("type")
    if ptype in PLANNED_TYPES:
        raise Invalid(f'package type "{ptype}" is not supported yet; RAST installs: runtime')
    if ptype not in SUPPORTED_TYPES:
        raise Invalid(f"unknown package type {ptype!r}; RAST installs: runtime")
    _keys(m, MANIFEST_KEYS, "manifest", REQUIRED_MANIFEST_KEYS)
    if not _is_id(m["id"]):
        raise Invalid(f"id {m['id']!r} must be 1-40 lowercase letters, digits or dashes")
    if m["id"] in RESERVED_IDS:
        raise Invalid(f"id {m['id']!r} is reserved")
    _text("name", m["name"], 1, 64)
    _text("description", m.get("description", ""), 0, 500)
    _optional_url("homepage", m.get("homepage"))
    if m.get("license") is not None:
        _text("license", m["license"], 1, 64)
    pub = m["publisher"]
    _keys(pub, PUBLISHER_KEYS, "publisher", ("id", "name"))
    if not _is_id(pub["id"]):
        raise Invalid(f"publisher.id {pub['id']!r} must be 1-40 lowercase letters, digits or dashes")
    _text("publisher.name", pub["name"], 1, 64)
    _optional_url("publisher.url", pub.get("url"))
    if not is_safe_version(m["version"]):
        raise Invalid(f"version {m['version']!r} is not a valid version")
    _uint("revision", m["revision"], 1, 10_000)
    channel = m.get("channel")
    if channel is not None and (
        not isinstance(channel, str) or not 0 < len(channel) <= 16 or re.fullmatch(r"[A-Za-z0-9.\-]+", channel) is None
    ):
        raise Invalid(f"channel {channel!r} is not valid")
    if m.get("minRastVersion") is not None and parse_version(str(m["minRastVersion"])) is None:
        raise Invalid(f"minRastVersion {m['minRastVersion']!r} is not a version")
    if m.get("releaseDate") is not None:
        _text("releaseDate", m["releaseDate"], 1, 32)
    if m.get("notes") is not None:
        _text("notes", m["notes"], 0, 1000)
    _validate_runtime(m["runtime"])
    _validate_artifacts(m["artifacts"])
    return m


def _validate_runtime(r):
    _keys(r, RUNTIME_KEYS, "runtime", ("executables", "checks"))
    exes = r["executables"]
    if not isinstance(exes, list) or not 1 <= len(exes) <= 32:
        raise Invalid("runtime.executables must list 1-32 executables")
    names = set()
    for exe in exes:
        _keys(exe, EXECUTABLE_KEYS, "executable", EXECUTABLE_KEYS)
        name = exe["name"]
        if not (isinstance(name, str) and 0 < len(name) <= 32 and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+\-]*", name)):
            raise Invalid(f"executable name {name!r} is not valid")
        if name in names:
            raise Invalid(f"executable {name!r} is listed twice")
        names.add(name)
        if not is_safe_relative_path(exe["path"]):
            raise Invalid(f"executable path {exe['path']!r} must be a relative path inside the package")
    checks = r["checks"]
    if not isinstance(checks, list) or not 1 <= len(checks) <= 8:
        raise Invalid("runtime.checks must list 1-8 version checks")
    for check in checks:
        _keys(check, CHECK_KEYS, "check", ("executable", "expect"))
        if check["executable"] not in names:
            raise Invalid(f"check uses {check['executable']!r}, which is not a declared executable")
        args = check.get("args", [])
        if not isinstance(args, list) or len(args) > 8:
            raise Invalid("a check may pass at most 8 arguments")
        for arg in args:
            _text("check argument", arg, 1, 64)
        _text("check expect", check["expect"], 1, 128)
        if check.get("label") is not None:
            _text("check label", check["label"], 1, 32)
    bundled = r.get("bundled", [])
    if not isinstance(bundled, list) or len(bundled) > 16:
        raise Invalid("runtime.bundled may list at most 16 tools")
    for tool in bundled:
        _keys(tool, BUNDLED_KEYS, "bundled", BUNDLED_KEYS)
        _text("bundled name", tool["name"], 1, 32)
        _text("bundled version", tool["version"], 1, 32)
    extensions = r.get("extensions", [])
    if not isinstance(extensions, list) or len(extensions) > 200:
        raise Invalid("runtime.extensions may list at most 200 entries")
    for ext in extensions:
        _text("extension", ext, 1, 48)


def _validate_artifacts(artifacts):
    if not isinstance(artifacts, list) or not 1 <= len(artifacts) <= 8:
        raise Invalid("artifacts must list 1-8 host targets")
    seen = set()
    for a in artifacts:
        _keys(a, ARTIFACT_KEYS, "artifact", ("target", "url", "sha256", "size", "format", "provenance"))
        target = a["target"]
        if target not in KNOWN_TARGETS:
            raise Invalid(f"unknown target {target!r}; known targets: {', '.join(KNOWN_TARGETS)}")
        if target in seen:
            raise Invalid(f"target {target!r} is listed twice")
        seen.add(target)
        if not is_https_url(a["url"]):
            raise Invalid(f"artifact URL for {target} must be https://")
        if not _is_sha256(a["sha256"]):
            raise Invalid(f"artifact sha256 for {target} must be 64 lowercase hex characters")
        _uint(f"artifact size for {target}", a["size"], 1, MAX_ARTIFACT_BYTES)
        if a["format"] != "tar.gz":
            raise Invalid(f"artifact format {a['format']!r} is not supported (schema version 1: tar.gz)")
        _uint("stripComponents", a.get("stripComponents", 0), 0, 4)
        req = a.get("requires", {})
        _keys(req, REQUIRES_KEYS, "requires")
        if req.get("glibc") is not None and parse_version(str(req["glibc"])) is None:
            raise Invalid(f"requires.glibc {req['glibc']!r} is not a version")
        if req.get("notes") is not None:
            _text("requires.notes", req["notes"], 0, 500)
        p = a["provenance"]
        _keys(p, PROVENANCE_KEYS, "provenance", ("kind", "source"))
        if p["kind"] not in ("upstream", "built"):
            raise Invalid('provenance.kind must be "upstream" or "built"')
        if not is_https_url(p["source"]):
            raise Invalid("provenance.source must be an https:// URL")
        if p.get("sourceSha256") is not None and not _is_sha256(p["sourceSha256"]):
            raise Invalid("provenance.sourceSha256 must be a sha256 hex digest")
        if p.get("verification") is not None:
            _text("provenance.verification", p["verification"], 1, 300)
        _optional_url("provenance.buildUrl", p.get("buildUrl"))
        _optional_url("provenance.attestationUrl", p.get("attestationUrl"))


# ----------------------------------------------------------------- catalog

def https_host(url: str) -> str:
    if not url.startswith("https://"):
        raise Invalid(f"{url} is not https")
    rest = url[len("https://"):]
    authority = rest.split("/", 1)[0].split("?", 1)[0]
    if "@" in authority:
        raise Invalid("user info is not allowed in URLs")
    host, _, port = authority.partition(":")
    if port and port != "443":
        raise Invalid("only the default https port is allowed")
    return host.lower()


def validate_catalog(c: dict) -> list[str]:
    """Validate a catalog; returns problems that RAST would report per version
    (the official workflow treats any of them as fatal)."""
    if c.get("kind") != CATALOG_KIND or c.get("schemaVersion") != 1:
        raise Invalid("not a rast-catalog schema version 1 document")
    hosts = {h.strip().lower() for h in c.get("allowedHosts", []) if h.strip()}
    if not hosts:
        raise Invalid("catalog lists no allowed download hosts")
    if not isinstance(c.get("sequence"), int) or c["sequence"] < 1:
        raise Invalid("sequence must be a positive integer")
    problems = []
    seen = set()
    for pkg in c.get("packages", []):
        if pkg["id"] in seen:
            raise Invalid(f"catalog lists package {pkg['id']!r} twice")
        seen.add(pkg["id"])
        versions = set()
        for m in pkg.get("versions", []):
            label = f"{pkg['id']} {m.get('version', '?')}"
            try:
                validate_manifest(m)
                if m["id"] != pkg["id"]:
                    raise Invalid("manifest id does not match package")
                if m["publisher"]["id"] != c["publisher"]["id"]:
                    raise Invalid("manifest publisher differs from the catalog publisher")
                for a in m["artifacts"]:
                    if https_host(a["url"]) not in hosts:
                        raise Invalid(f"artifact host {https_host(a['url'])} is not in allowedHosts")
                if m["version"] in versions:
                    raise Invalid("listed twice")
                versions.add(m["version"])
            except Invalid as e:
                problems.append(f"{label}: {e}")
    return problems
