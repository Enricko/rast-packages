"""Unit tests for the catalog tooling (python3 -m unittest discover tests)."""

import argparse
import copy
import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))

import catalog  # noqa: E402
import rastpkg  # noqa: E402


def manifest():
    return {
        "schemaVersion": 1,
        "kind": "rast-package",
        "id": "go",
        "type": "runtime",
        "name": "Go",
        "publisher": {"id": "rast", "name": "RAST"},
        "version": "1.26.11",
        "revision": 1,
        "runtime": catalog.runtime_spec("go", "1.26.11"),
        "artifacts": [{
            "target": "linux-x86_64",
            "url": "https://dl.google.com/go/go1.26.11.linux-amd64.tar.gz",
            "sha256": "a" * 64,
            "size": 10,
            "format": "tar.gz",
            "stripComponents": 1,
            "provenance": {"kind": "upstream", "source": "https://go.dev/dl/"},
        }],
    }


class ManifestRules(unittest.TestCase):
    def test_valid(self):
        rastpkg.validate_manifest(manifest())

    def test_rejections_match_rast(self):
        cases = {
            "abs path": lambda m: m["runtime"]["executables"][0].update(path="/usr/bin/go"),
            "dotdot": lambda m: m["runtime"]["executables"][0].update(path="bin/../../go"),
            "http": lambda m: m["artifacts"][0].update(url="http://dl.google.com/go.tar.gz"),
            "sha": lambda m: m["artifacts"][0].update(sha256="XYZ"),
            "target": lambda m: m["artifacts"][0].update(target="windows-x86_64"),
            "unknown field": lambda m: m.update(extra=1),
            "reserved id": lambda m: m.update(id="bin"),
            "future type": lambda m: m.update(type="service"),
            "check exe": lambda m: m["runtime"]["checks"][0].update(executable="nope"),
            "format": lambda m: m["artifacts"][0].update(format="zip"),
        }
        for name, mutate in cases.items():
            m = copy.deepcopy(manifest())
            mutate(m)
            with self.assertRaises(rastpkg.Invalid, msg=name):
                rastpkg.validate_manifest(m)

    def test_versions(self):
        self.assertEqual(rastpkg.default_channel("3.14.10"), "3.14")
        self.assertEqual(rastpkg.default_channel("26"), "26")
        self.assertGreater(rastpkg.version_key("1.27.0"), rastpkg.version_key("1.27rc3"))
        self.assertGreater(rastpkg.version_key("1.26.11"), rastpkg.version_key("1.26.9"))


class CatalogRules(unittest.TestCase):
    def doc(self, versions):
        cfg = json.loads((ROOT / "catalog.config.json").read_text())
        pkgs = catalog.package_entries(cfg)
        pkgs["go"]["versions"] = versions
        return catalog.catalog_doc(cfg, 3, pkgs)

    def test_hosts_and_publisher_are_enforced(self):
        good = manifest()
        self.assertEqual(rastpkg.validate_catalog(self.doc([good])), [])
        stranger = copy.deepcopy(good)
        stranger["artifacts"][0]["url"] = "https://evil.example/go.tar.gz"
        self.assertTrue(rastpkg.validate_catalog(self.doc([stranger])))
        other = copy.deepcopy(good)
        other["publisher"]["id"] = "someone"
        self.assertTrue(rastpkg.validate_catalog(self.doc([other])))

    def test_config_is_consistent(self):
        cfg = json.loads((ROOT / "catalog.config.json").read_text())
        self.assertTrue((ROOT / cfg["publicKey"]).exists())
        self.assertEqual(set(cfg["targets"]), set(rastpkg.KNOWN_TARGETS))
        for pid in cfg["packages"]:
            self.assertNotIn(pid, rastpkg.RESERVED_IDS)

    def test_runtime_specs_validate(self):
        for pkg, version, bundled in [
            ("python", "3.14.10", {"pip": "26.3"}),
            ("nodejs", "24.22.2", {"npm": "11.19.1"}),
            ("php", "8.5.12", {}),
            ("go", "1.27.4", {}),
        ]:
            m = manifest()
            m["id"] = pkg
            m["version"] = version
            m["runtime"] = catalog.runtime_spec(pkg, version, bundled)
            rastpkg.validate_manifest(m)
        self.assertEqual(catalog.channel_of("nodejs", "24.22.2"), "24")
        self.assertIsNone(catalog.channel_of("go", "1.26.11"))


class Reverification(unittest.TestCase):
    """A version that is re-verified (new target or revision) stays installable."""

    def run_pipeline(self, results):
        cfg = json.loads((ROOT / "catalog.config.json").read_text())
        pkgs = catalog.package_entries(cfg)
        pkgs["go"]["versions"] = [manifest()]
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d / "prev").mkdir()
            (d / "results").mkdir()
            (d / "prev" / "catalog.json").write_text(json.dumps(catalog.catalog_doc(cfg, 7, pkgs)))
            item = {"package": "go", "version": "1.26.11", "channel": "1.26", "kind": "upstream",
                    "targets": ["linux-x86_64", "linux-aarch64"], "revision": 1, "status": "new"}
            (d / "plan.json").write_text(json.dumps({"previousSequence": 7, "items": [item], "dropped": []}))
            (d / "results" / "r.json").write_text(json.dumps(results))
            ns = dict(plan=str(d / "plan.json"), previous=str(d / "prev"))
            catalog.cmd_interim(argparse.Namespace(**ns, out=str(d / "interim.json")))
            catalog.cmd_assemble(argparse.Namespace(**ns, results=str(d / "results"), php_releases="",
                                                    out=str(d / "final.json")))
            return json.loads((d / "interim.json").read_text()), json.loads((d / "final.json").read_text())

    @staticmethod
    def go(doc):
        return next(p for p in doc["packages"] if p["id"] == "go")

    def test_failed_reverification_keeps_the_previous_manifest(self):
        failed = [{"package": "go", "version": "1.26.11", "target": t, "ok": False, "error": "boom"}
                  for t in ("linux-x86_64", "linux-aarch64")]
        interim, final = self.run_pipeline(failed)
        for doc, seq in ((interim, 8), (final, 9)):
            self.assertEqual(doc["sequence"], seq)
            self.assertEqual([m["version"] for m in self.go(doc)["versions"]], ["1.26.11"])
            self.assertEqual(self.go(doc)["detected"], [])
            self.assertEqual(self.go(doc)["versions"][0], manifest())


class LocalRuns(unittest.TestCase):
    def test_no_invented_workflow_run_url(self):
        import os
        if not os.environ.get("GITHUB_RUN_ID"):
            self.assertIsNone(catalog.RUN_URL)
            self.assertEqual(catalog.RUN_NOTE, "")
        self.assertNotIn("/actions/runs/0", catalog.RUN_NOTE)


if __name__ == "__main__":
    unittest.main()
