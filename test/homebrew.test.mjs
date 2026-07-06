import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  RELEASE_ARCHIVES,
  collectChecksums,
  parseArgs,
  renderFormula,
  sha256File,
  writeFormula
} from "../scripts/update-homebrew-formula.mjs";

function withReleaseArchives(callback) {
  const tempDir = mkdtempSync(path.join(os.tmpdir(), "rucycle-homebrew-test-"));
  try {
    const assets = path.join(tempDir, "dist");
    mkdirSync(path.join(assets, "nested"), { recursive: true });

    for (const target of RELEASE_ARCHIVES) {
      const dir = target.key.includes("linux") ? path.join(assets, "nested") : assets;
      writeFileSync(path.join(dir, target.archive), `archive:${target.key}`);
    }

    callback({ assets, tempDir });
  } finally {
    rmSync(tempDir, { force: true, recursive: true });
  }
}

test("parses formula generation arguments", () => {
  assert.deepEqual(
    parseArgs([
      "--version",
      "0.1.0",
      "--asset-dir",
      "dist",
      "--tap-root",
      "../homebrew-rucycle"
    ]),
    {
      allowPrerelease: false,
      assetDir: "dist",
      output: path.join("../homebrew-rucycle", "Formula", "rucycle.rb"),
      repo: "BackRunner/rucycle",
      tag: "v0.1.0",
      version: "0.1.0"
    }
  );
});

test("rejects prerelease formula generation by default", () => {
  assert.throws(
    () => parseArgs(["--version", "0.1.0-beta.1", "--output", "Formula/rucycle.rb"]),
    /stable version/
  );
});

test("collects checksums from nested release artifacts", () => {
  withReleaseArchives(({ assets }) => {
    const checksums = collectChecksums(assets);
    const archive = path.join(assets, "rucycle-darwin-arm64.tar.gz");

    assert.equal(checksums.get("darwin-arm64"), sha256File(archive));
    assert.equal(checksums.size, RELEASE_ARCHIVES.length);
  });
});

test("renders a Homebrew formula for all supported platforms", () => {
  withReleaseArchives(({ assets }) => {
    const formula = renderFormula({
      checksums: collectChecksums(assets),
      repo: "BackRunner/rucycle",
      tag: "v0.1.0",
      version: "0.1.0"
    });

    assert.match(formula, /class Rucycle < Formula/);
    assert.match(formula, /on_macos do/);
    assert.match(formula, /on_linux do/);
    assert.match(formula, /rucycle-darwin-arm64\.tar\.gz/);
    assert.match(formula, /rucycle-linux-x64\.tar\.gz/);
    assert.match(formula, /assert_match "rucycle #\{version\}"/);
  });
});

test("writes Formula/rucycle.rb inside a tap root", () => {
  withReleaseArchives(({ assets, tempDir }) => {
    const output = writeFormula({
      assetDir: assets,
      output: path.join(tempDir, "tap", "Formula", "rucycle.rb"),
      repo: "BackRunner/rucycle",
      tag: "v0.1.0",
      version: "0.1.0"
    });

    const formula = readFileSync(output, "utf8");
    assert.match(formula, /version "0.1.0"/);
    assert.match(formula, /sha256 "[a-f0-9]{64}"/);
  });
});
