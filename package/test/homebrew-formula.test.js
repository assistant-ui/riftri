const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
const archives = [
  "riftri-darwin-arm64",
  "riftri-darwin-x64",
  "riftri-linux-arm64-gnu",
  "riftri-linux-arm64-musl",
  "riftri-linux-x64-gnu",
  "riftri-linux-x64-musl",
  "riftri-win32-arm64",
  "riftri-win32-x64",
];

function checksumManifest(version) {
  return `${archives
    .map((name, index) => `${String(index + 1).repeat(64)}  ${name}-v${version}.tar.gz`)
    .join("\n")}\n`;
}

test("the formula pins one checked-in archive and checksum per supported platform", async () => {
  const { renderHomebrewFormula, parseChecksums } = await import(
    "../scripts/update-homebrew-formula.mjs"
  );
  const formula = renderHomebrewFormula({
    version: "9.9.9",
    checksums: parseChecksums(checksumManifest("9.9.9")),
  });

  for (const archive of [
    "riftri-darwin-arm64",
    "riftri-darwin-x64",
    "riftri-linux-arm64-gnu",
    "riftri-linux-x64-gnu",
  ]) {
    const url = `https://github.com/assistant-ui/riftri/releases/download/v9.9.9/${archive}-v9.9.9.tar.gz`;
    assert.ok(formula.includes(url), `missing url for ${archive}`);
  }
  // musl and Windows archives have no Homebrew audience.
  assert.ok(!formula.includes("musl"));
  assert.ok(!formula.includes("win32"));

  assert.equal((formula.match(/sha256 "/g) ?? []).length, 4);
  assert.ok(formula.includes('version "9.9.9"'));
  assert.ok(formula.includes('bin.install "riftri"'));
});

test("rendering rejects a version without matching checksums", async () => {
  const { renderHomebrewFormula, parseChecksums } = await import(
    "../scripts/update-homebrew-formula.mjs"
  );
  assert.throws(
    () =>
      renderHomebrewFormula({
        version: "1.0.0",
        checksums: parseChecksums(checksumManifest("9.9.9")),
      }),
    /missing checksum for riftri-darwin-arm64-v1\.0\.0\.tar\.gz/,
  );
});

test("rendering rejects a tag-shaped version and malformed checksums", async () => {
  const { renderHomebrewFormula, parseChecksums } = await import(
    "../scripts/update-homebrew-formula.mjs"
  );
  assert.throws(
    () => renderHomebrewFormula({ version: "v1.0.0", checksums: new Map() }),
    /expected a bare semantic version/,
  );
  assert.throws(() => parseChecksums("not-a-checksum  riftri.tar.gz\n"), /malformed/);
});

test("updating writes the formula to disk from a checksum manifest", async (t) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-formula-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const checksumsPath = path.join(directory, "SHA256SUMS");
  const formulaPath = path.join(directory, "riftri.rb");
  fs.writeFileSync(checksumsPath, checksumManifest("1.2.3"));

  const { updateHomebrewFormula } = await import("../scripts/update-homebrew-formula.mjs");
  await updateHomebrewFormula({ version: "1.2.3", checksumsPath, formulaPath });

  const formula = fs.readFileSync(formulaPath, "utf8");
  assert.ok(formula.includes('version "1.2.3"'));
  assert.ok(formula.endsWith("end\n"));
});

test("the checked-in formula matches the generator and the released version", async () => {
  // .gitattributes pins the formula to LF, but normalize anyway so the test
  // does not depend on how a contributor's Git checked the file out.
  const formula = fs
    .readFileSync(path.join(root, "Formula/riftri.rb"), "utf8")
    .replace(/\r\n/g, "\n");
  const version = formula.match(/^  version "([^"]+)"$/m)?.[1];
  assert.ok(version, "formula must declare a version");

  // Regenerating from the formula's own checksums must reproduce it byte for
  // byte, so a hand edit or a partial bump fails here instead of at install.
  const checksums = new Map();
  const urls = formula.matchAll(/url "[^"]+\/(?<file>[^"/]+)"\s+sha256 "(?<sha>[0-9a-f]{64})"/g);
  for (const match of urls) {
    checksums.set(match.groups.file, match.groups.sha);
  }
  assert.equal(checksums.size, 4, "expected four pinned archives");

  const { renderHomebrewFormula } = await import("../scripts/update-homebrew-formula.mjs");
  assert.equal(renderHomebrewFormula({ version, checksums }), formula);
});
