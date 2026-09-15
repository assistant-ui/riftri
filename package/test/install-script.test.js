const assert = require("node:assert/strict");
const { createHash } = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
const installer = path.join(root, "package/install.sh");
const unix = process.platform !== "win32";
const version = "0.1.1";
const release = "https://github.com/assistant-ui/riftri/releases";

// No network or native binaries: exercise the real Bash script with a release
// fixture and a harmless executable. Actual release execution is a separate smoke check.
function fixture(t, options = {}) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-installer-test-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const bin = path.join(directory, "tools");
  const home = path.join(directory, "home");
  const scratch = path.join(directory, "scratch");
  const files = path.join(directory, "files");
  for (const item of [bin, home, scratch, files]) fs.mkdirSync(item);
  const installDir = path.join(home, ".local/bin");
  fs.mkdirSync(installDir, { recursive: true });
  const target = path.join(installDir, "riftri");
  fs.writeFileSync(target, "previous install\n", { mode: 0o755 });
  for (const name of [".bashrc", ".bash_profile", ".zshrc", ".profile"]) {
    fs.writeFileSync(path.join(home, name), "# leave this unchanged\n");
  }
  const executable = path.join(files, "riftri");
  fs.writeFileSync(executable, `#!/bin/sh\nprintf 'riftri ${options.binaryVersion || version}\\n'\nexit ${options.binaryExit || 0}\n`, { mode: 0o755 });
  if (options.symlinkArchive) {
    fs.renameSync(executable, path.join(files, "other"));
    fs.symlinkSync("other", executable);
  }
  const archive = path.join(directory, "download.tar.gz");
  const entries = ["riftri"];
  if (options.extraEntry) {
    fs.writeFileSync(path.join(files, "extra"), "unexpected");
    entries.push("extra");
  }
  const tar = spawnSync("tar", ["-czf", archive, "-C", files, ...entries]);
  assert.equal(tar.status, 0, tar.stderr.toString());
  const platform = options.platform || "darwin-arm64";
  const asset = `riftri-${platform}-v${version}.tar.gz`;
  const hash = createHash("sha256").update(fs.readFileSync(archive)).digest("hex");
  const checksum = `${options.corrupt ? "0".repeat(64) : hash}  ${asset}\n`;
  fs.writeFileSync(path.join(directory, "SHA256SUMS"), options.missingChecksum ? `${hash}  other.tar.gz\n` : checksum.repeat(options.duplicateChecksum ? 2 : 1));
  const mock = `#!${process.execPath}
const fs = require('node:fs');
const path = require('node:path');
const args = process.argv.slice(2);
const tool = path.basename(process.argv[1]);
if (tool === 'uname') {
  process.stdout.write(args[0] === '-s' ? process.env.TEST_OS : process.env.TEST_ARCH);
} else if (tool === 'getconf') {
  if (process.env.TEST_MUSL) process.exit(1);
  process.stdout.write('glibc 2.31');
} else {
  fs.appendFileSync(process.env.TEST_REQUESTS, JSON.stringify(args) + '\\n');
  if (!args.includes('--fail') || !args.includes('--location') || !args.includes('--proto') || !args.includes('--proto-redir') || !args.includes('=https')) process.exit(90);
  if (process.env.TEST_NETWORK_FAIL) process.exit(22);
  const url = args.find(a => a.startsWith('https://'));
  if (url === ${JSON.stringify(`${release}/latest`)}) {
    process.stdout.write(process.env.TEST_LATEST);
  } else {
    const filename = url === ${JSON.stringify(`${release}/download/v${version}/SHA256SUMS`)} ? 'SHA256SUMS' : url === ${JSON.stringify(`${release}/download/v${version}/${asset}`)} ? 'download.tar.gz' : null;
    if (!filename) process.exit(91);
    fs.copyFileSync(path.join(process.env.TEST_FIXTURE, filename), args[args.indexOf('--output') + 1]);
  }
}
`;
  for (const name of ["curl", "uname", "getconf"]) fs.writeFileSync(path.join(bin, name), mock, { mode: 0o755 });
  const requests = path.join(directory, "requests");
  const env = {
    ...process.env,
    TMPDIR: scratch,
    PATH: `${bin}:/usr/bin:/bin`,
    RIFTRI_INSTALL_DIR: installDir,
    TEST_FIXTURE: directory,
    TEST_REQUESTS: requests,
    TEST_OS: options.os || "Darwin",
    TEST_ARCH: options.arch || "arm64",
    TEST_LATEST: options.latest || `${release}/tag/v${version}`,
    TEST_MUSL: options.musl ? "1" : "",
    TEST_NETWORK_FAIL: options.networkFail ? "1" : "",
  };
  return {
    directory, home, scratch, installDir, target, env,
    run(args = [], overrides = {}) {
      return spawnSync("/bin/bash", ["-s", "--", ...args], {
        input: fs.readFileSync(installer), env: { ...env, ...overrides }, encoding: "utf8", timeout: 10000,
      });
    },
    requests() { return fs.existsSync(requests) ? fs.readFileSync(requests, "utf8") : ""; },
    unchanged() {
      assert.equal(fs.readFileSync(target, "utf8"), "previous install\n");
      assert.deepEqual(fs.readdirSync(scratch), []);
      assert.deepEqual(fs.readdirSync(installDir), ["riftri"]);
    },
  };
}

