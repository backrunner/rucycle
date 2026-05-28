const assert = require("node:assert/strict");
const test = require("node:test");

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
