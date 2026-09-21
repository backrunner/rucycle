# Changelog

## Unreleased

- Restrict cleanup to validated local target directories, with checks for project inputs, tracked files, links, cache markers, and active Cargo processes.
- Protect workspace member inputs and nested source links; refuse cleanup when a running Cargo process cannot be scoped.
- Update direct Rust dependencies and GitHub Actions to current stable releases, refresh compatible transitive dependencies, and adapt to the TOML document parsing API.
- Test on Linux, macOS, and Windows in CI using the checked-in dependency lockfile.
- Preserve the installed npm binary on failed downloads and clean temporary publishing directories on failure.
- Restore the terminal on initialization errors and panics, retain cleanup refusal reasons, and show summaries for the current cleanup run.

## 0.1.0

- Initial open-source release.
- Fast Rust project discovery with size and modification-time sorting.
- Interactive TUI for selecting projects and running `cargo clean`.
- npm wrapper support for `npx rucycle`.
- GitHub Actions CI and multi-platform release packaging.