test("Bash installer is syntactically valid", { skip: !unix }, () => {
  const result = spawnSync("/bin/bash", ["-n", installer], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
});

for (const [system, arch, platform, musl = false] of [
  ["Darwin", "arm64", "darwin-arm64"],
  ["Darwin", "x86_64", "darwin-x64"],
  ["Linux", "aarch64", "linux-arm64-gnu"],
  ["Linux", "x86_64", "linux-x64-gnu"],
  ["Linux", "aarch64", "linux-arm64-musl", true],
  ["Linux", "x86_64", "linux-x64-musl", true],
]) {
  test(`piped installer selects ${platform}, pins latest, and leaves profiles alone`, { skip: !unix }, (t) => {
    const f = fixture(t, { os: system, arch, platform, musl });
    const result = f.run();
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /Installed Riftri v0\.1\.1/);
    assert.match(result.stdout, /PATH/);
    assert.match(fs.readFileSync(f.target, "utf8"), /riftri 0\.1\.1/);
    assert.equal(fs.statSync(f.target).mode & 0o777, 0o755);
    assert.equal(f.requests().trim().split("\n").length, 3);
    assert.match(f.requests(), new RegExp(platform));
    assert.deepEqual(fs.readdirSync(f.scratch), []);
    assert.deepEqual(fs.readdirSync(f.installDir), ["riftri"]);
    for (const name of [".bashrc", ".bash_profile", ".zshrc", ".profile"]) {
      assert.equal(fs.readFileSync(path.join(f.home, name), "utf8"), "# leave this unchanged\n");
    }
  });
}

test("accepts a pinned version and a new install directory containing spaces", { skip: !unix }, (t) => {
  const f = fixture(t);
  const installDir = path.join(f.directory, "custom install", "bin");
  const result = f.run(["v0.1.1"], { RIFTRI_INSTALL_DIR: installDir });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(f.requests().trim().split("\n").length, 2);
  assert.match(fs.readFileSync(path.join(installDir, "riftri"), "utf8"), /riftri 0\.1\.1/);
  f.unchanged();
});

test("defaults to the per-user directory without changing shell startup files", () => {
  const source = fs.readFileSync(installer, "utf8");
  assert.match(source, /install_dir=\$\{RIFTRI_INSTALL_DIR:-\$\{HOME:\?HOME is required\}\/\.local\/bin\}/);
  assert.doesNotMatch(source, /\.bashrc|\.bash_profile|\.zshrc|\.profile|^\s*(sudo|xattr|spctl)\s/m);
});

