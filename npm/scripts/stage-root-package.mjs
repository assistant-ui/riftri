import { cp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(scriptDirectory, "..", "..");

export async function stageRootPackage(
  destination = path.join(repositoryRoot, "dist", "npm-root"),
) {
  const manifest = JSON.parse(
    await readFile(path.join(repositoryRoot, "package.json"), "utf8"),
  );
  for (const packageName of Object.keys(manifest.optionalDependencies)) {
    manifest.optionalDependencies[packageName] = manifest.version;
  }
  delete manifest.scripts;

  await rm(destination, { recursive: true, force: true });
  await mkdir(path.join(destination, "npm"), { recursive: true });
  await Promise.all([
    cp(
      path.join(repositoryRoot, "npm", "bin"),
      path.join(destination, "npm", "bin"),
      { recursive: true },
    ),
    cp(
      path.join(repositoryRoot, "npm", "lib"),
      path.join(destination, "npm", "lib"),
      { recursive: true },
    ),
    cp(path.join(repositoryRoot, "README.md"), path.join(destination, "README.md")),
    cp(path.join(repositoryRoot, "LICENSE"), path.join(destination, "LICENSE")),
    writeFile(
      path.join(destination, "package.json"),
      `${JSON.stringify(manifest, null, 2)}\n`,
    ),
  ]);

  return destination;
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const destination = await stageRootPackage();
  process.stdout.write(`staged public npm launcher at ${destination}\n`);
}
