# Contributing

Thanks for taking a look at `rucycle`.

## Development Setup

You need a recent stable Rust toolchain and Node.js 18 or newer.

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
npm test
```

## Pull Requests

- Keep changes focused and easy to review.
- Add or update tests for changes to scanning, sorting, or cleaning behavior.
- Run formatting, clippy, and tests before opening a pull request.
- Avoid checking in generated build artifacts from `target/`, `dist/`, or npm package tarballs.

## Release Notes

User-facing changes should be reflected in `README.md` when they affect installation, usage, keyboard controls, or release packaging.
