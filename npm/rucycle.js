#!/usr/bin/env node

const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const exe = process.platform === "win32" ? "rucycle.exe" : "rucycle";

function candidatePaths() {
  const repoRoot = path.resolve(__dirname, "..");
  return [
    path.join(__dirname, "bin", exe),
    path.join(repoRoot, "target", "release", exe),
    path.join(repoRoot, "target", "debug", exe)
  ];
}

function resolveBinary() {
  for (const candidate of candidatePaths()) {
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }

  const platform = `${process.platform}/${process.arch}`;
  console.error(
    [
      `rucycle does not have a native binary for ${platform}.`,
      "The npm install step should download it from the GitHub release.",
      "If you are developing locally, run `cargo build --release` or `npm run test:install` first."
    ].join("\n")
  );
  process.exit(1);
}

const result = spawnSync(resolveBinary(), process.argv.slice(2), {
  stdio: "inherit"
});

if (result.error) {
  console.error(`failed to start rucycle: ${result.error.message}`);
  process.exit(1);
}

process.exit(result.status ?? 1);
