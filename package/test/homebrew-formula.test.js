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

test("updating writes every formula copy to disk from a checksum manifest", async (t) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-formula-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const checksumsPath = path.join(directory, "SHA256SUMS");
  const targets = [
    path.join(directory, "riftri.rb"),
    // A missing parent directory must be created, as for a fresh staged copy.
    path.join(directory, "staged/Formula/riftri.rb"),
  ];
  fs.writeFileSync(checksumsPath, checksumManifest("1.2.3"));

  const { updateHomebrewFormula } = await import("../scripts/update-homebrew-formula.mjs");
  await updateHomebrewFormula({ version: "1.2.3", checksumsPath, targets });

  const copies = targets.map((target) => fs.readFileSync(target, "utf8"));
  assert.equal(copies[0], copies[1]);
  assert.ok(copies[0].includes('version "1.2.3"'));
  assert.ok(copies[0].endsWith("end\n"));
});

test("the staged tap formula is byte-identical to the canonical formula", () => {
  const canonical = fs.readFileSync(path.join(root, "Formula/riftri.rb"), "utf8");
  const staged = fs.readFileSync(path.join(root, "package/homebrew/Formula/riftri.rb"), "utf8");
  assert.equal(
    staged,
    canonical,
    "package/homebrew/Formula/riftri.rb must match Formula/riftri.rb; run node package/scripts/sync-homebrew-tap.mjs",
  );
});

test("the generator's default targets cover the canonical and staged copies", async () => {
  const { formulaPaths } = await import("../scripts/update-homebrew-formula.mjs");
  assert.deepEqual(
    formulaPaths.map((target) => path.relative(root, target).split(path.sep).join("/")),
    ["Formula/riftri.rb", "package/homebrew/Formula/riftri.rb"],
  );
});

test("tap sync normalizes versions and rejects non-release input", async () => {
  const { normalizeVersion } = await import("../scripts/sync-homebrew-tap.mjs");
  assert.equal(normalizeVersion("v1.2.3"), "1.2.3");
  assert.equal(normalizeVersion("1.2.3"), "1.2.3");
  assert.equal(normalizeVersion("v1.2.3-rc.1"), "1.2.3-rc.1");
  for (const bad of ["v1.2", "main", "v1.2.3.4", ""]) {
    assert.throws(() => normalizeVersion(bad), /expected a release version/);
  }
});

test("tap sync detects drift in check mode and repairs it when writing", async (t) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-tap-sync-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const targets = [
    path.join(directory, "Formula/riftri.rb"),
    path.join(directory, "staged/Formula/riftri.rb"),
  ];
  const fetchImpl = async (url) => {
    assert.match(url, /\/releases\/download\/v9\.9\.9\/SHA256SUMS$/);
    return { ok: true, text: async () => checksumManifest("9.9.9") };
  };

  const { syncHomebrewTap } = await import("../scripts/sync-homebrew-tap.mjs");

  // Both copies are missing, so a check must report both as drifted.
  const checked = await syncHomebrewTap({ version: "9.9.9", check: true, targets, fetchImpl });
  assert.deepEqual(checked.drifted, targets);
  assert.ok(!fs.existsSync(targets[0]), "check mode must not write");

  // Writing repairs both copies and a follow-up check is clean.
  await syncHomebrewTap({ version: "9.9.9", targets, fetchImpl });
  const copies = targets.map((target) => fs.readFileSync(target, "utf8"));
  assert.equal(copies[0], copies[1]);
  assert.ok(copies[0].includes('version "9.9.9"'));
  const clean = await syncHomebrewTap({ version: "9.9.9", check: true, targets, fetchImpl });
  assert.deepEqual(clean.drifted, []);
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
