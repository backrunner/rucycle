#!/usr/bin/env node

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

type Channel = "stable" | "latest" | "alpha" | "beta";

type PublishOptions = {
  access: "public" | "restricted";
  allowDirty: boolean;
  channel: Channel;
  dryRun: boolean;
  keepTemp: boolean;
  otp?: string;
  registry?: string;
  releaseTag?: string;
  skipReleaseCheck: boolean;
  version: string;
};

type PackageManifest = {
  files?: string[];
  name: string;
  rucycle?: {
    releaseChannel?: string;
    releaseTag?: string;
  };
  version: string;
  [key: string]: unknown;
};

const repoRoot = path.resolve(__dirname, "..");
const manifestPath = path.join(repoRoot, "package.json");
const semverPattern =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

function usage(): string {
  return [
    "usage:",
    "  node --experimental-strip-types scripts/publish-npm.ts --channel <stable|alpha|beta> --version <semver> [options]",
    "",
    "examples:",
    "  node --experimental-strip-types scripts/publish-npm.ts --channel beta --version 0.1.0-beta.1 --dry-run",
    "  node --experimental-strip-types scripts/publish-npm.ts --channel beta --version 0.1.0-beta.1",
    "  node --experimental-strip-types scripts/publish-npm.ts --channel stable --version 0.1.0",
    "",
    "options:",
    "  --access <public|restricted>   npm publish access, defaults to public",
    "  --allow-dirty                  allow publishing from a dirty git worktree",
    "  --dry-run                      run npm publish --dry-run",
    "  --keep-temp                    keep the temporary package directory",
    "  --otp <code>                   pass an npm two-factor auth code",
    "  --registry <url>               publish to a custom npm registry",
    "  --release-tag <tag>            GitHub release tag, defaults to v<version>",
    "  --skip-release-check           skip the GitHub release existence check"
  ].join("\n");
}

function fail(message: string): never {
  console.error(`publish-npm: ${message}`);
  process.exit(1);
}

function readJson(file: string): PackageManifest {
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

function parseArgs(argv: string[]): PublishOptions {
  const values = new Map<string, string | true>();

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--help" || arg === "-h") {
      console.log(usage());
      process.exit(0);
    }
    if (!arg.startsWith("--")) {
      fail(`unexpected argument: ${arg}`);
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

  const channel = normalizeChannel(valueFor(values, "channel"));
  const version = valueFor(values, "version");
  const access = (optionalValueFor(values, "access") ?? "public") as PublishOptions["access"];

  if (access !== "public" && access !== "restricted") {
    fail(`invalid --access ${access}; expected public or restricted`);
  }

  const options: PublishOptions = {
    access,
    allowDirty: values.has("allow-dirty"),
    channel,
    dryRun: values.has("dry-run"),
    keepTemp: values.has("keep-temp"),
    otp: optionalValueFor(values, "otp"),
    registry: optionalValueFor(values, "registry"),
    releaseTag: optionalValueFor(values, "release-tag"),
    skipReleaseCheck: values.has("skip-release-check"),
    version
  };

  validateVersionForChannel(options.version, options.channel);
  return options;
}

function valueFor(values: Map<string, string | true>, key: string): string {
  const value = values.get(key);
  if (value === undefined || value === true) {
    fail(`missing --${key}`);
  }
  return value;
}

function optionalValueFor(values: Map<string, string | true>, key: string): string | undefined {
  const value = values.get(key);
  if (value === undefined) {
    return undefined;
  }
  if (value === true) {
    fail(`--${key} requires a value`);
  }
  return value;
}

function normalizeChannel(value: string): Channel {
  const channel = value.toLowerCase();
  if (channel === "stable" || channel === "latest" || channel === "alpha" || channel === "beta") {
    return channel;
  }
  fail(`invalid --channel ${value}; expected stable, latest, alpha, or beta`);
}

function prereleaseChannel(version: string): string | undefined {
  const match = semverPattern.exec(version);
  if (!match) {
    fail(`invalid semver version: ${version}`);
  }
  return match[4]?.split(".")[0].toLowerCase();
}

function validateVersionForChannel(version: string, channel: Channel): void {
  const prerelease = prereleaseChannel(version);
  if (channel === "stable" || channel === "latest") {
    if (prerelease) {
      fail(`stable/latest publishes require a non-prerelease version; got ${version}`);
    }
    return;
  }

  if (prerelease !== channel) {
    fail(`--channel ${channel} requires a ${channel} prerelease version; got ${version}`);
  }
}

function npmTagForChannel(channel: Channel): string {
  return channel === "stable" || channel === "latest" ? "latest" : channel;
}

function releaseChannelForChannel(channel: Channel): string {
  return channel === "latest" ? "stable" : channel;
}

function run(command: string, args: string[], options: { cwd?: string; capture?: boolean } = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? repoRoot,
    encoding: "utf8",
    stdio: options.capture ? ["ignore", "pipe", "pipe"] : "inherit"
  });

  return result;
}

