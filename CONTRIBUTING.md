# Contributing to BTMS

Thank you for helping with BTMS · Buu's Tiles Map Scanner. Bug reports, fixes, documentation,
translations and new features are all welcome. By taking part in the project you agree to follow
the [Code of Conduct](CODE_OF_CONDUCT.md).

- [Ground rules](#ground-rules)
- [Bugs, ideas and questions](#bugs-ideas-and-questions)
- [Building from source](#building-from-source)
- [Tests, formatting and lints](#tests-formatting-and-lints)
- [Code style](#code-style)
- [Sign your work: file headers](#sign-your-work-file-headers)
- [Working on the interface](#working-on-the-interface)
- [Adding a language](#adding-a-language)
- [Commits and pull requests](#commits-and-pull-requests)
- [Releases](#releases)
- [License of contributions](#license-of-contributions)

## Ground rules

BTMS reads the game's own files on disk, offline and read-only. These rules are part of what the
project is, and pull requests that break them are not merged:

- **Protected resources stay protected.** Files protected by the FiveM asset escrow (they start
  with `FXAP`) are detected, reported and left out of the tiles. Code that decrypts or bypasses
  that protection, or recovers protected content in any other way, is not accepted, and issues
  that ask for it are closed.
- **No attaching to the game or to FiveM.** No reading or writing of process memory, no
  injection, no hooks. Everything comes from files on disk.
- **Nothing is written to the game folder.** BTMS writes tiles, reports and its own settings only.
- **No keys and no game files in the repository.** Never commit or attach `magic.dat`, decryption
  keys, game files, assets extracted from them, or resources you have no right to share. Users get
  the keys from CodeWalker's `magic.dat`: the installer can download it for them, or they import a
  copy with `btms import-keys <magic.dat>`.
- **Offline.** The program never connects to the internet. The interface is served on `127.0.0.1`
  only, and neither the interface page nor the viewer page loads anything from the internet
  (fonts, scripts, styles or images). The only download is the installer's optional keys task.

## Bugs, ideas and questions

- **Bugs, feature requests and translation mistakes:** use the
  [issue forms](https://github.com/BuuDevOff/BTMS/issues/new/choose).
- **Questions and ideas** that are not yet a clear request:
  [Discussions](https://github.com/BuuDevOff/BTMS/discussions).
- **Security problems:** a private report, as described in [SECURITY.md](SECURITY.md), never a
  public issue.

## Building from source

BTMS runs on Windows, like GTA V and FiveM. You need:

- **Windows 10 or 11, x64.**
- **Rust stable, 1.90 or newer** (the `rust-version` of `Cargo.toml`) with the MSVC toolchain
  (`stable-x86_64-pc-windows-msvc`). Install it with [rustup](https://rustup.rs/).
- **The MSVC C/C++ build tools.** The WebP encoder is libwebp, compiled from its C sources by the
  `libwebp-sys` crate. Install
  [Build Tools for Visual Studio](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with
  the "Desktop development with C++" workload (MSVC compiler and Windows SDK).
- **A graphics card is optional.** Rendering falls back to the processor when no DirectX 12 or
  Vulkan adapter can be used.

```powershell
git clone https://github.com/BuuDevOff/BTMS.git
cd BTMS
cargo build --release
```

The executable is `target\release\btms.exe`. The interface page and the viewer page are embedded
in it, so it runs without any file next to it.

```powershell
cargo run --release -- help        # the usage
cargo run --release -- --version   # btms <version>
cargo run --release                # the interface (the default with no command)
```

Rendering tiles needs a GTA V installation and the imported keys. You need neither to build BTMS,
run its tests, or work on the interface in demo mode.

## Tests, formatting and lints

CI runs these checks on Windows for every pull request. Run them before you push:

```powershell
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

- **GPU tests skip themselves without a graphics card.** They look for a hardware DirectX 12 or
  Vulkan adapter. When there is none, they print why and pass without checking anything. Software
  adapters (such as WARP, the "Microsoft Basic Render Driver") are only used when `BTMS_GPU` names
  one. On a machine with a card, the GPU tests compare what the card draws with what the
  processor draws.
- **`BTMS_GPU=<words>`** picks the adapter whose backend and name contain all the words, for
  example `$env:BTMS_GPU = "vulkan"` or `$env:BTMS_GPU = "dx12 radeon"` in PowerShell.
- **Ignored tests** are measurements or checks that read an installed copy of the game. They never
  run in CI. Run one on your own machine, with the game and the keys in place:
  `cargo test --release -- --ignored <test name>`.
- **Behaviour changes come with a test.** Test names read as sentences that say what is checked,
  for example `a_card_short_of_memory_keeps_drawing`.

The formatting rules are in `rustfmt.toml`. If you change JavaScript and have Node.js installed,
`node --check <file>` catches syntax errors. Node is not needed to build or run BTMS.

## Code style

- **English everywhere:** identifiers, comments, doc comments, log lines, error messages, CLI
  commands and flags, JSON keys, test names. The only other languages in the tree are the
  translations of the interface, the viewer page and the installer.
- **Comments explain why:** the constraint, the measurement, the game quirk. Do not repeat what
  the code already says. Keep them plain and precise.
- **Names:** CLI commands and flags in kebab-case, option values in lowercase, boolean option
  values `on|off`. JSON keys in snake_case. JavaScript in camelCase. CSS classes, ids and data
  attributes in kebab-case. Environment variables start with `BTMS_`.
- **Errors:** return `anyhow::Result` with context that names the file or the option involved, so
  that a user can act on the message.
- **The same picture on both paths.** The GPU path and the CPU path draw the same tiles. A change
  to one usually needs the same change to the other, and a test that compares them.

The modules and how a render flows through them are described in
[docs/architecture.md](docs/architecture.md).

## Sign your work: file headers

The ASCII banner at the top of the existing files, with `Author : BuuDevOff`, is BuuDevOff's
signature on the files BuuDevOff wrote. **Please do not copy it into your own files.** Your work
deserves your name: sign it, we want to credit you.

**A new file starts with your own header:** your name or handle, a link (your GitHub profile or
website), any banner you like, and the licence line `SPDX-License-Identifier: GPL-3.0-only`. In
Rust (WGSL is the same):

```rust
// Author : YourName · https://github.com/your-handle
// SPDX-License-Identifier: GPL-3.0-only

//! What this module does, and why.
```

In JavaScript:

```js
// Author : YourName · https://github.com/your-handle
// SPDX-License-Identifier: GPL-3.0-only

// What this file does, and why.
```

CSS takes the same lines in a `/* */` comment, HTML in a `<!-- -->` comment.

**A file you change substantially** keeps its header, and you may add a line of your own under it,
saying what you did:

```rust
// … (the end of the file's existing header)
// SPDX-License-Identifier: GPL-3.0-only
// Contributor : YourName · https://github.com/your-handle (lossless WebP for partly empty tiles)
```

Several contributors add one line each. A small fix needs no line: the git history and GitHub's
list of contributors credit every commit.

## Working on the interface

The interface is a local web page served by `btms interface` on `127.0.0.1`. Its files are in
`web/interface/` and are embedded in the executable with `include_bytes!` (the `PAGE` list in
`src/interface/mod.rs`).

- **Demo mode** needs no game. Run `cargo run -- interface --no-window`, open the address it
  prints in a browser, and add `?demo=1`. A fake server in memory (`web/interface/js/demo.js`)
  then replaces the real one: it renders nothing and touches nothing. It follows the same routes
  and JSON as the real server, so keep the two in step when you change the protocol.
- **Without rebuilding:** `BTMS_INTERFACE_DIR` serves the page files from a folder instead of the
  embedded copies. Point it at `web\interface` and reload the page after each change:

  ```powershell
  $env:BTMS_INTERFACE_DIR = "web\interface"
  cargo run -- interface --no-window
  ```

- **Text:** every string the page shows goes through `t(key, params)` from
  `web/interface/js/i18n.js`. Add new keys to `web/interface/locales/en.json`, the source of
  truth. Keys are flat, dotted and namespaced by the file that owns them: `page.*` (static text of
  `index.html`), `app.*`, `map.*`, `fallback.*`, `common.*` (units and generic words), `server.*`
  (messages from the Rust server). Static HTML uses `data-i18n`, `data-i18n-title`,
  `data-i18n-aria-label` and `data-i18n-placeholder`. A key missing from another language falls
  back to English, so you do not have to translate your new keys yourself.
- **Messages from Rust** that the page shows are sent as English text plus a stable snake_case
  `code` and optional `params`. Add a `server.<code>` key to `en.json` for each new code. Job logs
  (the output of a render) stay in English and are not translated.

## Adding a language

The interface, the viewer page and the installer ship in English (`en`), French (`fr`), German
(`de`), Dutch (`nl`), Russian (`ru`), Swedish (`sv`) and Spanish (`es`). To add a language:

1. **Interface dictionary.** Copy `web/interface/locales/en.json` to
   `web/interface/locales/<code>.json`, where `<code>` is the two-letter language code (the
   interface matches it against the first two letters of the browser's languages). It mirrors
   `en.json`: the same keys, every value translated, the `{placeholders}` unchanged. Plurals use the
   categories of `Intl.PluralRules` as key suffixes: `.one` and `.other`, plus `.few`, `.many` or
   `.zero` where the language needs them.
2. **Language list.** Add the code and the language's native name to `LANGUAGES` in
   `web/interface/js/i18n.js`.
3. **Embedding.** Add the new file to the `PAGE` list in `src/interface/mod.rs`, next to the other
   locales (and to the list of names its test expects), so that the server serves
   `/locales/<code>.json`.
4. **Viewer page.** Add the language to the inline dictionary and to the selector of
   `web/viewer/index.html`. That page must stay one self-contained file.
5. **Installer.** In `installer/btms.iss`, add a line to `[Languages]` with Inno Setup's messages
   file for the language (for example `Name: "it"; MessagesFile: "compiler:Languages\Italian.isl"`),
   and a `<code>.` line for every message in `[CustomMessages]`. Keep that file in UTF-8 with a BOM
   and CRLF line endings. If Inno Setup has no official translation for your language, say so in
   the pull request.
6. **Check it.** Run the interface in demo mode, pick the language in the header and go through
   every panel: long words can break narrow layouts. Then open a rendered folder's `index.html`
   with the language. To check the installer, build it locally (see [Releases](#releases)).
7. **README.** Add the language to its list. The changelog line comes from your pull request's
   title, for example `feat: add Italian translation`.

Corrections to an existing translation are just as welcome: edit its `locales/<code>.json`, and
the viewer dictionary or the installer messages if the same text is there.

## Commits and pull requests

- **Branch from `main`** and keep one topic per pull request. Small pull requests are reviewed
  faster.
- **Conventional Commits.** Pull requests are squash-merged, and their title becomes the commit
  on `main`. The title reads `<type>: <summary>` in the imperative mood (72 characters or fewer),
  for example `fix: keep the alpha of partly empty WebP tiles`. A check enforces it. The types:
  - `feat` (a new feature: minor version);
  - `fix` (a bug fix: patch version);
  - `perf` (faster: patch version);
  - `revert` (undoes an earlier change);
  - `docs`, `refactor`, `test`, `build`, `ci`, `style` and `chore` (no release).

  Add `!` after the type for a breaking change (`feat!: …`: major version). Use the body to
  explain why, and reference issues with `Fixes #123`.
- **CI must pass:** formatting, clippy with warnings as errors, and the tests, on Windows.
- **Changelog:** do not edit [CHANGELOG.md](CHANGELOG.md) by hand. The release workflow writes it
  from the commit titles, so write titles a user can understand.
- **Documentation:** update `docs/` and `map.toml` when you add or change a command, a flag, a
  `map.toml` key or the `tiles.json` format.
- **File headers:** new files carry your own header, not BuuDevOff's banner (see
  [Sign your work](#sign-your-work-file-headers)).
- **Interface changes:** include a screenshot (demo mode is fine).
- **Renders:** if your change alters the tiles, say which area you rendered to check it, and
  include a before and after crop of a few tiles.

## Releases

Releases are automatic (`.github/workflows/release.yml`):

1. **Every push to `main` builds BTMS for Windows**: the installer `BTMS-Setup-<version>-x64.exe`
   and the portable `BTMS-<version>-x64-portable.zip`, kept for 30 days as the run's artifacts
   (Actions tab → the run → Artifacts). Use them to try a change before it is released.
2. **A push that contains a `feat`, `fix` or `perf` commit, or a breaking change, publishes a new
   version.** [release-please](https://github.com/googleapis/release-please) computes the version
   from the commits since the last release. It bumps `Cargo.toml` and `Cargo.lock`, writes the
   `CHANGELOG.md` section and opens a release pull request. The workflow merges that pull request
   at once, which tags `vX.Y.Z` and creates the GitHub release. The Windows build of that tag is
   then attached to the release, with `SHA256SUMS.txt` and a download table.
3. **To review releases yourself instead,** set the repository variable `BTMS_AUTO_RELEASE` to
   `false` (Settings → Secrets and variables → Actions → Variables). The release pull request then
   waits, and merging it publishes the release.
4. **To rebuild a release,** run the Release workflow by hand (Actions → Release → Run workflow)
   with its tag, for example `v1.0.0`. Its files are built again and replaced.

The very first push publishes `v1.0.0`, the version in `Cargo.toml`, with its `CHANGELOG.md`
section. One repository setting is required for the release pull requests: Settings → Actions →
General → Workflow permissions → **Allow GitHub Actions to create and approve pull requests**.

The installer is built with [Inno Setup](https://jrsoftware.org/isinfo.php) from
[`installer/btms.iss`](installer/btms.iss). To build it locally, install Inno Setup 6, then run:

```powershell
cargo build --release
iscc /DAppVersion=1.0.0 installer\btms.iss    # writes dist\BTMS-Setup-1.0.0-x64.exe
```

## License of contributions

BTMS is licensed under the [GNU General Public License v3.0 only](LICENSE). By submitting a
contribution, you agree that it is licensed under the same terms, as the SPDX line of your header
says. There is no contributor license agreement.
