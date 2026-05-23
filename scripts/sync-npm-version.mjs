#!/usr/bin/env node

import { readFileSync, writeFileSync } from "node:fs";

const version = process.argv[2];

if (!version) {
  console.error("usage: sync-npm-version.mjs <version>");
  process.exit(1);
}

const manifestPath = "package.json";
const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
manifest.version = version;

writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
