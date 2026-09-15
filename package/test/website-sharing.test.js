const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");
const root = path.resolve(__dirname, "../..");

test("sharing metadata identifies the canonical domain and large preview card", () => {
  const source = fs.readFileSync(path.join(root, "website/src/app/layout.tsx"), "utf8");
  for (const value of ['canonical: "https://riftri.dev/"', "openGraph:", 'card: "summary_large_image"', "https://riftri.dev/og.png"]) {
    assert.ok(source.includes(value), value);
  }
});

test("sharing card is a real 1200 by 630 PNG", () => {
  const bytes = fs.readFileSync(path.join(root, "website/public/og.png"));
  assert.deepEqual(bytes.subarray(0, 8), Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]));
  assert.equal(bytes.readUInt32BE(16), 1200);
  assert.equal(bytes.readUInt32BE(20), 630);
  assert.ok(bytes.length < 250_000, "keep link previews small");
});
