import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import {
  access,
  chmod,
  copyFile,
  cp,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { stageRootPackage } from "./stage-root-package.mjs";

const require = createRequire(import.meta.url);
const { binaryName, packageNameForPlatform } = require("../lib/platform.js");
const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const defaultRepositoryRoot = path.resolve(scriptDirectory, "..", "..");

function run(command, arguments_, options = {}) {
  const result = spawnSync(command, arguments_, {
    cwd: options.cwd,
    env: options.env ?? process.env,
    encoding: "utf8",
    shell: options.shell ?? false,
    windowsHide: false,
  });
  if (result.error) {
    throw new Error(`could not run ${command}: ${result.error.message}`);
  }
  if (result.status !== 0) {
    throw new Error(
      [
        `${command} ${arguments_.join(" ")} exited with ${result.status}`,
        result.stdout.trim(),
        result.stderr.trim(),
      ]
        .filter(Boolean)
        .join("\n"),
    );
  }
  return result.stdout;
}

function runNpm(arguments_, options = {}) {
  if (process.env.npm_execpath) {
    return run(process.execPath, [process.env.npm_execpath, ...arguments_], options);
  }
  return run("npm", arguments_, {
    ...options,
    shell: process.platform === "win32",
  });
}

function pack(packageDirectory, tarballsDirectory, repositoryRoot) {
  const output = runNpm(
    [
      "pack",
      packageDirectory,
      "--pack-destination",
      tarballsDirectory,
      "--ignore-scripts",
      "--json",
    ],
    { cwd: repositoryRoot },
  );
  const records = JSON.parse(output);
  if (!Array.isArray(records) || records.length !== 1 || !records[0].filename) {
    throw new Error(`npm pack returned unexpected output: ${output}`);
  }
  return path.join(tarballsDirectory, records[0].filename);
}

function runInstalled(launcher, arguments_, cwd, environment) {
  return run(process.execPath, [launcher, ...arguments_], {
    cwd,
    env: environment,
  });
}

function runGit(repository, arguments_) {
  return run("git", arguments_, { cwd: repository });
}

async function exerciseApfsLifecycle(root, launcher, environment) {
  const repository = path.join(root, "repository");
  const view = path.join(root, "packaged-view");
  await mkdir(path.join(repository, "src"), { recursive: true });
  await writeFile(path.join(repository, "README.md"), "# Package smoke fixture\n");
  await writeFile(path.join(repository, "src", "app.txt"), "source\n");
  runGit(repository, ["init", "--quiet"]);
  runGit(repository, ["config", "user.name", "Riftri Package Smoke"]);
  runGit(repository, ["config", "user.email", "riftri@example.invalid"]);
  runGit(repository, ["config", "core.autocrlf", "false"]);
  runGit(repository, ["add", "--", "README.md", "src/app.txt"]);
  runGit(repository, ["commit", "--quiet", "-m", "initial fixture"]);

  runInstalled(launcher, ["enable"], repository, environment);
  runInstalled(
    launcher,
    [
      "exec",
      "--",
      "git",
      "worktree",
      "add",
      "-b",
      "smoke/packaged-install",
      view,
      "HEAD",
    ],
    repository,
    environment,
  );

  if (runGit(view, ["status", "--porcelain=v1"]).trim() !== "") {
    throw new Error("the packaged CLI created a dirty worktree");
  }
  await writeFile(path.join(view, "src", "app.txt"), "packaged view change\n");
  if (
    (await readFile(path.join(repository, "src", "app.txt"), "utf8")) !==
    "source\n"
  ) {
    throw new Error("a packaged-view write changed the source worktree");
  }
  runGit(view, ["add", "--", "src/app.txt"]);
  runGit(view, ["commit", "--quiet", "-m", "package smoke change"]);
  runInstalled(
    launcher,
    ["exec", "--", "git", "worktree", "remove", view],
    repository,
    environment,
  );
  await access(view).then(
    () => {
      throw new Error("managed worktree still exists after packaged removal");
    },
    () => {},
  );

  const beforeCollection = runInstalled(
    launcher,
    ["status"],
    repository,
    environment,
  );
  if (
    !beforeCollection.includes("Active views: 0") ||
    !beforeCollection.includes("Retained bases: 1")
  ) {
    throw new Error(`unexpected status after packaged removal:\n${beforeCollection}`);
  }
  runInstalled(launcher, ["gc", "--apply"], repository, environment);
  const afterCollection = runInstalled(
    launcher,
    ["status"],
    repository,
    environment,
  );
  if (
    !afterCollection.includes("Active views: 0") ||
    !afterCollection.includes("Retained bases: 0")
  ) {
    throw new Error(`unexpected status after packaged collection:\n${afterCollection}`);
  }
  if (runGit(repository, ["status", "--porcelain=v1"]).trim() !== "") {
    throw new Error("the source worktree became dirty during the packaged lifecycle");
  }
  runInstalled(launcher, ["disable"], repository, environment);
}

export async function smokeInstalledPackage(options = {}) {
  const repositoryRoot = path.resolve(
    options.repositoryRoot ?? defaultRepositoryRoot,
  );
  const temporary = await mkdtemp(
    path.join(os.tmpdir(), "riftri-installed-smoke-"),
  );
  try {
    const packageName = packageNameForPlatform(
      process.platform,
      process.arch,
      process.report,
    );
    if (!packageName) {
      throw new Error(
        `no packaged Riftri binary is defined for ${process.platform}/${process.arch}`,
      );
    }

    const executable = binaryName(process.platform);
    const sourceBinary = path.join(repositoryRoot, "target", "debug", executable);
    await access(sourceBinary);
    const platformStage = path.join(temporary, "platform", packageName);
    await cp(
      path.join(repositoryRoot, "package", "platforms", packageName),
      platformStage,
      { recursive: true },
    );
    const platformBinary = path.join(platformStage, "bin", executable);
    await mkdir(path.dirname(platformBinary), { recursive: true });
    await copyFile(sourceBinary, platformBinary);
    if (process.platform !== "win32") {
      await chmod(platformBinary, 0o755);
    }

    const rootStage = path.join(temporary, "npm-root");
    await stageRootPackage(rootStage);
    const tarballs = path.join(temporary, "tarballs");
    await mkdir(tarballs);
    const platformTarball = pack(platformStage, tarballs, repositoryRoot);
    const rootTarball = pack(rootStage, tarballs, repositoryRoot);

    const consumer = path.join(temporary, "consumer");
    await mkdir(consumer);
    await writeFile(
      path.join(consumer, "package.json"),
      `${JSON.stringify(
        {
          private: true,
          dependencies: {
            riftri: `file:${rootTarball}`,
            [packageName]: `file:${platformTarball}`,
          },
        },
        null,
        2,
      )}\n`,
    );
    runNpm(
      [
        "install",
        "--ignore-scripts",
        "--no-audit",
        "--no-fund",
        "--offline",
        "--omit=optional",
      ],
      { cwd: consumer },
    );

    const launcher = path.join(
      consumer,
      "node_modules",
      "riftri",
      "bin",
      "riftri.js",
    );
    await access(launcher);
    const environment = { ...process.env };
    for (const key of [
      "RIFTRI_BINARY",
      "RIFTRI_REAL_GIT",
      "RIFTRI_SHIM_ACTIVE",
      "RIFTRI_BYPASS",
    ]) {
      delete environment[key];
    }
    const version = runInstalled(launcher, ["--version"], consumer, environment);
    const gitVersion = runInstalled(
      launcher,
      ["exec", "--", "git", "--version"],
      consumer,
      environment,
    );
    if (!gitVersion.startsWith("git version ")) {
      throw new Error(`packaged process-scoped Git failed: ${gitVersion}`);
    }

    const lifecycleTested = process.platform === "darwin";
    if (lifecycleTested) {
      await exerciseApfsLifecycle(temporary, launcher, environment);
    }
    return { lifecycleTested, version };
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const result = await smokeInstalledPackage();
  const scope = result.lifecycleTested ? "APFS lifecycle" : "launcher only";
  process.stdout.write(
    `installed-package smoke passed (${scope})\n`,
  );
}
