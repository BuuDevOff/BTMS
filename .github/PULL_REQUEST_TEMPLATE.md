## What this changes

<!-- What does this pull request do, and why? Link the issue it addresses, for example "Fixes #123". -->

## How it was checked

<!-- Tests added or run, the area of the map rendered to check it (with a before and after crop if the tiles change), screenshots of the interface (demo mode is fine). -->

## Checklist

- [ ] The title follows Conventional Commits (`feat: …`, `fix: …`, `docs: …`) and reads well in the changelog.
- [ ] `cargo fmt --all` leaves nothing to change.
- [ ] `cargo clippy --all-targets -- -D warnings` passes.
- [ ] `cargo test --all-targets` passes.
- [ ] `docs/` and `map.toml` are updated for new or changed commands, flags, `map.toml` keys or `tiles.json` fields.
- [ ] New interface text has keys in `web/interface/locales/en.json`.
- [ ] Code, comments, logs and messages are in English.
- [ ] New files carry my own header and the SPDX line, not BuuDevOff's banner ([Sign your work](https://github.com/BuuDevOff/BTMS/blob/main/CONTRIBUTING.md#sign-your-work-file-headers)).
- [ ] Nothing here includes keys, `magic.dat`, game files, extracted assets or protected resource files.
- [ ] Nothing here decrypts or works around protected (escrow, `FXAP`) resources, attaches to the game or FiveM, or makes the program connect to the internet.
