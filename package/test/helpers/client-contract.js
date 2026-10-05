const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const ts = require("typescript");
const { Riftri } = require("../../lib/client.js");

const root = path.resolve(__dirname, "../../..");

function fixture(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-contract-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const repository = path.join(directory, "repository");
  const stateDir = path.join(directory, "state");
  fs.mkdirSync(stateDir);
  const git = spawnSync("git", ["init", "--quiet", repository], { encoding: "utf8" });
  assert.equal(git.status, 0, git.stderr);
  const binary = path.join(root, "target", "debug", process.platform === "win32" ? "riftri.exe" : "riftri");
  return { directory, repository, stateDir, client: new Riftri({ repository, stateDir, binary }) };
}

function assertTypes(directory, source) {
  const file = path.join(directory, "contract.ts");
  const declaration = path.join(root, "package/lib/client").replaceAll("\\", "/");
  fs.writeFileSync(file, `import { Riftri, StatusReport } from ${JSON.stringify(declaration)};\n${source}`);
  const program = ts.createProgram([file], {
    noEmit: true,
    strict: true,
    target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.CommonJS,
    moduleResolution: ts.ModuleResolutionKind.Node10,
    typeRoots: [path.join(root, "node_modules/@types")],
    types: ["node"],
  });
  const diagnostics = ts.getPreEmitDiagnostics(program);
  assert.equal(diagnostics.length, 0, ts.formatDiagnosticsWithColorAndContext(diagnostics, {
    getCanonicalFileName: (name) => name,
    getCurrentDirectory: () => root,
    getNewLine: () => "\n",
  }));
}

module.exports = { fixture, assertTypes };
