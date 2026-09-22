const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
const read = (file) => fs.readFileSync(path.join(root, file), "utf8");

test("release-facing documentation matches current guarantees", () => {
  const readme = read("README.md");
  assert.match(readme, /MIT License/);
  assert.doesNotMatch(readme, /Apache License 2\.0/);
  // npm publication is live as of 0.3.5; only riftri-win32-arm64 is still
  // held in registry review, so the README must scope the caveat to that
  // platform rather than claiming the whole channel is unavailable.
  assert.match(readme, /npm install --global riftri/);
  assert.match(readme, /Windows on ARM64 is not yet available through npm/);
  assert.doesNotMatch(readme, /npm launcher is a separate distribution channel/);

  const support = read("SUPPORT.md");
  assert.match(support, /APFS/);
  assert.match(support, /Btrfs/);
  assert.match(support, /OverlayFS/);
  assert.match(support, /ReFS/);
  assert.doesNotMatch(support, /Linux and Windows mutation\s+backends are not implemented/);

  const security = read("SECURITY.md");
  assert.doesNotMatch(security, /Until the first tagged release/);

  const guarantees = read("docs/backend-guarantees.md");
  assert.match(guarantees, /Canonical Git LFS paths/);
  assert.match(guarantees, /Snapshot-guarded forced remove/);
  assert.match(guarantees, /Compact pristine view/);
  assert.doesNotMatch(guarantees, /Riftri has no forced managed removal/);
});
