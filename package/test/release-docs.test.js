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

test("public docs do not call sparse checkout unsupported", () => {
  // `riftri worktree add --sparse-dir` ships. README, the agent reference, and
  // troubleshooting all grouped sparse checkout with submodules and custom
  // filters, which told readers a working feature did not exist.
  for (const doc of ["README.md", "website/public/index.md", "docs/troubleshooting.md"]) {
    const text = read(doc);
    assert.match(text, /--sparse-dir/, `${doc} must point at the supported interface`);
    // The unsupported half is the repository's own profile, not the feature.
    assert.doesNotMatch(
      text,
      /(?:filters, |attributes, )sparse checkout(?:,| and)/,
      `${doc} still lists sparse checkout as an unsupported checkout feature`,
    );
  }
  assert.doesNotMatch(
    read("docs/troubleshooting.md"),
    /not yet supported and fail closed/,
    "troubleshooting still says sparse checkout is unsupported",
  );
});

test("both Node API guides describe npm availability the same way", () => {
  // The rendered page and the canonical Markdown are separate files, so they
  // drifted: riftri.dev/docs/node-api still told readers npm was blocked long
  // after 0.3.5 published. Guard both, not just the one that got edited.
  for (const guide of ["docs/node-api.md", "website/content/guides/node-api.md"]) {
    const text = read(guide);
    assert.match(text, /npm install riftri/, `${guide} must show the install command`);
    assert.match(text, /Windows on ARM64|Windows ARM64/, `${guide} must name the exception`);

    assert.doesNotMatch(text, /blocked pending registry review/, `${guide} is stale`);
    assert.doesNotMatch(text, /does not work yet/, `${guide} is stale`);
    // client.js requires ./platform.js, so "copy this one file" never worked.
    assert.doesNotMatch(
      text,
      /copy\s+`?package\/lib\/client\.js`?\s+into your project/,
      `${guide} recommends an incomplete copy-only fallback`,
    );
  }
});
