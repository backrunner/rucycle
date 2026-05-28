#!/usr/bin/env node

const fs = require("node:fs");
const http = require("node:http");
const https = require("node:https");
const path = require("node:path");

const supportedTargets = new Map([
  ["darwin:arm64", { asset: "rucycle-darwin-arm64", exe: "rucycle" }],
  ["darwin:x64", { asset: "rucycle-darwin-x64", exe: "rucycle" }],
  ["linux:arm64", { asset: "rucycle-linux-arm64", exe: "rucycle" }],
  ["linux:x64", { asset: "rucycle-linux-x64", exe: "rucycle" }],
  ["win32:x64", { asset: "rucycle-win32-x64.exe", exe: "rucycle.exe" }]
]);

const semverPattern =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

function parseArgs(argv) {
  const values = {};
  for (let index = 0; index < argv.length; index += 1) {
    const key = argv[index];
    if (!key.startsWith("--")) {
      throw new Error(`unexpected argument: ${key}`);
    }
    values[key.slice(2)] = argv[index + 1] ?? true;
    if (values[key.slice(2)] !== true) {
      index += 1;
    }
  }
  return values;
}

function packageManifest() {
  return JSON.parse(fs.readFileSync(path.resolve(__dirname, "..", "package.json"), "utf8"));
}

function packageVersion(manifest = packageManifest()) {
  return manifest.version;
}

function targetForCurrentPlatform() {
  const key = `${process.platform}:${process.arch}`;
  const target = supportedTargets.get(key);
  if (!target) {
    const supported = Array.from(supportedTargets.keys()).join(", ");
    throw new Error(
      `unsupported platform ${process.platform}/${process.arch}; supported targets: ${supported}`
    );
  }
  return target;
}

function ensureExecutable(file) {
  if (process.platform !== "win32") {
    fs.chmodSync(file, 0o755);
  }
}

function copyLocalBinary(sourceDir, destination, exe) {
  const source = path.resolve(sourceDir, exe);
  if (!fs.existsSync(source)) {
    throw new Error(`local binary not found: ${source}`);
  }
  fs.copyFileSync(source, destination);
  ensureExecutable(destination);
}

function inferReleaseChannel(version) {
  const match = semverPattern.exec(version);
  if (!match) {
    throw new Error(`invalid package version: ${version}`);
  }

  const prerelease = match[4];
  if (!prerelease) {
    return "stable";
  }

  const channel = prerelease.split(".")[0].toLowerCase();
  if (!/^[a-z][a-z0-9-]*$/.test(channel)) {
    throw new Error(
      `cannot infer release channel from prerelease version ${version}; expected a channel prefix like alpha.1 or beta.1`
    );
  }

  return channel;
}

function releaseInfo(manifest = packageManifest(), overrides = {}) {
  const version = overrides.version ?? packageVersion(manifest);
  const metadata = manifest.rucycle ?? {};
  const channel =
    overrides.channel ??
    (overrides.version === undefined ? metadata.releaseChannel : undefined) ??
    inferReleaseChannel(version);
  const tag =
    overrides.tag ??
    (overrides.version === undefined ? metadata.releaseTag : undefined) ??
    `v${version}`;

  return {
    version,
    channel,
    tag
  };
}

function releaseUrl(asset, release) {
  const baseUrl = (
    process.env.RUCYCLE_RELEASE_BASE_URL ??
    "https://github.com/BackRunner/rucycle/releases/download"
  ).replace(/\/+$/, "");
  return `${baseUrl}/${encodeURIComponent(release.tag)}/${encodeURIComponent(asset)}`;
}

function download(url, destination, redirects = 0) {
  if (redirects > 5) {
    return Promise.reject(new Error("too many redirects while downloading rucycle"));
  }

  const client = url.startsWith("https:") ? https : http;
  return new Promise((resolve, reject) => {
    const request = client.get(
      url,
      {
        headers: {
          "User-Agent": "rucycle-installer"
        }
      },
      (response) => {
        const status = response.statusCode ?? 0;
        if ([301, 302, 303, 307, 308].includes(status) && response.headers.location) {
          response.resume();
          resolve(download(new URL(response.headers.location, url).toString(), destination, redirects + 1));
          return;
        }

        if (status < 200 || status >= 300) {
          response.resume();
          reject(new Error(`download failed with HTTP ${status}: ${url}`));
          return;
        }

        const file = fs.createWriteStream(destination, { mode: 0o755 });
        response.pipe(file);
        file.on("finish", () => {
          file.close(() => resolve());
        });
        file.on("error", reject);
      }
    );
    request.on("error", reject);
  });
}

async function main() {
  if (process.env.RUCYCLE_SKIP_DOWNLOAD === "1") {
    return;
  }

  const args = parseArgs(process.argv.slice(2));
  const target = targetForCurrentPlatform();
  const binDir = path.resolve(__dirname, "bin");
  const destination = path.join(binDir, target.exe);

  fs.mkdirSync(binDir, { recursive: true });

  if (args["from-local"]) {
    copyLocalBinary(args["from-local"], destination, target.exe);
  } else {
    const release = releaseInfo(packageManifest(), {
      version: args.version ?? undefined,
      channel: args.channel ?? undefined,
      tag: args["release-tag"] ?? undefined
    });
    const url = releaseUrl(target.asset, release);
    await download(url, destination);
    ensureExecutable(destination);
  }

  if (!fs.existsSync(destination)) {
    throw new Error(`rucycle binary was not installed: ${destination}`);
  }
}

if (require.main === module) {
  main().catch((error) => {
    console.error(`rucycle install failed: ${error.message}`);
    console.error("Install from source with `cargo install --path .` or report the issue.");
    process.exit(1);
  });
}

module.exports = {
  inferReleaseChannel,
  packageManifest,
  packageVersion,
  parseArgs,
  releaseInfo,
  releaseUrl,
  targetForCurrentPlatform
};
