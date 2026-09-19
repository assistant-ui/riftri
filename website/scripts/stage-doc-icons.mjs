import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import * as lucide from "lucide-react";

// Farm serializes its docs config. Generate SVG strings ahead of the build so
// neither React's server renderer nor the full icon library enters the client.
const groups = JSON.parse(await readFile(new URL("../content/docs.json", import.meta.url), "utf8"));
const names = [...new Set(groups.flatMap((group) => group.pages.map((page) => page.icon)))].sort();
const icons = Object.fromEntries(names.map((name) => {
  if (!name || !lucide[name]) throw new Error(`Unknown docs icon: ${name}`);
  return [name, renderToStaticMarkup(createElement(lucide[name]))];
}));
const output = new URL("../content/docs-icons.json", import.meta.url);
const json = `${JSON.stringify(icons, null, 2)}\n`;
if (process.argv.includes("--check")) {
  if (await readFile(output, "utf8") !== json) throw new Error("Docs icons are stale. Run pnpm stage.");
} else {
  await writeFile(output, json);
}

// CSS masks keep the page actions decorative and the Markdown links readable
// without HTML, JavaScript, or a client-side icon dependency.
const actionDir = new URL("../public/docs-icons/", import.meta.url);
if (!process.argv.includes("--check")) await mkdir(actionDir, { recursive: true });
for (const [file, Icon] of [["file-text", lucide.FileText], ["square-pen", lucide.SquarePen]]) {
  const svg = `${renderToStaticMarkup(createElement(Icon))}\n`;
  const destination = new URL(`${file}.svg`, actionDir);
  if (process.argv.includes("--check")) {
    if (await readFile(destination, "utf8") !== svg) throw new Error(`Stale docs action icon: ${file}`);
  } else {
    await writeFile(destination, svg);
  }
}
