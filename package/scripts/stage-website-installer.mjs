import { copyFile, mkdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));

export async function stageWebsiteInstaller(destination = path.join(root, "website/public")) {
  await mkdir(destination, { recursive: true });
  await copyFile(path.join(root, "package/install.sh"), path.join(destination, "install.sh"));
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  await stageWebsiteInstaller();
}
