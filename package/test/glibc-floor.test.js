"use strict";

const assert = require("node:assert/strict");
const { readFile } = require("node:fs/promises");
const path = require("node:path");
const { test } = require("node:test");

const root = path.resolve(__dirname, "..", "..");

/**
 * A minimal 64-bit ELF carrying one versioned glibc requirement, so the parser
 * is tested against bytes rather than against whatever happens to be built.
 */
function elfRequiring(version, symbol) {
  const strings = Buffer.concat([
    Buffer.from([0]),
    Buffer.from(`${symbol}\0`),
    Buffer.from(`GLIBC_${version}\0`),
    Buffer.from(".dynstr\0.gnu.version_r\0.gnu.version\0.dynsym\0.shstrtab\0"),
  ]);
  const symbolOffset = 1;
  const versionOffset = 1 + symbol.length + 1;
  // Offsets are computed, not counted: an off-by-one here silently produces a
  // section the parser cannot find, which looks like a parser bug.
  const sectionNames = versionOffset + `GLIBC_${version}`.length + 1;
  const names = {};
  let cursor = sectionNames;
  for (const [key, label] of [
    ["dynstr", ".dynstr"],
    ["verneed", ".gnu.version_r"],
    ["versym", ".gnu.version"],
    ["dynsym", ".dynsym"],
    ["shstrtab", ".shstrtab"],
  ]) {
    names[key] = cursor;
    cursor += label.length + 1;
  }

  // One Elf64_Verneed with one Elf64_Vernaux, declaring version index 2.
  const verneed = Buffer.alloc(32);
  verneed.writeUInt16LE(1, 0); // version
  verneed.writeUInt16LE(1, 2); // count
  verneed.writeUInt32LE(0, 4); // file
  verneed.writeUInt32LE(16, 8); // aux offset
  verneed.writeUInt32LE(0, 12); // next
  verneed.writeUInt32LE(0, 16); // hash
  verneed.writeUInt16LE(0, 20); // flags
  verneed.writeUInt16LE(2, 22); // other: the version index
  verneed.writeUInt32LE(versionOffset, 24); // name
  verneed.writeUInt32LE(0, 28); // next aux

  // Two symbols: index 0 is the reserved null entry.
  const versym = Buffer.alloc(4);
  versym.writeUInt16LE(0, 0);
  versym.writeUInt16LE(2, 2);
  const dynsym = Buffer.alloc(48);
  dynsym.writeUInt32LE(symbolOffset, 24); // second entry's name

  const header = Buffer.alloc(64);
  const body = Buffer.concat([strings, verneed, versym, dynsym]);
  const stringsAt = 64;
  const verneedAt = stringsAt + strings.length;
  const versymAt = verneedAt + verneed.length;
  const dynsymAt = versymAt + versym.length;
  const sectionsAt = 64 + body.length;

  header.writeUInt32BE(0x7f454c46, 0);
  header[4] = 2; // 64-bit
  header[5] = 1; // little-endian
  header.writeBigUInt64LE(BigInt(sectionsAt), 0x28);
  header.writeUInt16LE(64, 0x3a); // section entry size
  header.writeUInt16LE(5, 0x3c); // section count
  header.writeUInt16LE(4, 0x3e); // shstrtab index

  const section = (name, offset, size, info, entrySize) => {
    const s = Buffer.alloc(64);
    s.writeUInt32LE(name, 0);
    s.writeBigUInt64LE(BigInt(offset), 0x18);
    s.writeBigUInt64LE(BigInt(size), 0x20);
    s.writeUInt32LE(info, 0x2c);
    s.writeBigUInt64LE(BigInt(entrySize), 0x38);
    return s;
  };

  return Buffer.concat([
    header,
    body,
    section(names.dynstr, stringsAt, strings.length, 0, 0),
    section(names.verneed, verneedAt, verneed.length, 1, 0),
    section(names.versym, versymAt, versym.length, 0, 2),
    section(names.dynsym, dynsymAt, dynsym.length, 0, 24),
    section(names.shstrtab, stringsAt, strings.length, 0, 0),
  ]);
}

test("the floor check reads glibc requirements out of an ELF binary", async () => {
  const { glibcRequirements } = await import("../scripts/check-glibc-floor.mjs");
  const required = glibcRequirements(elfRequiring("2.39", "pidfd_spawnp"));
  assert.deepEqual([...required.keys()], ["2.39"]);
  assert.deepEqual([...required.get("2.39")], ["pidfd_spawnp"]);
});

test("a binary above the floor is rejected and names the symbols", async () => {
  const { checkGlibcFloor, GLIBC_FLOOR } = await import("../scripts/check-glibc-floor.mjs");
  const { mkdtemp, writeFile } = require("node:fs/promises");
  const os = require("node:os");
  const directory = await mkdtemp(path.join(os.tmpdir(), "riftri-elf-"));
  const binary = path.join(directory, "riftri");
  // This is the exact requirement that made the v0.4.0 GNU build unloadable
  // on RHEL 9, Debian 12, and Ubuntu 22.04.
  await writeFile(binary, elfRequiring("2.39", "pidfd_spawnp"));
  await assert.rejects(() => checkGlibcFloor(binary, GLIBC_FLOOR), /GLIBC_2\.39: pidfd_spawnp/);

  await writeFile(binary, elfRequiring("2.17", "memcpy"));
  assert.deepEqual(await checkGlibcFloor(binary, GLIBC_FLOOR), ["2.17"]);
});

test("the floor is one number shared by the checker, installer, and docs", async () => {
  const { GLIBC_FLOOR } = await import("../scripts/check-glibc-floor.mjs");
  const installer = await readFile(path.join(root, "package/install.sh"), "utf8");
  assert.match(
    installer,
    new RegExp(`glibc_floor=${GLIBC_FLOOR.replace(".", "\\.")}\\b`),
    "install.sh must fall back to musl using the same floor",
  );
  for (const doc of ["docs/install.md", "docs/troubleshooting.md"]) {
    const text = await readFile(path.join(root, doc), "utf8");
    assert.match(text, new RegExp(`glibc ${GLIBC_FLOOR.replace(".", "\\.")}`), `${doc} must state the floor`);
  }
});

test("GNU release artifacts are built on the floor and verified", async () => {
  const workflow = await readFile(path.join(root, ".github/workflows/release.yml"), "utf8");
  // Building GNU targets on a newer image is the bug: std links whatever that
  // image provides, which is how v0.4.0 came to need GLIBC_2.39.
  for (const target of ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]) {
    const entry = workflow.slice(workflow.indexOf(`target: ${target}`) - 200, workflow.indexOf(`target: ${target}`));
    assert.match(entry, /os: ubuntu-22\.04(-arm)?/, `${target} must build on the floor image`);
  }
  assert.doesNotMatch(
    workflow,
    /os: ubuntu-24\.04(-arm)?\n\s+target: \w+-unknown-linux-gnu/,
    "no GNU target may build on a newer glibc image",
  );
  assert.match(workflow, /check-glibc-floor\.mjs/, "the release must verify the floor");
  assert.match(workflow, /Run on the oldest supported glibc/, "the release must run the binary on the floor");
});
