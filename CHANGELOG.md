# Changelog

## Unreleased

## 0.1.0-beta.2 (2026-10-25)

- Open the TUI before scanning, report live discovery and size-measurement progress, and allow scan cancellation.
- Move cleanup off the UI thread and show queued work, process checks, safety validation, Cargo cleanup, and elapsed time while keeping navigation responsive.
- Collapse nested projects into expandable groups with aggregate sizes, partial selection, and explicit hidden-selection counts; measure nested files once and update parent totals after cleanup.
- Refuse existing staging and rollback destinations atomically, including empty directories; capture target identity before validation and verify actual removal before reporting success.
- Protect bare Git repositories inside package output and readme/license inputs inherited from enclosing workspaces, including when cleaning a selected member.
- Restrict cleanup to validated local target directories, with checks for project inputs, tracked files, links, cache markers, and active Cargo processes.
- Protect workspace member inputs, nested source links, and path dependencies declared in any dependency/patch/replace/platform table; refuse cleanup when a running Cargo process cannot be scoped.
- Recognize `cargo package` output under `target/package/` as Cargo's own, refuse bare or renamed Git repositories and additional VCS markers (`.jj`, `.bzr`, `_darcs`, Fossil, Pijul), and fail closed on repository markers that are not plain directories or files.
- Pin the validated target directory by renaming it to a fresh sibling name and re-verifying its filesystem identity before deletion, so a path swap cannot redirect the cleanup; restore the directory on failure.
- Resolve `git` and `cargo` from PATH while ignoring relative or current-directory entries, and bound Git inspection with a timeout so blocked tools refuse cleanup instead of hanging.
- Apply the Git timeout to inherited output pipes as well as the direct process, and refuse cleanup when output collection fails.
- Verify downloaded binaries against the `<asset>.sha256` checksums published with each GitHub release (`RUCYCLE_SKIP_CHECKSUM=1` opts out), and refuse redirects that downgrade a download to plain HTTP.
- Update direct Rust dependencies and GitHub Actions to current stable releases, refresh compatible transitive dependencies, and adapt to the TOML document parsing API.
- Test on Linux, macOS, and Windows in CI using the checked-in dependency lockfile.
- Preserve the installed npm binary on failed downloads and clean temporary publishing directories on failure.
- Restore the terminal on initialization errors and panics, retain cleanup refusal reasons, and show summaries for the current cleanup run.

## 0.1.0-beta.1

- Initial open-source release.
- Fast Rust project discovery with size and modification-time sorting.
- Interactive TUI for selecting projects and running `cargo clean`.
- npm wrapper support for `npx rucycle`.
- GitHub Actions CI and multi-platform release packaging.
