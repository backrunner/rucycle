#!/usr/bin/env node

import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  writeFileSync
} from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

export const RELEASE_ARCHIVES = Object.freeze([
  {
    key: "darwin-arm64",
    archive: "rucycle-darwin-arm64.tar.gz",
    os: "macos",
    arch: "arm"
  },
  {
    key: "darwin-x64",
    archive: "rucycle-darwin-x64.tar.gz",
    os: "macos",
    arch: "intel"
  },
  {
    key: "linux-arm64",
    archive: "rucycle-linux-arm64.tar.gz",
    os: "linux",
    arch: "arm"
  },
  {
    key: "linux-x64",
    archive: "rucycle-linux-x64.tar.gz",
    os: "linux",
    arch: "intel"
  }
]);

const semverPattern =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

function usage() {
  return [
    "usage:",
    "  node scripts/update-homebrew-formula.mjs --version <semver> --asset-dir <dist> --output <Formula/rucycle.rb>",
    "  node scripts/update-homebrew-formula.mjs --version <semver> --asset-dir <dist> --tap-root <path>",
    "",
    "options:",
    "  --allow-prerelease  allow generating a formula for a prerelease version",
    "  --asset-dir <path>  directory containing release archives, defaults to dist",
    "  --output <path>     write the formula to this exact path",
    "  --repo <owner/repo> GitHub repository, defaults to backrunner/rucycle",
    "  --tag <tag>         release tag, defaults to v<version>",
    "  --tap-root <path>   checkout root; writes Formula/rucycle.rb inside it",
    "  --version <semver>  release version"
  ].join("\n");
}

function fail(message) {
  console.error(`update-homebrew-formula: ${message}`);
  process.exit(1);
}

export function parseArgs(argv) {
  const values = new Map();

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--help" || arg === "-h") {
      console.log(usage());
      process.exit(0);
    }
    if (!arg.startsWith("--")) {
      throw new Error(`unexpected argument: ${arg}`);
    }

    const key = arg.slice(2);
    const next = argv[index + 1];
    if (next === undefined || next.startsWith("--")) {
      values.set(key, true);
    } else {
      values.set(key, next);
      index += 1;
    }
  }

  const version = requiredValue(values, "version");
  const output = optionalValue(values, "output");
  const tapRoot = optionalValue(values, "tap-root");

  if (!output && !tapRoot) {
    throw new Error("pass either --output or --tap-root");
  }

  if (output && tapRoot) {
    throw new Error("pass only one of --output or --tap-root");
  }

  validateVersion(version, values.has("allow-prerelease"));

  return {
    allowPrerelease: values.has("allow-prerelease"),
    assetDir: optionalValue(values, "asset-dir") ?? "dist",
    output: output ?? path.join(tapRoot, "Formula", "rucycle.rb"),
    repo: optionalValue(values, "repo") ?? "backrunner/rucycle",
    tag: optionalValue(values, "tag") ?? `v${version}`,
    version
  };
}

function requiredValue(values, key) {
  const value = values.get(key);
  if (value === undefined || value === true) {
    throw new Error(`missing --${key}`);
  }
  return value;
}

function optionalValue(values, key) {
  const value = values.get(key);
  if (value === undefined) {
    return undefined;
  }
  if (value === true) {
    throw new Error(`--${key} requires a value`);
  }
  return value;
}

function validateVersion(version, allowPrerelease) {
  const match = semverPattern.exec(version);
  if (!match) {
    throw new Error(`invalid semver version: ${version}`);
  }
  if (match[4] && !allowPrerelease) {
    throw new Error(
      `Homebrew tap updates require a stable version; pass --allow-prerelease to generate ${version}`
    );
  }
}

export function sha256File(file) {
  return createHash("sha256").update(readFileSync(file)).digest("hex");
}

export function findReleaseArchive(assetDir, archiveName) {
  const entries = readdirSync(assetDir, { withFileTypes: true });
  for (const entry of entries) {
    const candidate = path.join(assetDir, entry.name);
    if (entry.isFile() && entry.name === archiveName) {
      return candidate;
    }
    if (entry.isDirectory()) {
      const found = findReleaseArchive(candidate, archiveName);
      if (found) {
        return found;
      }
    }
  }
  return undefined;
}

export function collectChecksums(assetDir) {
  if (!existsSync(assetDir)) {
    throw new Error(`asset directory was not found: ${assetDir}`);
  }

  const checksums = new Map();
  for (const target of RELEASE_ARCHIVES) {
    const archivePath = findReleaseArchive(assetDir, target.archive);
    if (!archivePath) {
      throw new Error(`release archive was not found under ${assetDir}: ${target.archive}`);
    }
    checksums.set(target.key, sha256File(archivePath));
  }
  return checksums;
}

function releaseUrl(repo, tag, archive) {
  return `https://github.com/${repo}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(archive)}`;
}

function urlAndSha(repo, tag, target, checksums) {
  const sha256 = checksums.get(target.key);
  if (!sha256) {
    throw new Error(`missing checksum for ${target.key}`);
  }

  return [
    `      url "${releaseUrl(repo, tag, target.archive)}"`,
    `      sha256 "${sha256}"`
  ].join("\n");
}

export function renderFormula({ checksums, repo = "backrunner/rucycle", tag, version }) {
  const targets = Object.fromEntries(RELEASE_ARCHIVES.map((target) => [target.key, target]));

  return `# This file is generated by rucycle's release workflow.
class Rucycle < Formula
  desc "Fast TUI for finding Rust projects and cleaning Cargo build artifacts"
  homepage "https://github.com/${repo}"
  version "${version}"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
${urlAndSha(repo, tag, targets["darwin-arm64"], checksums)}
    else
${urlAndSha(repo, tag, targets["darwin-x64"], checksums)}
    end
  end

  on_linux do
    if Hardware::CPU.arm?
${urlAndSha(repo, tag, targets["linux-arm64"], checksums)}
    else
${urlAndSha(repo, tag, targets["linux-x64"], checksums)}
    end
  end

  def install
    binary = Dir["rucycle-*/rucycle"].first
    bin.install binary => "rucycle"
  end

  test do
    assert_match "rucycle #{version}", shell_output("#{bin}/rucycle --version")
  end
end
`;
}

export function writeFormula(options) {
  const checksums = collectChecksums(options.assetDir);
  const formula = renderFormula({
    checksums,
    repo: options.repo,
    tag: options.tag,
    version: options.version
  });

  mkdirSync(path.dirname(options.output), { recursive: true });
  writeFileSync(options.output, formula);
  return options.output;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const output = writeFormula(options);
  console.log(`wrote ${output}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    fail(error instanceof Error ? error.message : String(error));
  });
}
