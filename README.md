# rast-packages

The **official package catalog** for the [RAST](https://github.com/Enricko/rast)
Package Center: verified Python, PHP CLI, Go, Node.js, Rust, Eclipse Temurin
JDK, Bun, Deno, .NET SDK (C#), Clang/LLVM (C/C++), Flutter (with Dart),
Composer, Zig, Kotlin, CMake, Gradle and Apache Maven runtimes that RAST
installs side by side under `/var/lib/rast/packages/runtimes/<id>/<version>/`.

| Repository | Responsibility |
|---|---|
| [Enricko/rast](https://github.com/Enricko/rast) | Package Center UI and API, installer, validation, authorization, jobs |
| **Enricko/rast-packages** (this) | Official catalog: discovers, verifies or builds, tests, signs and publishes (all via the Rust `rast catalog` commands) |
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
| **Rust** stable | The official [static.rust-lang.org](https://static.rust-lang.org/dist/) dist components `rustc`, `rust-std`, `cargo`, `rustfmt` and `clippy` for `x86_64`/`aarch64-unknown-linux-gnu`, unmodified, merged into one standalone prefix (not rustup; `rust-toolchain.toml` is rustup-only, so a RAST-pinned version is used instead) | The channel manifest's SHA-256 must match `channel-rust-stable.toml.sha256`; its OpenPGP signature is verified with `gpgv` against the pinned Rust release signing key (fingerprint `108F 6620 5EAE B0AA A8DD 5E1C 85AB 96E6 FA1B E5FE`, fetched from `static.rust-lang.org`); **every component's SHA-256 comes from that signed manifest**; all five archives are downloaded and re-hashed and `rustc --version` / `cargo --version` / `rustfmt --version` / `clippy-driver --version` run. Needs glibc ≥ 2.17. |
| **Eclipse Temurin JDK** 8 / 11 / 17 / 21 / 25 (LTS) / 27 | [Adoptium](https://api.adoptium.net) `linux` `jdk` HotSpot `.tar.gz` per feature version, unmodified | The latest assets per feature version come from the Adoptium API; each SHA-256 must equal the GitHub release asset digest **and** the published `.sha256.txt`; bytes re-hashed; `java --version`, `javac -version`, `jar --version` and `jshell --version` run (JDK 8 has no `jshell` and uses `java -version`). Needs glibc ≥ 2.17. |
| **Bun** stable | Official `oven-sh/bun` GitHub release `bun-linux-{x64,aarch64}.zip`, unmodified | GitHub asset SHA-256 digest must equal the release `SHASUMS256.txt`; bytes re-hashed; `bun --version` runs. Needs glibc ≥ 2.17. |
| **Deno** stable | Official `denoland/deno` GitHub release `deno-{x86_64,aarch64}-unknown-linux-gnu.zip`, unmodified | GitHub asset SHA-256 digest must equal the published per-asset `.sha256sum`; the GitHub release attestation (in-toto release predicate) is confirmed for this exact artifact digest through the GitHub attestations API (GitHub CLI reports no attestations for this predicate); bytes re-hashed; `deno --version` runs. Needs glibc ≥ 2.27. |
| **.NET SDK** 10.0 LTS / 11.0 STS | Microsoft `dotnet-sdk-<version>-linux-{x64,arm64}.tar.gz` from the official release metadata (`builds.dotnet.microsoft.com`), unmodified; the SDK archive includes the .NET and ASP.NET Core runtimes and provides the C# toolchain | SHA-512 of every archive must match the hash in Microsoft's `releases.json` release metadata; the manifest pins the computed SHA-256 of the downloaded bytes; `dotnet --version`, `dotnet --list-sdks` and `dotnet --list-runtimes` run on the native runner. Needs glibc ≥ 2.27. |
| **Clang/LLVM** stable | `LLVM-<version>-Linux-{X64,ARM64}.tar.xz` from `llvm/llvm-project` releases, unmodified: clang, clang++, ld.lld, lldb (shipped) and the libc++ headers/libraries | SHA-256 from the GitHub release asset digest, which must equal the digest named by the release's SLSA build-provenance bundle (`<asset>.jsonl`); `gh attestation verify --repo llvm/llvm-project` verifies that bundle's signature; bytes re-hashed; `clang --version`, `clang++ --version` and `ld.lld --version` run on the native runner (`runtime.env` sets `LD_LIBRARY_PATH=<prefix>/lib`). `lldb` is shipped but not executed in CI because the official build needs a host libpython shared object (e.g. `libpython3.14.so.1.0` on the release runner); install the matching python3 library to use lldb — clang/clang++/ld.lld work without it. Built on Ubuntu 22.04, needs glibc ≥ 2.34. |
| **Flutter** stable | Official Google Flutter Linux SDK archives (`storage.googleapis.com/flutter_infra_release/releases/stable/linux/flutter_linux_<version>-stable.tar.xz`), unmodified; includes the Dart SDK (`flutter`, `dart`) | SHA-256 from Google's `releases_linux.json`; size from the storage object (HEAD); bytes re-hashed; `flutter --version` and `dart --version` run on the native runner. Google publishes **x86_64 Linux host archives only**, so linux-aarch64 has no artifact and the manifest notes the explicit reason. The SDK writes its cache inside `<prefix>/bin/cache`; needs glibc ≥ 2.31. |
| **Composer** stable | Single-file `composer.phar` from `composer/composer` releases; installed read-only as `<prefix>/composer.phar` with a generated `bin/composer` shell wrapper (`exec php "$root/composer.phar" "$@"`) — the phar is never executed by the installer | SHA-256 from the GitHub release asset digest, which must equal getcomposer.org's published `composer.phar.sha256sum`; bytes re-hashed; `composer --version` runs through the generated wrapper with the PHP CLI on PATH. Requires a PHP CLI (e.g. the PHP runtime in this catalog) on PATH. |
| **Zig** stable | Official `ziglang.org/download/<version>/zig-<arch>-linux-<version>.tar.xz`, unmodified | SHA-256 and size from the official `index.json`; bytes re-hashed; `zig version` runs. The executable is statically linked and sits at the prefix root. |
| **Kotlin** stable | `kotlin-compiler-<version>.zip` from `JetBrains/kotlin` releases (architecture independent, runs on a JVM) | SHA-256 from the GitHub release asset digest, which must equal the published `.zip.sha256`; bytes re-hashed; `kotlinc -version` runs with the JDK on PATH. |
| **CMake** stable | `cmake-<version>-linux-{x86_64,aarch64}.tar.gz` from `Kitware/CMake` releases, unmodified | SHA-256 from the GitHub release asset digest, which must equal the release `SHA-256.txt` (the release also publishes an OpenPGP `.asc` signature for that file, which is not pinned here); bytes re-hashed; `cmake --version` and `ctest --version` run. |
| **Gradle** stable | `gradle-<version>-bin.zip` from the official `services.gradle.org` distribution (served through GitHub release assets), architecture independent | SHA-256 from Gradle's version metadata (`/versions/all`), which must equal the published `<zip>.sha256`; bytes re-hashed; `gradle --version` runs with the JDK on PATH. |
| **Apache Maven** 3.10 | `apache-maven-<version>-bin.tar.gz` from the official Apache archive (`archive.apache.org`), architecture independent | SHA-512 from Apache's published `.sha512`; bytes re-hashed; the manifest pins the computed SHA-256 of the downloaded bytes; `mvn --version` runs with the JDK on PATH. |

Targets: `linux-x86_64` (ubuntu-24.04 runners) and `linux-aarch64`
(ubuntu-24.04-arm runners). Each artifact is extracted and executed on a
native runner of its target before it is published. The JVM-based providers
(Kotlin, Gradle, Maven), Composer's wrapper and the Flutter checks use the
tools already present on the GitHub-hosted runners (JDK 17+ and PHP 8.3 are
preinstalled); Flutter's SDK cache is written inside the extracted tree, which
is writable in the verify job. Every executable is exposed on `PATH` in RAST
Terminal shells through `runtime.pathDirs`/`runtime.env`. The Clang/LLVM
archive expands to ~11.6 GiB, so the verify job first removes preinstalled
toolchains it never uses (Android SDK, .NET) to keep enough free space on the
14 GB runner.

### Database servers are not managed runtimes

MySQL and PostgreSQL **servers** are not part of this catalog: they are
stateful system daemons with their own data directories, users, upgrades and
service management. Install them from the operating system packages
(`mysql-server`/`mariadb-server`, `postgresql`) and manage them with systemd;
the Package Center catalog only ships read-only, side-by-side language/tool
runtimes. (A PostgreSQL/MySQL *client* could be added later if needed.)

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
   nodejs.org, the python-build-standalone release, php.net, the Rust stable
   channel manifest, the Adoptium API, the Bun/Deno/LLVM/Kotlin/CMake GitHub
   releases, Google's Flutter release metadata, the ziglang.org download
   index, Gradle's version metadata and the Apache Maven archive listing; keep
   the newest patch(es) of each tracked channel (`catalog.config.json`).
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

A version that is only being **re-verified** (a new target, a revision bump or
`force`) is not announced again and stays installable with its previous
manifest throughout; if re-verification fails, or covers fewer targets than
before, the previous manifest is kept and the run summary says so.

> **Release sequencing:** the Rust, Temurin, Bun, Deno and .NET providers use
> the multi-archive/zip manifest fields and `runtime.env`/`runtime.pathDirs`
> added in RAST 0.14.0, and the Clang, Flutter, Composer, Zig, Kotlin, CMake,
> Gradle and Maven providers additionally need the `format: "tar.xz"` and
> `format: "phar"` manifest extensions. Old RAST binaries (including the
> current workflow pin) do not parse those manifests, so the workflow's
> `RAST_VERSION` pin and this repository's `minRastVersion` must move together
> with the release that ships them — not before. Until then the pinned binary
> cannot publish the providers that need the newer manifest fields.

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

Download the Rust tool once (replace `v0.11.6` with the release named in the
workflow) and run the catalog commands from the repository root:

```sh
mkdir -p bin
curl -fsSL -o bin/rast https://github.com/Enricko/rast/releases/download/v0.11.6/rast-linux-x86_64
chmod +x bin/rast

./bin/rast catalog previous --out work/previous
./bin/rast catalog discover --previous work/previous --out work/plan.json
./bin/rast catalog verify --plan work/plan.json --target linux-x86_64 --out work/results/x86_64.json
./bin/rast catalog check work/previous/catalog.json
```

`rast catalog` uses RAST's own manifest validator, so nothing invalid is ever
signed. Signing uses `CATALOG_SIGNING_KEY` (PKCS#8 PEM) and Ed25519 from Rust;
`gh` and `gpgv` are only used to download releases and verify Node's signature,
exactly as before.