for (const [label, options, message] of [
  ["corrupt download", { corrupt: true }, /checksum/i],
  ["duplicate checksum", { duplicateChecksum: true }, /checksum/i],
  ["missing checksum", { missingChecksum: true }, /checksum/i],
  ["unexpected archive entry", { extraEntry: true }, /archive/i],
  ["symlink archive entry", { symlinkArchive: true }, /regular file/i],
  ["wrong binary version", { binaryVersion: "0.0.0" }, /version/i],
  ["binary cannot run", { binaryExit: 1 }, /run/i],
  ["failed download", { networkFail: true }, /download|release/i],
  ["unsupported OS", { os: "MINGW64_NT" }, /unsupported|Windows/i],
  ["unsupported architecture", { arch: "riscv64" }, /architecture/i],
  ["unexpected latest redirect", { latest: "https://example.org/tag/v0.1.1" }, /release/i],
  ["invalid latest tag", { latest: `${release}/tag/v0.1.1/../../oops` }, /version/i],
]) {
  test(`refuses ${label} without replacing the installation`, { skip: !unix }, (t) => {
    const f = fixture(t, options);
    const result = f.run();
    assert.notEqual(result.status, 0, result.stdout);
    assert.match(result.stderr, message);
    f.unchanged();
  });
}

test("help and invalid arguments do not download or install", { skip: !unix }, (t) => {
  const f = fixture(t);
  const help = f.run(["--help"]);
  assert.equal(help.status, 0);
  assert.ok(help.stdout.includes("https://riftri.dev/install.sh"));
  for (const args of [[""], ["--unknown"], ["0.1.1", "extra"], ["../../oops"], ["v0.1.1; echo bad"]]) {
    assert.notEqual(f.run(args).status, 0);
  }
  assert.equal(f.requests(), "");
  f.unchanged();
});

test("refuses relative install directories and existing symlink targets", { skip: !unix }, (t) => {
  const f = fixture(t);
  assert.notEqual(f.run([], { RIFTRI_INSTALL_DIR: "relative/bin" }).status, 0);
  const original = path.join(f.installDir, "original");
  fs.renameSync(f.target, original);
  fs.symlinkSync(original, f.target);
  const result = f.run();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /symlink|regular file/i);
  assert.equal(fs.readFileSync(original, "utf8"), "previous install\n");
  assert.ok(fs.lstatSync(f.target).isSymbolicLink());
  assert.equal(f.requests(), "");
});

for (const command of ["cp", "mv"]) {
  test(`failed ${command} keeps the previous binary and removes the staged file`, { skip: !unix }, (t) => {
    const f = fixture(t);
    fs.writeFileSync(path.join(f.directory, "tools", command), "#!/bin/sh\nexit 1\n", { mode: 0o755 });
    const result = f.run([version]);
    assert.notEqual(result.status, 0);
    f.unchanged();
  });
}

test("user tar options cannot alter extraction", { skip: !unix }, (t) => {
  const f = fixture(t);
  const result = f.run([version], { TAR_OPTIONS: "--strip-components=1" });
  assert.equal(result.status, 0, result.stderr);
  assert.match(fs.readFileSync(f.target, "utf8"), /riftri 0\.1\.1/);
});

test("truncated piped scripts cannot begin installation", { skip: !unix }, (t) => {
  const f = fixture(t);
  const source = fs.readFileSync(installer, "utf8");
  for (const end of [Math.floor(source.length / 2), source.lastIndexOf('main "$@"')]) {
    spawnSync("/bin/bash", ["-s"], { input: source.slice(0, end), env: f.env });
    assert.equal(f.requests(), "");
    f.unchanged();
  }
});

test("directory targets and pinned download failures preserve existing data", { skip: !unix }, (t) => {
  const f = fixture(t);
  const failedDownload = f.run([version], { TEST_NETWORK_FAIL: "1" });
  assert.notEqual(failedDownload.status, 0);
  assert.match(failedDownload.stderr, /download/i);
  f.unchanged();
  fs.unlinkSync(f.target);
  fs.mkdirSync(f.target);
  fs.writeFileSync(path.join(f.target, "keep"), "user data");
  const result = f.run([version]);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /regular file/i);
  assert.equal(fs.readFileSync(path.join(f.target, "keep"), "utf8"), "user data");
});
