const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
const read = (file) => fs.readFileSync(path.join(root, file), "utf8");

test("current product copy describes capabilities without blanket maturity labels", () => {
  // Historical changelog entries keep the wording used by those releases.
  for (const file of [
    "README.md", "SUPPORT.md", "SECURITY.md", "package.json",
    "docs/safety.md", "docs/backing-out.md", "docs/global-activation.md",
    "docs/linux-reflink.md", "docs/windows-refs.md",
    "website/public/index.md", "website/content/introduction.md",
    "website/content/guides/safety.md", "website/content/guides/backing-out.md",
    "website/content/guides/windows-refs.md",
    "website/src/components/faq.tsx", "website/src/components/materialization-track.tsx",
  ]) {
    assert.doesNotMatch(read(file), /\bexperimental\b|pre-release software|production-ready/i, file);
  }
  assert.equal(JSON.parse(read("package.json")).description,
    "Native copy-on-write Git worktrees for parallel development");
});

test("capability wording retains backup guidance and security boundaries", () => {
  for (const file of ["README.md", "docs/safety.md", "website/public/index.md", "website/content/guides/safety.md"]) {
    assert.match(read(file).replace(/^> ?/gm, "").replace(/\s+/g, " "), /Keep important work committed or backed up/i, file);
  }
  assert.match(read("SECURITY.md"), /not a security sandbox/);
  assert.match(read("SECURITY.md"), /latest tagged release and `main` receive security fixes/);
  assert.match(read("SUPPORT.md"), /unsupported filesystems stop before mutation/);
  assert.match(read("docs/windows-refs.md"), /NTFS is not an optimized backend/);
  assert.match(read("README.md"), /mount-capable namespace or a one-time\nhelper/);
});
