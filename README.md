# rucycle

`rucycle` is a fast Rust CLI/TUI for finding Rust projects under a directory and cleaning their Cargo build artifacts in bulk. It is built for the common "my workspace folder is full of old `target/` directories" problem.

## Features

- Recursively scans a directory for valid Rust `Cargo.toml` manifests.
- Shows each project in a terminal UI with:
  - reclaimable `target/` size
  - total project size
  - latest modified time across the whole project folder
- Selects every project by default.
- Supports keyboard navigation, pagination, select all/none, and sorting.
- Shows live scan activity and completed-project progress while measuring sizes.
- Groups nested projects into expandable rows with combined target sizes and selection states.
- Keeps the TUI responsive during cleanup, with per-project stages and elapsed time.
- Runs Cargo clean only on validated local `target/` directories for selected projects.
- Pins both Cargo output directories and skips unsafe or ambiguous cleanup targets.
- Reports and skips any project tree that already has an active `cargo` process.
- Provides non-interactive modes for scripts and CI.
- Ships as a native Rust binary, with npm wrapper support for `npx rucycle`.

## Install

Use it immediately with npm:

```sh
npx rucycle
```

Or install globally:

```sh
npm install -g rucycle
rucycle
```

With Homebrew on macOS or Linux:

```sh
brew tap backrunner/rucycle https://github.com/backrunner/rucycle
brew install backrunner/rucycle/rucycle
```

From source:

```sh
cargo install --locked --path .
```

This installs `rucycle` into `~/.cargo/bin`; keep that directory on your `PATH` to run it from any directory. To use `~/.local/bin` instead (when it is already on your `PATH`):

```sh
cargo install --locked --path . --root "$HOME/.local"
rucycle --version
```

## Usage

Scan the current directory and open the TUI:

```sh
rucycle
```

Scan another directory:

```sh
rucycle ~/Projects
```

Preview detected projects without opening the TUI:

```sh
rucycle ~/Projects --dry-run
```

Clean validated local targets without prompting (unsafe targets are skipped):

```sh
rucycle ~/Projects --yes
```

Start with projects sorted by last modification time:

```sh
rucycle ~/Projects --sort modified
```

## TUI Controls

| Key | Action |
| --- | --- |
| `Up` / `Down`, `j` / `k` | Move selection |
| `PageUp` / `PageDown` | Change page |
| `Home` / `End` | Jump to first or last project |
| `Left` / `Right` | Collapse / expand a project group |
| `Enter` | Toggle group expansion |
| `Space` | Toggle current project and its nested projects |
| `a` | Select all or none |
| `s` | Toggle sorting by target size or modified time |
| `c` | Clean selected projects |
| `q`, `Esc` | Quit |

Nested projects start collapsed. Group sizes include their descendants, and `[-]` means only part of the group is selected. Expand a group to select individual nested projects. The header and cleanup confirmation include selected projects hidden inside collapsed groups; each local target is counted once in the reclaimable total. The footer shows the focused project's own local target size.

Scanning opens the TUI immediately. Discovery shows visited entries and projects found; size measurement shows completed projects, inspected bytes, the current path, and elapsed time. The progress bar counts projects, so a large project can take longer than several small ones. Press `q` or `Esc` to cancel a scan.

After confirming cleanup, the TUI shows queued projects immediately, then running-process checks, target safety validation, and Cargo cleanup. You can navigate, expand groups, and sort while it runs. Selection changes and quitting are disabled until cleanup finishes so an in-progress target move or deletion is not interrupted.

## How Scanning Works

`rucycle` looks for files named `Cargo.toml`, parses them as TOML, and treats manifests with a `[package]` or `[workspace]` table as Rust projects. It skips expensive discovery inside `target/`, `node_modules/`, and VCS directories, then computes project sizes in parallel. Nested project files are measured once and their totals are rolled up into parent projects. TUI groups follow directory ancestry; they do not change Cargo workspace membership or cleanup scope. Non-interactive output retains one row per discovered manifest.

The "modified" sort key is the newest modification time of any file under the project folder. The "target" size is the size of files under that project's `target/` directory.

Custom/shared Cargo output directories are deliberately left alone. Selecting a workspace member only cleans that member's local `target/`, if present; select the workspace root to clean its `target/`. Sizes describe discovered files, not a promise that every target will pass the cleanup safety checks.

## npm Distribution

The `rucycle` npm package is a small JavaScript launcher with no third-party npm dependencies. During npm install, it downloads the matching native binary from the GitHub Release for the package version and stores it in `npm/bin/`. Downloads are staged before replacement, so a failed transfer preserves the existing binary. Each binary is verified against the `<asset>.sha256` checksum published alongside the release assets; set `RUCYCLE_SKIP_CHECKSUM=1` only to install releases predating checksum publication.

Release channels are represented by npm dist-tags and semver prerelease versions:

- `npx rucycle` or `npx rucycle@latest` installs the stable package and downloads assets from `v<version>`.
- `npx rucycle@beta` installs the package published with the `beta` dist-tag, such as `0.1.0-beta.1`, and downloads assets from `v0.1.0-beta.1`.
- `npx rucycle@alpha` installs the package published with the `alpha` dist-tag, such as `0.1.0-alpha.1`, and downloads assets from `v0.1.0-alpha.1`.

Supported release assets:

- `rucycle-darwin-arm64`
- `rucycle-darwin-x64`
- `rucycle-linux-arm64`
- `rucycle-linux-x64`
- `rucycle-win32-x64.exe`

