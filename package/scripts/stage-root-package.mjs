import { cp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const defaultRepositoryRoot = path.resolve(scriptDirectory, "..", "..");

export async function stageRootPackage(
  destination = path.join(defaultRepositoryRoot, "dist", "npm-root"),
  repositoryRoot = defaultRepositoryRoot,
) {
  const manifest = JSON.parse(
    await readFile(path.join(repositoryRoot, "package.json"), "utf8"),
  );
  for (const packageName of Object.keys(manifest.optionalDependencies)) {
    manifest.optionalDependencies[packageName] = manifest.version;
  }
  manifest.bin.riftri = "bin/riftri.js";
  // The tarball places bin/ and lib/ at its root, so every published entry
  // point drops the repository's `package/` prefix. The programmatic API is
  // resolved through these, so a stale prefix would publish a package whose
  // `require("riftri")` cannot be resolved.
  const published = (entry) => entry.replace(/^package\//, "");
  manifest.main = published(manifest.main);
  manifest.types = published(manifest.types);
  // Rewrite every subpath, not just ".". Rebuilding the object from one key
  // would publish a package missing the others, and nothing downstream looks:
  // npm resolves a subpath only when something imports it.
  const publishedEntry = (entry) =>
    `./${published(entry.replace(/^\.\//, ""))}`;
  manifest.exports = Object.fromEntries(
    Object.entries(manifest.exports).map(([subpath, target]) => [
      subpath,
      typeof target === "string"
        ? publishedEntry(target)
        : Object.fromEntries(
            Object.entries(target).map(([condition, entry]) => [
              condition,
              publishedEntry(entry),
            ]),
          ),
    ]),
  );
  manifest.files = ["bin", "lib", "README.md", "LICENSE"];
  delete manifest.scripts;

  await rm(destination, { recursive: true, force: true });
  await mkdir(destination, { recursive: true });
  await Promise.all([
    cp(
      path.join(repositoryRoot, "package", "bin"),
      path.join(destination, "bin"),
      { recursive: true },
    ),
    cp(
      path.join(repositoryRoot, "package", "lib"),
      path.join(destination, "lib"),
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

// `process.argv[1]` is undefined under `node -e` and in embedders, where
// pathToFileURL throws. The sibling release scripts all guard it.
if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href
) {
  const destination = await stageRootPackage();
  process.stdout.write(`staged public npm launcher at ${destination}\n`);
}
