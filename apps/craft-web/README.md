# Craft apps (ArtCraft) for RAST

ArtCraft's Craft apps — PhotoCraft, PdfCraft, FilmCraft, WordCraft,
LightCraft, VectorCraft, EffectCraft, CADCraft, GridCraft, FigCraft,
DesignCraft, DeckCraft, SoundCraft, TextureCraft, MaterialCraft and Craft
Anything — publish web builds (WebAssembly) with each release. Each one is
its own App Center package here, so an administrator installs only the ones
they want and each updates on its own.

RAST shows the app's `web/index.html` as a web page window: a frame sandboxed
in an origin of its own, which reaches nothing of RAST (no session, no API)
and keeps no storage between visits. Files open from the person's device and
save as downloads.

## Files

| File | What it is |
|---|---|
| `crafts.json` | The apps: package id, name, upstream repository and the version packaged |
| `artcraft-release-signing-key.asc` | ArtCraft's release signing key, pinned (fingerprint in `crafts.json`) |
| `package.py` | `check`, `build <package> <out>`, `catalog-entries <minRastVersion>` |
| `../../icons/<package>.png` | Each app's own 128 px icon, from its repository |

`package.py build` downloads `<repo>-web-<version>.zip` from the upstream
release `v<version>` and accepts it only when upstream's SHA256SUMS.txt is
signed by the pinned key (gpgv) and lists the zip's SHA-256, which must also
be GitHub's digest for the asset. The unmodified build becomes
`<package>/web/`, next to `NOTICE-RAST.md`; the archives are reproducible.

## Releasing

Set the app's `version` in `crafts.json`, then push the tag
`<package>-v<version>` (or run the Craft web apps workflow by hand). The
catalog lists a package once `catalog.config.json` has its entry
(`package.py catalog-entries` prints them); the entries need a RAST whose
catalog tool knows app packages without a server and web page windows.
