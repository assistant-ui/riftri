// Fail a GNU/Linux build that requires a newer glibc than Riftri supports.
//
// The installer picks the GNU archive for any glibc host without checking its
// version, so a binary built against a newer glibc installs cleanly and then
// refuses to start. v0.4.0 shipped that way: built on Ubuntu 24.04, it needed
// GLIBC_2.39 and could not run on Ubuntu 22.04, Debian 12, or RHEL 9.
//
//   node package/scripts/check-glibc-floor.mjs <binary> [maxVersion]
//
// Reads the ELF version requirements directly. readelf is not installed on
// every runner and is absent from macOS entirely.

import { readFile } from "node:fs/promises";

/** Highest glibc version Riftri's GNU artifacts may require. */
export const GLIBC_FLOOR = "2.34";

const compare = (a, b) => {
  const [aMajor, aMinor] = a.split(".").map(Number);
  const [bMajor, bMinor] = b.split(".").map(Number);
  return aMajor - bMajor || aMinor - bMinor;
};

/**
 * Every `GLIBC_x.y` version this ELF binary requires, with the symbols that
 * ask for each. Returns null when the file is not ELF.
 */
export function glibcRequirements(buffer) {
  if (buffer.length < 64 || buffer.readUInt32BE(0) !== 0x7f454c46) return null;
  if (buffer[4] !== 2) throw new Error("only 64-bit ELF is supported");
  const little = buffer[5] === 1;
  if (!little) throw new Error("only little-endian ELF is supported");

  const u16 = (o) => buffer.readUInt16LE(o);
  const u32 = (o) => buffer.readUInt32LE(o);
  const u64 = (o) => Number(buffer.readBigUInt64LE(o));

  const sectionOffset = u64(0x28);
  const sectionSize = u16(0x3a);
  const sectionCount = u16(0x3c);
  const nameIndex = u16(0x3e);

  const sections = [];
  for (let i = 0; i < sectionCount; i += 1) {
    const o = sectionOffset + i * sectionSize;
    sections.push({
      name: u32(o),
      offset: u64(o + 0x18),
      size: u64(o + 0x20),
      info: u32(o + 0x2c),
      entrySize: u64(o + 0x38),
    });
  }

  const stringTable = sections[nameIndex];
  const stringAt = (base, offset) => {
    const start = base + offset;
    const end = buffer.indexOf(0, start);
    return buffer.toString("utf8", start, end === -1 ? undefined : end);
  };
  const named = new Map(
    sections.map((s) => [stringAt(stringTable.offset, s.name), s]),
  );

  const dynstr = named.get(".dynstr");
  const verneed = named.get(".gnu.version_r");
  const versym = named.get(".gnu.version");
  const dynsym = named.get(".dynsym");
  // A static binary (musl) has no dynamic glibc requirement at all.
  if (!dynstr || !verneed || !versym || !dynsym) return new Map();

  // .gnu.version_r maps a version index to its name, e.g. 4 -> "GLIBC_2.39".
  const versions = new Map();
  let entry = verneed.offset;
  for (let i = 0; i < verneed.info; i += 1) {
    const auxCount = u16(entry + 2);
    let aux = entry + u32(entry + 8);
    for (let j = 0; j < auxCount; j += 1) {
      versions.set(u16(aux + 6), stringAt(dynstr.offset, u32(aux + 8)));
      const next = u32(aux + 12);
      if (!next) break;
      aux += next;
    }
    const next = u32(entry + 12);
    if (!next) break;
    entry += next;
  }

  const required = new Map();
  const symbolCount = Math.floor(dynsym.size / dynsym.entrySize);
  for (let i = 0; i < symbolCount; i += 1) {
    const version = versions.get(u16(versym.offset + i * 2) & 0x7fff);
    const match = version?.match(/^GLIBC_(\d+\.\d+)$/);
    if (!match) continue;
    const symbol = stringAt(dynstr.offset, u32(dynsym.offset + i * dynsym.entrySize));
    if (!required.has(match[1])) required.set(match[1], new Set());
    required.get(match[1]).add(symbol);
  }
  return required;
}

export async function checkGlibcFloor(binary, max = GLIBC_FLOOR) {
  const required = glibcRequirements(await readFile(binary));
  if (required === null) throw new Error(`${binary} is not an ELF binary`);
  const versions = [...required.keys()].sort(compare);
  const over = versions.filter((v) => compare(v, max) > 0);
  if (over.length > 0) {
    const detail = over
      .map((v) => `  GLIBC_${v}: ${[...required.get(v)].sort().join(", ")}`)
      .join("\n");
    throw new Error(
      `${binary} requires glibc newer than ${max}, so it cannot start on the ` +
        `oldest supported distributions:\n${detail}\n` +
        `Build this target on a host whose glibc is ${max}, or raise the ` +
        `documented floor in docs/install.md and GLIBC_FLOOR together.`,
    );
  }
  return versions;
}

if (process.argv[1] && import.meta.url.endsWith(process.argv[1].replace(/^.*?(?=\/)/, ""))) {
  const [, , binary, max] = process.argv;
  if (!binary) {
    process.stderr.write("usage: check-glibc-floor.mjs <binary> [maxVersion]\n");
    process.exit(2);
  }
  try {
    const versions = await checkGlibcFloor(binary, max || GLIBC_FLOOR);
    process.stdout.write(
      `${binary}: highest required glibc is ${versions.at(-1) ?? "none"} ` +
        `(floor ${max || GLIBC_FLOOR})\n`,
    );
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exit(1);
  }
}
