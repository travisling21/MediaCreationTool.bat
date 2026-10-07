Windows ISO Validator
=====================

A portable Windows desktop app (single `.exe`, no installer, no runtime to install) that wraps
`MediaCreationTool.bat` and adds native downloading and validation:

- **ESD catalog** - lists every ESD Microsoft publishes for each Windows 10 / 11 version the script
  supports (1507 to 26H2), using the script's own version table and link tables. Downloads are
  resumable and verified against the catalog SHA-1 / SHA-256 while they stream.
- **Microsoft ISO** - fetches the official multi-edition ISO links from Microsoft's download page
  (the method used by Rufus / Fido) together with the SHA-256 table Microsoft publishes next to
  them, then downloads and verifies.
- **Validate** - computes MD5, SHA-1 and SHA-256 of any ISO / ESD / WIM, identifies the file
  (ISO volume label, WIM header) and compares against an expected hash, every loaded catalog and
  Microsoft's published ISO hashes.
- **Create media (MCT)** - writes the bundled `MediaCreationTool.bat` into a work folder and
  launches it with the chosen version, preset, edition, language and architecture. The script
  does what it always does (elevation prompt, own console window, setup-check bypass unless `def`),
  and the resulting ISO shows up in the app for validation.

Every byte downloaded comes from Microsoft servers. The script is embedded at build time from the
repository root, so the app and the script never drift apart.

Building
--------

Requires a stable Rust toolchain (1.85 or newer).

    cd WindowsISOValidator
    cargo build --release          # on Windows: target\release\WindowsISOValidator.exe

The `.cargo/config.toml` links the C runtime statically, so the MSVC build runs on any Windows 10 /
11 machine without the Visual C++ redistributable. The GitHub Actions workflow
`.github/workflows/windows-iso-validator.yml` builds and uploads `WindowsISOValidator-win64` on
every push that touches the app or the script.

On Linux / macOS the app builds and runs for development (catalog, downloads and validation work
everywhere); only launching the script needs Windows.

Portable behaviour
------------------

- Settings live in `WindowsISOValidator.json` next to the executable (or in the temp folder when
  that location is read-only).
- Downloads default to `downloads\` and the script work folder to `mct\`, both next to the exe.
- Partial downloads are kept as `.part` files and resumed on the next attempt, with the hash
  recomputed over the whole file.

Notes
-----

- Windows 11 24H2 and newer require a CPU with POPCNT / SSE4.2; the script cannot bypass that.
- Microsoft rate-limits the download page API per IP address (error 715-123130); wait a day or use
  another network if that happens. Links expire after 24 hours.
- For 25H2 and 26H2 the official MCT no longer publishes a static catalog; the app, like the script,
  rebuilds the catalog from Microsoft's 24H2 one with the link table embedded in the script.

Tests
-----

    cargo test --release                        # unit tests
    cargo test --release -- --ignored           # also the live catalog test (needs network)
