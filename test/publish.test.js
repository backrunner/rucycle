const assert = require("node:assert/strict");
const test = require("node:test");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const [major, minor] = process.versions.node.split(".").map(Number);
const supportsTypeStripping = major > 22 || (major === 22 && minor >= 6);

for (const packagingFailure of [false, true]) {
  test(packagingFailure ? "publish failure removes staging directory" : "failed publish prerequisites do not create staging directory", {
    skip: supportsTypeStripping ? false : "publishing requires Node 22.6+ type stripping"
  }, () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "rucycle-publish-test-"));
    try {
      const scripts = path.join(root, "scripts");
      const staging = path.join(root, "staging");
      fs.mkdirSync(scripts);
      fs.mkdirSync(staging);
      const script = path.join(scripts, "publish-npm.ts");
      fs.copyFileSync(path.resolve(__dirname, "../scripts/publish-npm.ts"), script);
      fs.writeFileSync(path.join(root, "package.json"), JSON.stringify({
        name: "rucycle-test-fixture",
        version: "0.1.0",
        files: ["missing-file"]
      }));
      const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith("GIT_")));
      const result = spawnSync(process.execPath, [
        "--experimental-strip-types", script,
        "--channel", "stable", "--version", "0.1.0",
        "--dry-run", "--skip-release-check",
        ...(packagingFailure ? ["--allow-dirty"] : [])
      ], {
        cwd: root,
        env: { ...env, TMPDIR: staging, TMP: staging, TEMP: staging },
        encoding: "utf8",
        timeout: 5000
      });
      assert.equal(result.error, undefined);
      assert.equal(result.status, 1, result.stdout + result.stderr);
      assert.match(result.stderr, packagingFailure ? /package file .* was not found/ : /failed to inspect git status/);
      assert.deepEqual(fs.readdirSync(staging), []);
      assert.ok(fs.existsSync(path.join(root, "package.json")));
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
}
