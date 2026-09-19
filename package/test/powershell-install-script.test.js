const assert = require("node:assert/strict");
const { createHash } = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { test } = require("node:test");

const root = path.resolve(__dirname, "../..");
const installer = path.join(root, "package/install.ps1");
const packageVersion = JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8")).version;

test("PowerShell installer keeps installation explicit and profile-free", () => {
  const source = fs.readFileSync(installer, "utf8");
  assert.match(source, /riftri-win32-\$Architecture-v\$Version\.tar\.gz/);
  assert.match(source, /Security\.Cryptography\.SHA256/);
  assert.match(source, /ComputeHash/);
  assert.match(source, /riftri\.exe/);
  assert.match(source, /File\]::Replace/);
  assert.match(source, /IsPathRooted/);
  assert.match(
    source,
    /Invoke-WebRequest -Uri \$Uri -OutFile \$Destination -UseBasicParsing -PassThru/,
    "-OutFile downloads must pass -PassThru so Assert-HttpsResponse receives a response",
  );
  assert.match(
    source,
    /Could not verify the final download URI/,
    "Assert-HttpsResponse must fail closed when no final URI is observable",
  );
  assert.ok(source.includes("Write-Output 'Installer: https://riftri.dev/install.ps1'"));
  assert.doesNotMatch(source, /Microsoft\.PowerShell_profile|CurrentUserAllHosts|SetEnvironmentVariable|\$PROFILE|winget|choco/i);
});

test("PowerShell installer is syntactically valid", { skip: process.platform !== "win32" }, () => {
  const result = spawnSync("powershell.exe", [
    "-NoLogo",
    "-NoProfile",
    "-NonInteractive",
    "-Command",
    `$errors = $null; [void][System.Management.Automation.Language.Parser]::ParseFile('${installer.replaceAll("'", "''")}', [ref]$null, [ref]$errors); if ($errors.Count) { $errors | Out-String | Write-Error; exit 1 }`,
  ], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
});

function windowsFixture(t, { corrupt = false, downgrade = false } = {}) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "riftri-powershell-installer-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const files = path.join(directory, "files");
  const installDir = path.join(directory, "install dir");
  fs.mkdirSync(files);
  fs.mkdirSync(installDir);
  const binary = path.join(root, "target/debug/riftri.exe");
  assert.ok(fs.existsSync(binary), "npm test must build the Windows binary first");
  fs.copyFileSync(binary, path.join(files, "riftri.exe"));
  const archiveName = `riftri-win32-x64-v${packageVersion}.tar.gz`;
  const archive = path.join(directory, archiveName);
  const packed = spawnSync("tar.exe", ["-czf", archive, "-C", files, "riftri.exe"], { encoding: "utf8" });
  assert.equal(packed.status, 0, packed.stderr);
  const hash = createHash("sha256").update(fs.readFileSync(archive)).digest("hex");
  fs.writeFileSync(path.join(directory, "SHA256SUMS"), `${corrupt ? "0".repeat(64) : hash}  ${archiveName}\n`);
  const requests = path.join(directory, "requests.txt");
  const harness = path.join(directory, "harness.ps1");
  fs.writeFileSync(harness, `
function global:Invoke-WebRequest {
  param([string]$Uri, [string]$OutFile, [switch]$UseBasicParsing, [switch]$PassThru)
  Add-Content -LiteralPath $env:TEST_REQUESTS -Value $Uri
  if ($Uri -notlike 'https://github.com/assistant-ui/riftri/releases/download/*') { throw 'unexpected URL' }
  $source = if ($Uri.EndsWith('/SHA256SUMS')) { Join-Path $env:TEST_FIXTURE 'SHA256SUMS' } else { Join-Path $env:TEST_FIXTURE ([IO.Path]::GetFileName($Uri)) }
  Copy-Item -LiteralPath $source -Destination $OutFile
  # The real cmdlet writes the file and returns nothing when -OutFile is used
  # without -PassThru; only -PassThru puts a response on the pipeline.
  if (-not $PassThru) { return }
  $finalUri = if ($env:TEST_DOWNGRADE -eq '1') { [Uri]($Uri -replace '^https:', 'http:') } else { [Uri]$Uri }
  [pscustomobject]@{ BaseResponse = [pscustomobject]@{ ResponseUri = $finalUri } }
}
& $env:TEST_INSTALLER -Version $env:TEST_VERSION
exit $LASTEXITCODE
`);
  return {
    directory,
    installDir,
    binary,
    requests,
    run() {
      return spawnSync("powershell.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", harness], {
        encoding: "utf8",
        env: {
          ...process.env,
          PROCESSOR_ARCHITECTURE: "AMD64",
          RIFTRI_INSTALL_DIR: installDir,
          TEST_DOWNGRADE: downgrade ? "1" : "0",
          TEST_FIXTURE: directory,
          TEST_INSTALLER: installer,
          TEST_REQUESTS: requests,
          TEST_VERSION: packageVersion,
        },
      });
    },
  };
}

test("PowerShell installer installs a checksum-verified Windows binary", { skip: process.platform !== "win32" }, (t) => {
  const fixture = windowsFixture(t);
  fs.copyFileSync(fixture.binary, path.join(fixture.installDir, "riftri.exe"));
  const result = fixture.run();
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, new RegExp(`Installed Riftri v${packageVersion.replaceAll(".", "\\.")}`));
  const installed = path.join(fixture.installDir, "riftri.exe");
  assert.ok(fs.statSync(installed).isFile());
  const version = spawnSync(installed, ["--version"], { encoding: "utf8" });
  assert.equal(version.status, 0, version.stderr);
  assert.equal(version.stdout.trim(), `riftri ${packageVersion}`);
  assert.equal(fs.readFileSync(fixture.requests, "utf8").trim().split(/\r?\n/).length, 2);
  assert.deepEqual(fs.readdirSync(fixture.installDir), ["riftri.exe"]);
});

test("PowerShell installer refuses a download whose final URI is not HTTPS", { skip: process.platform !== "win32" }, (t) => {
  const fixture = windowsFixture(t, { downgrade: true });
  const target = path.join(fixture.installDir, "riftri.exe");
  fs.writeFileSync(target, "existing installation");
  const result = fixture.run();
  assert.notEqual(result.status, 0, "the installer must refuse an http final URI; success means the downgrade guard was skipped");
  assert.match(result.stderr, /redirected away from HTTPS/);
  assert.equal(fs.readFileSync(target, "utf8"), "existing installation");
  assert.deepEqual(fs.readdirSync(fixture.installDir), ["riftri.exe"]);
});

test("PowerShell installer preserves an existing target after checksum failure", { skip: process.platform !== "win32" }, (t) => {
  const fixture = windowsFixture(t, { corrupt: true });
  const target = path.join(fixture.installDir, "riftri.exe");
  fs.writeFileSync(target, "existing installation");
  const result = fixture.run();
  assert.notEqual(result.status, 0, result.stdout);
  assert.match(result.stderr, /checksum/i);
  assert.equal(fs.readFileSync(target, "utf8"), "existing installation");
  assert.deepEqual(fs.readdirSync(fixture.installDir), ["riftri.exe"]);
});