function ensureCleanWorktree(allowDirty: boolean): void {
  if (allowDirty) {
    return;
  }

  const result = run("git", ["status", "--porcelain"], { capture: true });
  if (result.status !== 0) {
    fail(`failed to inspect git status:\n${result.stderr}`);
  }
  if (result.stdout.trim()) {
    fail("git worktree is dirty; commit/stash changes or pass --allow-dirty");
  }
}

function ensureNpmAuth(options: PublishOptions): void {
  if (options.dryRun) {
    return;
  }

  const args = ["whoami"];
  if (options.registry) {
    args.push("--registry", options.registry);
  }

  const result = run("npm", args, { capture: true });
  if (result.status !== 0) {
    fail("npm authentication is required for local publish; run `npm login` or set NODE_AUTH_TOKEN");
  }
}

async function ensureReleaseExists(releaseTag: string, skipReleaseCheck: boolean): Promise<void> {
  if (skipReleaseCheck) {
    return;
  }

  const url = `https://api.github.com/repos/BackRunner/rucycle/releases/tags/${encodeURIComponent(releaseTag)}`;
  const response = await fetch(url, {
    headers: {
      "Accept": "application/vnd.github+json",
      "User-Agent": "rucycle-publish-npm"
    }
  });

  if (response.status === 404) {
    fail(`GitHub release ${releaseTag} was not found; create it first or pass --skip-release-check`);
  }
  if (!response.ok) {
    fail(`failed to verify GitHub release ${releaseTag}: HTTP ${response.status}`);
  }
}

function copyPackageFiles(manifest: PackageManifest, tempDir: string): void {
  const files = manifest.files ?? [];
  for (const entry of files) {
    const source = path.join(repoRoot, entry);
    const destination = path.join(tempDir, entry);

    if (!fs.existsSync(source)) {
      fail(`package file listed in package.json was not found: ${entry}`);
    }

    fs.mkdirSync(path.dirname(destination), { recursive: true });
    fs.cpSync(source, destination, {
      force: true,
      preserveTimestamps: true,
      recursive: true,
      verbatimSymlinks: true
    });
  }
}

function writePublishManifest(
  manifest: PackageManifest,
  tempDir: string,
  options: PublishOptions,
  releaseTag: string
): void {
  const releaseChannel = releaseChannelForChannel(options.channel);
  const nextManifest: PackageManifest = {
    ...manifest,
    version: options.version,
    rucycle: {
      ...(manifest.rucycle ?? {}),
      releaseChannel,
      releaseTag
    }
  };

  fs.writeFileSync(path.join(tempDir, "package.json"), `${JSON.stringify(nextManifest, null, 2)}\n`);
}

function publishFromTemp(tempDir: string, options: PublishOptions, npmTag: string): void {
  const args = ["publish", "--access", options.access, "--tag", npmTag];
  if (options.dryRun) {
    args.push("--dry-run");
  }
  if (options.otp) {
    args.push("--otp", options.otp);
  }
  if (options.registry) {
    args.push("--registry", options.registry);
  }

  const result = run("npm", args, { cwd: tempDir });
  if (result.status !== 0) {
    fail(`npm publish failed with exit code ${result.status}`);
  }
}

async function main(): Promise<void> {
  const options = parseArgs(process.argv.slice(2));
  const manifest = readJson(manifestPath);
  const releaseTag = options.releaseTag ?? `v${options.version}`;
  const npmTag = npmTagForChannel(options.channel);
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "rucycle-npm-"));

  ensureCleanWorktree(options.allowDirty);
  ensureNpmAuth(options);
  await ensureReleaseExists(releaseTag, options.skipReleaseCheck);

  console.log(`package: ${manifest.name}@${options.version}`);
  console.log(`channel: ${releaseChannelForChannel(options.channel)}`);
  console.log(`npm tag: ${npmTag}`);
  console.log(`release tag: ${releaseTag}`);
  console.log(`temp dir: ${tempDir}`);

  try {
    copyPackageFiles(manifest, tempDir);
    writePublishManifest(manifest, tempDir, options, releaseTag);
    publishFromTemp(tempDir, options, npmTag);
  } finally {
    if (options.keepTemp) {
      console.log(`kept temp dir: ${tempDir}`);
    } else {
      fs.rmSync(tempDir, { force: true, recursive: true });
    }
  }
}

main().catch((error) => {
  fail(error instanceof Error ? error.message : String(error));
});
