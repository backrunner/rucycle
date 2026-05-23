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

function packageVersion() {
  const manifest = JSON.parse(
    fs.readFileSync(path.resolve(__dirname, "..", "package.json"), "utf8")
  );
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

function releaseUrl(asset, version) {
  const baseUrl =
    process.env.RUCYCLE_RELEASE_BASE_URL ??
    "https://github.com/BackRunner/rucycle/releases/download";
  return `${baseUrl}/v${version}/${asset}`;
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
    const url = releaseUrl(target.asset, args.version ?? packageVersion());
    await download(url, destination);
    ensureExecutable(destination);
  }

  if (!fs.existsSync(destination)) {
    throw new Error(`rucycle binary was not installed: ${destination}`);
  }
}

main().catch((error) => {
  console.error(`rucycle install failed: ${error.message}`);
  console.error("Install from source with `cargo install --path .` or report the issue.");
  process.exit(1);
});
