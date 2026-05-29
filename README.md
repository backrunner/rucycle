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
- Runs `cargo clean --manifest-path <Cargo.toml>` for selected projects.
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

From source:

```sh
cargo install --path .
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

Clean every detected project without prompting:

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
| `Space` | Toggle current project |
| `a` | Select all or none |
| `s` | Toggle sorting by target size or modified time |
| `c` | Clean selected projects |
| `q`, `Esc` | Quit |

## How Scanning Works

`rucycle` looks for files named `Cargo.toml`, parses them as TOML, and treats manifests with a `[package]` or `[workspace]` table as Rust projects. It skips expensive discovery inside `target/`, `node_modules/`, and VCS directories, then computes project sizes in parallel.

The "modified" sort key is the newest modification time of any file under the project folder. The "target" size is the size of files under that project's `target/` directory.

## npm Distribution

The `rucycle` npm package is a small JavaScript launcher. During npm install, it downloads the matching native binary from the GitHub Release for the package version and stores it in `npm/bin/`.

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

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
npm run test:installer
npm run test:install
npm test
```

During local development, the npm launcher falls back to `target/release/rucycle` and `target/debug/rucycle`, so `npm test` works after a Cargo build.

`npm run test:install` copies a locally built binary into `npm/bin/` and verifies the same executable path used by `npx rucycle`.

## Release

GitHub Actions includes:

- `CI`: formatting, clippy, tests, release build, local npm install smoke test, and npm launcher smoke test.
- `Release`: builds native binaries for Linux, macOS, and Windows, uploads them to GitHub Releases, then publishes the npm package.

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

`rucycle` only invokes `cargo clean`; it does not remove arbitrary directories itself. Before cleaning each project, it checks for an active `cargo` process in the same project tree, reports that project as skipped if one is found, and continues with the rest. In interactive mode you can inspect and change the selected projects before any clean command runs. For non-interactive cleanup, use `--dry-run` first when scanning a large or unfamiliar directory.

## License

MIT
