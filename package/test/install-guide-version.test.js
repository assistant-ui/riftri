const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
const read = (file) => fs.readFileSync(path.join(root, file), "utf8");

const platforms = [
  "riftri-darwin-arm64",
  "riftri-darwin-x64",
  "riftri-linux-arm64-gnu",
  "riftri-linux-arm64-musl",
  "riftri-linux-x64-gnu",
  "riftri-linux-x64-musl",
  "riftri-win32-arm64",
  "riftri-win32-x64",
];

test("the installation guide's archive table matches the current version", () => {
  const { version } = JSON.parse(read("package.json"));
  const guide = read("docs/install.md");
  for (const platform of platforms) {
    assert.ok(
      guide.includes(`${platform}-v${version}.tar.gz`),
      `docs/install.md must list ${platform}-v${version}.tar.gz; bump the archive table when releasing`,
    );
  }
});

test("the installation guide pins the current version in its worked examples", () => {
  const { version } = JSON.parse(read("package.json"));
  const guide = read("docs/install.md");
  for (const example of [
    `bash -s -- v${version}`,
    `version=${version}`,
    `$Version = '${version}'`,
  ]) {
    assert.ok(
      guide.includes(example),
      `docs/install.md must show \`${example}\`; bump the pinned examples when releasing`,
    );
  }
});

test("only deliberate historical references name an older release", () => {
  const { version } = JSON.parse(read("package.json"));
  const guide = read("docs/install.md");
  // Statements about when a capability started are facts about that release
  // and must not be bumped, so they are listed here explicitly. Any other
  // stale version is an example that was missed.
  const historical = ["Releases after v0.2.1 also publish signed build provenance"];
  let remaining = guide;
  for (const statement of historical) {
    assert.ok(remaining.includes(statement), `stale allowlist entry: ${statement}`);
    remaining = remaining.replace(statement, "");
  }
  const stale = [...remaining.matchAll(/\bv?(\d+\.\d+\.\d+)\b/g)]
    .map((match) => match[1])
    .filter((found) => found !== version);
  assert.deepEqual(
    [...new Set(stale)],
    [],
    "docs/install.md references a version that is neither current nor an allowed historical statement",
  );
});
