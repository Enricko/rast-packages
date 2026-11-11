# rast-packages

The **official package catalog** for the [RAST](https://github.com/Enricko/rast)
Package Center: verified Python, PHP CLI, Go and Node.js runtimes that RAST
installs side by side under `/var/lib/rast/packages/runtimes/<id>/<version>/`.

| Repository | Responsibility |
|---|---|
| [Enricko/rast](https://github.com/Enricko/rast) | Package Center UI and API, installer, validation, authorization, jobs |
| **Enricko/rast-packages** (this) | Official catalog: discovers, verifies or builds, tests, signs and publishes |
| [Enricko/rast-package-template](https://github.com/Enricko/rast-package-template) | Forkable starter for third-party packages installed from GitHub |

## What RAST downloads

RAST reads one file: the **signed catalog** attached to the
[`catalog` release](https://github.com/Enricko/rast-packages/releases/tag/catalog):

* `catalog.json` — every installable version as a full `rast-package.json`
  manifest (schema: [`schema/rast-package.schema.json`](schema/rast-package.schema.json)),
  plus upstream versions that were *detected* but are not verified yet.
* `catalog.json.sig` — Ed25519 signature over the exact bytes of
  `catalog.json`, key id `rast-official-2026-11`
  ([public key](keys/rast-official-2026-11.pub.pem); RAST embeds it).
* `catalog-<sequence>.json` — history. `sequence` only grows; RAST refuses
  an older sequence (rollback) or the same sequence with other contents.

Every artifact is pinned by SHA-256 and size, and its host must be listed in
the catalog's `allowedHosts` (checked by RAST on every redirect).

## Sources and verification

| Runtime | Artifact | How it is verified before it is listed |
|---|---|---|
| **Python** 3.13 / 3.14 / 3.15 | [python-build-standalone](https://github.com/astral-sh/python-build-standalone) `install_only` (gnu) builds, unmodified | SHA-256 from the release `SHA256SUMS` must equal the GitHub asset digest and the downloaded bytes; GitHub build-provenance attestation verified with `gh attestation verify --repo astral-sh/python-build-standalone`; `python3 --version` and `pip3 --version` run on native runners. Needs glibc ≥ 2.17. |
| **PHP CLI** 8.4 / 8.5 | Built **in this repository's CI** with [static-php-cli](https://github.com/crazywhalecc/static-php-cli) in `alpine:3.22` → one static musl binary per target | static-php-cli release binary checked against its GitHub asset digest; the PHP source archive used must match the SHA-256 published by php.net; `php -v` / `php -m` on native runners; GitHub build-provenance attestation for each archive (`gh attestation verify <file> --repo Enricko/rast-packages`). |
| **Go** 1.26 / 1.27 | Official `go.dev` Linux archives (`dl.google.com`), unmodified | SHA-256 from the go.dev release index; downloaded and re-hashed; `go version` runs. Statically linked; cgo builds need a host C compiler. |
| **Node.js** 24 / 26 LTS | Official `nodejs.org/dist` Linux `.tar.gz`, unmodified | `SHASUMS256.txt` signature verified with `gpgv` against the [nodejs/release-keys](https://github.com/nodejs/release-keys) keyring; bytes re-hashed; `node --version` and `npm --version` run — the **npm version recorded is the one found in the archive**. Needs glibc ≥ 2.28. |

Targets: `linux-x86_64` (ubuntu-24.04 runners) and `linux-aarch64`
(ubuntu-24.04-arm runners). Each artifact is extracted and executed on a
native runner of its target before it is published.

### PHP extensions and host dependencies

`bcmath calendar ctype curl dom exif fileinfo filter iconv mbregex mbstring
openssl pcntl pdo pdo_mysql pdo_sqlite phar posix readline session simplexml
sockets sodium sqlite3 tokenizer xml xmlreader xmlwriter zip zlib` (the final
list in each manifest comes from `php -m` on the built binary). The binary is
static: no host libraries are needed. TLS uses the host CA bundle
(`/etc/ssl/certs`). No `php.ini` is shipped.

## The catalog workflow

[`.github/workflows/catalog.yml`](.github/workflows/catalog.yml) runs every six
hours and on demand:

1. **discover** — fetch and verify the published catalog; query go.dev,
   nodejs.org, the python-build-standalone release and php.net; keep the newest
   patch(es) of each tracked channel (`catalog.config.json`).
2. **announce** — if anything is new, publish a catalog (sequence + 1) that
   lists it under `detected` with state `verifying` or `building`. RAST shows
   this as **"New upstream version detected"**; it is not installable yet.
3. **verify** (per target) — download, check checksums/signatures/attestations,
   extract with the same path rules as RAST, run the version checks.
4. **php** (per version × target) — build, attest, smoke-test.
5. **publish** — upload PHP archives to a `php-<version>-r<revision>` release,
   assemble the final catalog (sequence + 2), re-download every newly hosted
   artifact and compare bytes, sign, validate with the same rules as RAST and
   upload. Verified versions become **"Update ready to install"** in RAST; a
   failure stays visible as `detected` with state `failed` and the reason.

Manual run options: `withhold` (e.g. `go@1.26.11`) keeps a version out of the
catalog; `force` re-verifies versions already listed. To rebuild a version,
bump its revision in `catalog.config.json` (`"revisions": {"php@8.5.12": 2}`).

## Keys

The private key is the repository secret `CATALOG_SIGNING_KEY` (PKCS#8 PEM).
To rotate: generate a new key (`openssl genpkey -algorithm ed25519`), add its
public key to RAST's `OFFICIAL_KEYS` and release RAST, wait until servers have
updated, then switch the secret and `keyId`. Servers can also trust extra keys
with `packages_catalog_keys` in `rast.toml`.

## Local development

```sh
python3 -m unittest discover tests
python3 scripts/catalog.py previous --out work/previous
python3 scripts/catalog.py discover --out work/plan.json
python3 scripts/catalog.py verify --plan work/plan.json --target linux-x86_64 --out work/results/x86_64.json
python3 scripts/catalog.py check work/previous/catalog.json
```

`scripts/rastpkg.py` mirrors RAST's manifest rules so nothing invalid is ever
signed.
