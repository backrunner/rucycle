const assert = require("node:assert/strict");
const test = require("node:test");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const http = require("node:http");
const { execFile } = require("node:child_process");
const { promisify } = require("node:util");

const {
  inferReleaseChannel,
  parseArgs,
  releaseInfo,
  releaseUrl
} = require("../npm/install.js");

test("infers stable channel for normal semver versions", () => {
  assert.equal(inferReleaseChannel("0.1.0"), "stable");
});

test("infers prerelease channel from the first prerelease identifier", () => {
  assert.equal(inferReleaseChannel("0.1.0-alpha.1"), "alpha");
  assert.equal(inferReleaseChannel("0.1.0-beta.4"), "beta");
  assert.equal(inferReleaseChannel("0.1.0-BETA.4"), "beta");
});

test("rejects prerelease versions without a named channel", () => {
  assert.throws(
    () => inferReleaseChannel("0.1.0-1"),
    /cannot infer release channel/
  );
});

test("builds release info from package metadata and overrides", () => {
  assert.deepEqual(releaseInfo({ version: "0.1.0-beta.2" }), {
    version: "0.1.0-beta.2",
    channel: "beta",
    tag: "v0.1.0-beta.2"
  });

  assert.deepEqual(
    releaseInfo({
      version: "0.1.0",
      rucycle: {
        releaseChannel: "alpha",
        releaseTag: "v0.1.0-alpha.5"
      }
    }),
    {
      version: "0.1.0",
      channel: "alpha",
      tag: "v0.1.0-alpha.5"
    }
  );

  assert.deepEqual(
    releaseInfo(
      {
        version: "0.1.0",
        rucycle: {
          releaseChannel: "alpha",
          releaseTag: "v0.1.0-alpha.5"
        }
      },
      { version: "0.1.0-beta.1" }
    ),
    {
      version: "0.1.0-beta.1",
      channel: "beta",
      tag: "v0.1.0-beta.1"
    }
  );
});

test("builds encoded release asset URLs", () => {
  const previousBaseUrl = process.env.RUCYCLE_RELEASE_BASE_URL;
  process.env.RUCYCLE_RELEASE_BASE_URL = "https://releases.rucycle.test/download/";

  try {
    assert.equal(
      releaseUrl("rucycle-linux-x64", { tag: "v0.1.0-beta.1" }),
      "https://releases.rucycle.test/download/v0.1.0-beta.1/rucycle-linux-x64"
    );
    assert.equal(
      releaseUrl("rucycle-linux-x64", { tag: "v0.1.0+build.7" }),
      "https://releases.rucycle.test/download/v0.1.0%2Bbuild.7/rucycle-linux-x64"
    );
  } finally {
    if (previousBaseUrl === undefined) {
      delete process.env.RUCYCLE_RELEASE_BASE_URL;
    } else {
      process.env.RUCYCLE_RELEASE_BASE_URL = previousBaseUrl;
    }
  }
});

test("parses installer arguments", () => {
  assert.deepEqual(parseArgs(["--version", "0.1.0-beta.1", "--channel", "beta"]), {
    version: "0.1.0-beta.1",
    channel: "beta"
  });
});

for (const interrupted of [false, true]) {
  test(interrupted ? "interrupted download preserves installed binary" : "complete download atomically replaces installed binary", async () => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "rucycle-install-test-"));
    const npmDir = path.join(root, "npm");
    const binDir = path.join(npmDir, "bin");
    const exe = process.platform === "win32" ? "rucycle.exe" : "rucycle";
    const destination = path.join(binDir, exe);
    const previous = "previous working binary";
    const replacement = "new complete binary";
    fs.mkdirSync(binDir, { recursive: true });
    fs.copyFileSync(require.resolve("../npm/install.js"), path.join(npmDir, "install.js"));
    fs.writeFileSync(path.join(root, "package.json"), JSON.stringify({ version: "0.1.0" }));
    fs.writeFileSync(destination, previous);
    const server = http.createServer((request, response) => {
      response.writeHead(200, { "Content-Length": Buffer.byteLength(replacement) });
      if (interrupted) {
        response.write("partial");
        setImmediate(() => response.destroy());
      } else {
        response.end(replacement);
      }
    });
    try {
      await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
      const run = promisify(execFile)(process.execPath, [path.join(npmDir, "install.js")], {
        env: {
          ...process.env,
          RUCYCLE_SKIP_DOWNLOAD: "0",
          RUCYCLE_RELEASE_BASE_URL: `http://127.0.0.1:${server.address().port}`
        },
        timeout: 5000
      });
      if (interrupted) {
        await assert.rejects(run);
      } else {
        await run;
      }
      assert.equal(fs.readFileSync(destination, "utf8"), interrupted ? previous : replacement);
      assert.deepEqual(fs.readdirSync(binDir), [exe]);
    } finally {
      await new Promise((resolve) => server.close(resolve));
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
}
