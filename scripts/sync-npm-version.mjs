#!/usr/bin/env node

import { appendFileSync, readFileSync, writeFileSync } from "node:fs";

const version = process.argv[2];

if (!version) {
  console.error("usage: sync-npm-version.mjs <version>");
  process.exit(1);
}

const semverPattern =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

function releaseChannelForVersion(value) {
  const match = semverPattern.exec(value);
  if (!match) {
    throw new Error(`invalid package version: ${value}`);
  }

  const prerelease = match[4];
  if (!prerelease) {
    return "stable";
  }

  const channel = prerelease.split(".")[0].toLowerCase();
  if (!/^[a-z][a-z0-9-]*$/.test(channel)) {
    throw new Error(
      `cannot infer release channel from prerelease version ${value}; expected a channel prefix like alpha.1 or beta.1`
    );
  }

  return channel;
}

const releaseChannel = releaseChannelForVersion(version);
const npmTag = releaseChannel === "stable" ? "latest" : releaseChannel;
const releaseTag = `v${version}`;

const manifestPath = "package.json";
const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
manifest.version = version;
manifest.rucycle = {
  ...(manifest.rucycle ?? {}),
  releaseChannel,
  releaseTag
};

writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);

if (process.env.GITHUB_OUTPUT) {
  appendFileSync(
    process.env.GITHUB_OUTPUT,
    [
      `version=${version}`,
      `release_channel=${releaseChannel}`,
      `release_tag=${releaseTag}`,
      `npm_tag=${npmTag}`,
      `prerelease=${releaseChannel === "stable" ? "false" : "true"}`
    ].join("\n") + "\n"
  );
}