On macOS and Linux, the installer sets executable permissions on the downloaded binary so `npx rucycle` can run it directly.

## Homebrew Distribution

The Homebrew formula lives in this repository at `Formula/rucycle.rb`. Because the repository is not named with Homebrew's `homebrew-` tap prefix, users pass the repository URL when tapping:

```sh
brew tap backrunner/rucycle https://github.com/backrunner/rucycle
```

Stable releases update `Formula/rucycle.rb` on the `main` branch from the GitHub Release tarball assets and their SHA-256 checksums. Prerelease tags such as alpha and beta builds do not update the stable Homebrew formula.

The first stable release after this setup creates the formula file. Until then, only npm and source installs are available.

The release workflow runs `scripts/update-homebrew-formula.mjs` after the GitHub Release and npm package are published, then commits the formula update back to this repository with the workflow `GITHUB_TOKEN`.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
npm run test:installer
npm run test:homebrew
npm run test:install
npm test
```

During local development, the npm launcher falls back to `target/release/rucycle` and `target/debug/rucycle`, so `npm test` works after a Cargo build.

`npm run test:install` copies a locally built binary into `npm/bin/` and verifies the same executable path used by `npx rucycle`.

## Release

GitHub Actions includes:

- `CI`: formatting, clippy, tests, release build, local npm install smoke test, and npm launcher smoke test on Linux, macOS, and Windows. Cargo checks and builds use the checked-in lockfile.
- `Release`: builds native binaries for Linux, macOS, and Windows, uploads them to GitHub Releases, publishes the npm package, then updates the Homebrew formula for stable releases.

To publish, configure npm Trusted Publishing for this GitHub Actions workflow, then push a semver tag:

```sh
git tag v0.1.0
git push origin v0.1.0
```

Prerelease tags are published to their matching npm dist-tag and marked as GitHub prereleases:

```sh
git tag v0.1.0-beta.1
git push origin v0.1.0-beta.1
```

If npm Trusted Publishing is not available, publish the npm package locally after the GitHub release assets exist:

```sh
npm run publish:npm -- --channel beta --version 0.1.0-beta.1 --dry-run
npm run publish:npm -- --channel beta --version 0.1.0-beta.1
```

The local publish script builds a temporary npm package with matching `rucycle.releaseChannel` and `rucycle.releaseTag` metadata, then publishes it with the corresponding npm dist-tag. Use `--channel alpha` for alpha releases and `--channel stable --version 0.1.0` for `latest`. Local publishing uses your npm login session or `NODE_AUTH_TOKEN`.

No long-lived `NPM_TOKEN` secret is needed for the CI Trusted Publishing path.

## Safety Notes

`rucycle` invokes `cargo clean` with an explicit absolute `--target-dir` and `build.build-dir` override, both pinned to the selected project's validated local `target/` after staging it under a sibling name. Environment variables, Cargo configuration, and workspace defaults cannot expand those output paths. `--frozen` prevents network access and Cargo lockfile updates. A missing local target is a no-op.

Immediately before invoking Cargo, cleanup validates the project and target again. It refuses targets without the Cargo `CACHEDIR.TAG` signature, symlinks/reparse points, files with hard links outside the target, special files, nested Cargo manifests or VCS metadata (including `.jj`, `.bzr`, `_darcs`, Fossil and bare Git repositories), and paths that Cargo could interpret as build-directory templates. Crate manifests under `target/package/` are allowed as Cargo packaging output, but repository metadata is refused there too. Cargo's internal artifact hard links are allowed only when every link is accounted for inside the target. On Unix it also refuses filesystem device boundaries. Standard source locations, declared source/build/readme/license paths (including discovered workspace members and readme/license paths inherited from enclosing workspaces), path dependencies declared by any dependency/patch/replace/platform table, source-tree links into the target, and Git-tracked target files are protected. Git inspection failures and other unreadable/ambiguous paths cause a skip, not a forced cleanup. Non-interactive runs return a nonzero status when a safety check skips a project; in the TUI, move to a skipped row to see its reason.

The target's filesystem identity is captured before validation and rechecked afterwards. It is then renamed to an unoccupied sibling name and its identity is verified again to detect directory replacement. Both staging and rollback use atomic moves that refuse existing destinations, including empty directories. On a failed clean the directory is moved back, or its retained location is reported if the original name was recreated. A successful Cargo exit is only reported as cleaned once the staged directory is confirmed absent; remaining data is preserved and reported as a failure. `git` and `cargo` are resolved from PATH with relative/current-directory entries ignored, Git inspection runs with a timeout, and repository markers that are not plain directories or files fail closed.

An active `cargo` process found in the same project tree causes a skip. If a detected Cargo process's project cannot be determined, cleanup is also refused. These checks are snapshots, not locks or a security sandbox. They cannot guarantee protection against changes racing inside a validated directory between the final check and Cargo's deletion, same-device bind mounts, a compromised Cargo/toolchain, or unique untracked data stored inside an otherwise valid build cache. Keep irreplaceable files outside `target/`, retain backups, and avoid concurrent builds or filesystem changes during cleanup. No cleanup tool can honestly promise unconditional absolute safety.

In interactive mode you can inspect and change the selection and must confirm before cleanup. `--dry-run` only lists projects and never invokes cleanup, including when combined with `--yes`; it is not a certificate that a later cleanup will pass safety validation.

## License

MIT
