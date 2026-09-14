const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");

test("Dependabot monitors both JavaScript package roots", () => {
  const config = fs.readFileSync(path.join(root, ".github/dependabot.yml"), "utf8");

  assert.match(config, /- package-ecosystem: npm\n\s+directory: \/\n/);
  assert.match(config, /- package-ecosystem: npm\n\s+directory: \/website\n/);
});
