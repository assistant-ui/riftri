const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");
const root = path.resolve(__dirname, "../..");
const read = (file) => fs.readFileSync(path.join(root, file), "utf8");

test("quick start includes a copyable, review-first PowerShell installer", () => {
  const page = read("website/src/app/page.tsx");
  assert.ok(page.includes("Windows / PowerShell"));
  assert.ok(page.includes("<CopyCommand command={powershellInstall} compact multiline"));
  const command = page.match(/const powershellInstall = `([\s\S]*?)`;/)?.[1];
  assert.ok(command);
  assert.ok(read("docs/install.md").includes(command), "keep the command identical to the reviewed installation guide");
  assert.ok(!/ExecutionPolicy|Bypass|Invoke-Expression/.test(command));
  assert.ok(command.endsWith("Get-Content $Installer"), "copying the review command must not also execute the installer");
  assert.ok(page.includes('<CopyCommand command="& $Installer"'));
});

test("Windows requirements and PATH guidance are visible with the command", () => {
  const page = read("website/src/app/page.tsx");
  assert.ok(page.includes("ReFS volume, not ordinary NTFS"));
  assert.ok(page.includes("follow its printed PATH command"));
  assert.ok(read("website/src/app/globals.css").includes(".command-multiline code"));
});
