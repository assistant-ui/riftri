// Read-only source export; all mutations are confined to a new output directory.
// Usage: node benchmarks/config-batching.mjs BEFORE AFTER SOURCE COMMIT OUTPUT
// Run with ordinary Git on PATH, on a supported native-COW volume, without other
// builds/benchmarks running. OUTPUT must not already exist or be inside SOURCE.
// Use a UTF-8-path fixture whose Git archive and checkout have identical bytes.
// Node >= 20 required.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";

const [beforeArg, afterArg, sourceArg, commit, outputArg] = process.argv.slice(2);
assert.ok(outputArg && /^(?:[a-f0-9]{40}|[a-f0-9]{64})$/.test(commit), "expected BEFORE AFTER SOURCE COMMIT OUTPUT");
const requestedRoot = path.resolve(outputArg);
const root = path.join(fs.realpathSync(path.dirname(requestedRoot)), path.basename(requestedRoot));
const source = fs.realpathSync(sourceArg);
const relative = path.relative(source, root);
assert.ok(relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative), "OUTPUT must be outside SOURCE");
const binaries = { before: path.resolve(beforeArg), after: path.resolve(afterArg) };
for (const binary of Object.values(binaries)) assert.ok(fs.statSync(binary).isFile());
fs.mkdirSync(root); // Refuse existing output, including someone else's fixture.
const repo = path.join(root, "repository");
const env = { ...process.env };
for (const name of Object.keys(env)) {
  if (name.startsWith("GIT_") || name.startsWith("RIFTRI_")) delete env[name];
}
env.GIT_CONFIG_NOSYSTEM = "1";
env.GIT_CONFIG_GLOBAL = process.platform === "win32" ? "NUL" : "/dev/null";
function exec(command, args, cwd = repo, input) {
  const r = spawnSync(command, args, { cwd, env, input, encoding: input ? undefined : "utf8", maxBuffer: 100e6, timeout: 60000 });
  assert.equal(r.status, 0, `${command} ${args.join(" ")}\n${r.stderr}`);
  return r.stdout;
}
const archive = exec("git", ["archive", commit], source, Buffer.alloc(0));
fs.mkdirSync(repo);
exec("tar", ["-xf", "-", "-C", repo], root, archive);
exec("git", ["init", "--quiet"]);
for (const [key, value] of [["user.name", "Riftri Benchmark"], ["user.email", "benchmark@example.invalid"], ["core.autocrlf", "false"], ["core.hooksPath", process.platform === "win32" ? "NUL" : "/dev/null"], ["commit.gpgSign", "false"]]) {
  exec("git", ["config", key, value]);
}
exec("git", ["add", "--all"]);
exec("git", ["commit", "--quiet", "-m", "test: exact-tree benchmark fixture"]);
const tree = exec("git", ["rev-parse", "HEAD^{tree}"]).trim();
assert.equal(tree, exec("git", ["rev-parse", `${commit}^{tree}`], source).trim());
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const treeBytes = exec("git", ["ls-tree", "-rz", "HEAD"], repo, Buffer.alloc(0));
const treeText = new TextDecoder("utf-8", { fatal: true }).decode(treeBytes);
const manifest = treeText.split("\0").filter(Boolean).map(record => {
  const tab = record.indexOf("\t");
  const [mode, kind] = record.slice(0, tab).split(" ");
  const name = record.slice(tab + 1);
  assert.equal(kind, "blob", "benchmark fixture must not contain submodules");
  const file = path.join(repo, name);
  const bytes = mode === "120000" ? fs.readlinkSync(file, { encoding: "buffer" }) : fs.readFileSync(file);
  return { name, mode, bytes: bytes.length, sha256: sha256(bytes) };
});
exec(binaries.before, ["enable", repo]);
const report = {
  commit, tree, createdAt: new Date().toISOString(), platform: process.platform,
  git: exec("git", ["--version"]).trim(), node: process.version,
  files: manifest.length, logicalBytes: manifest.reduce((n, f) => n + f.bytes, 0),
  binaries: Object.fromEntries(Object.entries(binaries).map(([name, file]) => [name, { path: file, sha256: sha256(fs.readFileSync(file)) }])),
  cases: [], batches: [],
};
function save() { fs.writeFileSync(path.join(root, "results.json"), JSON.stringify(report, null, 2)); }
function assertClean(view) {
  assert.equal(exec("git", ["status", "--porcelain=v1", "-z", "--untracked-files=all"], view), "");
  exec("git", ["diff", "--exit-code", "HEAD"], view);
}
function verify(view) {
  assertClean(view);
  for (const entry of manifest) {
    const file = path.join(view, entry.name);
    const metadata = fs.lstatSync(file);
    assert.equal(metadata.isSymbolicLink(), entry.mode === "120000");
    const bytes = entry.mode === "120000" ? fs.readlinkSync(file, { encoding: "buffer" }) : fs.readFileSync(file);
    assert.equal(sha256(bytes), entry.sha256, entry.name);
    if (process.platform !== "win32" && entry.mode !== "120000") assert.equal(Boolean(metadata.mode & 0o100), entry.mode === "100755");
  }
}
function run(version, mode, label) {
  const destination = path.join(root, label);
  const args = mode === "doctor" ? ["doctor", repo, "--destination", destination, "--json"]
    : mode === "shim" ? ["exec", "--", "git", "worktree", "add", "--detach", destination, "HEAD"]
    : ["worktree", "add", "--detach", "--repository", repo, destination, "HEAD"];
  const trace = path.join(root, label + ".trace2.jsonl");
  assert.ok(!fs.existsSync(destination) && !fs.existsSync(trace));
  return new Promise((resolve, reject) => {
    const start = process.hrtime.bigint();
    const child = spawn(binaries[version], args, { cwd: repo, env: { ...env, GIT_TRACE2_EVENT: trace } });
    let stdout = "", stderr = "";
    child.stdout.on("data", chunk => { stdout += chunk; });
    child.stderr.on("data", chunk => { stderr += chunk; });
    child.on("error", reject);
    child.on("close", code => {
      const seconds = Number(process.hrtime.bigint() - start) / 1e9;
      fs.writeFileSync(path.join(root, label + ".log"), stdout + stderr);
      if (code !== 0) return reject(new Error(`${label} failed: ${stderr}`));
      const events = fs.readFileSync(trace, "utf8").trim().split("\n").map(JSON.parse);
      const starts = events.filter(e => e.event === "start");
      const result = { version, mode, label, destination, seconds, gitProcesses: starts.length,
        topLevelGitProcesses: starts.filter(e => !e.sid.includes("/")).length,
        settingReads: starts.filter(e => e.argv.includes("config") && (e.argv.includes("--get") || e.argv.includes("--get-regexp"))).map(e => e.argv) };
      report.cases.push(result); save(); resolve(result);
    });
  });
}
function remove(result) {
  verify(result.destination);
  exec(binaries.after, ["worktree", "remove", "--repository", repo, result.destination]);
  result.verifiedAndRemoved = true; save();
}
function order(round) { return round % 2 ? ["before", "after"] : ["after", "before"]; }
// Alternate order to reduce systematic warm-cache/order bias.
for (let round = 1; round <= 10; round++) {
  for (const version of order(round)) await run(version, "doctor", `doctor-${round}-${version}`);
}
for (let round = 1; round <= 3; round++) {
  for (const version of order(round)) {
    exec(binaries.after, ["gc", repo, "--apply"]);
    const result = await run(version, "explicit", `cold-${round}-${version}`);
    console.log(result.label, result.seconds.toFixed(3)); remove(result);
  }
}
for (let round = 1; round <= 6; round++) {
  for (const mode of ["explicit", "shim"]) {
    for (const version of order(round)) {
      const result = await run(version, mode, `warm-${mode}-${round}-${version}`);
      console.log(result.label, result.seconds.toFixed(3)); remove(result);
    }
  }
}
for (let round = 1; round <= 3; round++) {
  for (const version of order(round)) {
    const start = process.hrtime.bigint();
    const results = await Promise.all(Array.from({ length: 9 }, (_, i) => run(version, "shim", `parallel-${round}-${version}-${i + 1}`)));
    const seconds = Number(process.hrtime.bigint() - start) / 1e9;
    report.batches.push({ round, version, seconds }); save();
    console.log(`parallel-${round}-${version}`, seconds.toFixed(3));
    // Exercise a same-size private edit while sibling worktrees still exist.
    const sample = manifest.find(f => f.mode === "100644" && f.bytes > 0 && !f.name.startsWith("."));
    assert.ok(sample);
    const file = path.join(results[0].destination, sample.name);
    const original = fs.readFileSync(file);
    const edited = Buffer.from(original); edited[0] ^= 1;
    fs.writeFileSync(file, edited);
    assert.notEqual(exec("git", ["status", "--porcelain=v1", "-z"], results[0].destination), "");
    for (const sibling of results.slice(1)) assert.equal(sha256(fs.readFileSync(path.join(sibling.destination, sample.name))), sample.sha256);
    fs.writeFileSync(file, original);
    for (const result of results) remove(result);
  }
}
exec(binaries.after, ["gc", repo, "--apply"]);
const status = exec(binaries.after, ["status", repo]);
for (const text of ["Active views: 0\n", "Retained bases: 0\n", "State issues: 0\n"]) assert.ok(status.includes(text));
fs.writeFileSync(path.join(root, "final-status.log"), status);
verify(repo);
assert.equal(exec("git", ["worktree", "list", "--porcelain"]).split("\n").filter(line => line.startsWith("worktree ")).length, 1);
report.completedAt = new Date().toISOString(); save();
console.log("COMPLETE", path.join(root, "results.json"));
// Keep the standalone source fixture and logs for inspection; never delete
// original source files or auto-remove a failed/dirty experimental worktree.
