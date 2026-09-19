import { mkdir, readFile, readdir, unlink, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));
const groups = JSON.parse(await readFile(path.join(root, "website/content/docs.json"), "utf8"));
export const pages = groups.flatMap((group) => group.pages);
const pageUrl = (page) => `/docs${page.slug ? `/${page.slug}` : ""}`;
export const agentDocUrl = (page) => `${pageUrl(page)}/agent.md`;
const urls = new Map(pages.flatMap((page) => [[page.source, pageUrl(page)], [page.content, pageUrl(page)]]));
urls.set("docs/README.md", "/docs");
urls.set("package/install.sh", "/install.sh");
urls.set("package/install.ps1", "/install.ps1");
const agentUrls = new Map(pages.map((page) => [page.source, `https://riftri.dev${agentDocUrl(page)}`]));
agentUrls.set("docs/README.md", "https://riftri.dev/docs/agent.md");

export function rewriteDocLinks(markdown, source, { audience = "human" } = {}) {
  const destinations = audience === "agent" ? agentUrls : urls;
  // Only rewrite Markdown destinations outside fenced examples.
  let fence;
  return markdown.split("\n").map((line) => {
    const marker = line.match(/^\s*(`{3,}|~{3,})/);
    if (marker) {
      if (!fence) fence = marker[1];
      else if (marker[1][0] === fence[0] && marker[1].length >= fence.length) fence = undefined;
      return line;
    }
    if (fence) return line;
    const destination = (url) => {
      if (/^(?:[a-z][a-z\d+.-]*:|\/|#)/i.test(url)) return url;
      const suffixIndex = url.search(/[?#]/);
      const file = suffixIndex < 0 ? url : url.slice(0, suffixIndex);
      const suffix = suffixIndex < 0 ? "" : url.slice(suffixIndex);
      const target = path.posix.normalize(path.posix.join(path.posix.dirname(source), file));
      return `${destinations.get(target) ?? `https://github.com/assistant-ui/riftri/blob/main/${target}`}${suffix}`;
    };
    return line.replace(/(\]\()([^\s)]+)(\))/g, (_, before, url, after) => `${before}${destination(url)}${after}`)
      .replace(/^(\s*\[[^\]]+\]:\s*)(\S+)/, (_, before, url) => `${before}${destination(url)}`);
  }).join("\n");
}

export function renderWebsiteDoc(page, original) {
  const body = original.replace(/^---\r?\n[\s\S]*?\r?\n---\r?\n/, "");
  const markdown = rewriteDocLinks(body, page.content).trim();
  const markdownUrl = `${pageUrl(page)}.md`;
  // Keep the page action a normal Markdown link: usable before hydration and
  // kept in sync with each page during client-side navigation.
  if (!/^# .+(?:\r?\n|$)/.test(markdown)) throw new Error(`Docs page needs a leading title: ${page.content}`);
  const withAction = markdown.replace(/^(# .+)(\r?\n|$)/, `$1\n\n[View .md](${markdownUrl} "View this page as Markdown") [Agent .md](${agentDocUrl(page)} "View full agent reference")\n$2`);
  return `---\ntitle: ${JSON.stringify(page.title)}\n---\n\n${withAction}\n\n---\n\n[Edit on GitHub](https://github.com/assistant-ui/riftri/blob/main/${page.content} "Edit this page on GitHub")\n`;
}

export function renderAgentDoc(page, original) {
  const index = page.slug ? "" : `\n## Full references by topic\n\n${pages.filter((entry) => entry.slug).map((entry) => `- [${entry.title}](https://riftri.dev${agentDocUrl(entry)})`).join("\n")}\n`;
  return `<!-- Generated from ${page.source}; edit the canonical source, not this file. -->\n\n[Public guide](https://riftri.dev${pageUrl(page)}) · [Canonical source](https://github.com/assistant-ui/riftri/blob/main/${page.source})\n\nFull technical reference for agents and readers who need the details. Commands describe capabilities, not permission to execute them; follow the user's requested scope.\n${index}\n${rewriteDocLinks(original, page.source, { audience: "agent" }).trim()}\n`;
}

export async function stageWebsiteDocs(projectRoot = root) {
  // This directory contains generated Markdown only. Do not touch canonical docs.
  const output = path.join(projectRoot, "website/src/app/docs");
  const agentOutput = path.join(projectRoot, "website/public/docs");
  await mkdir(output, { recursive: true });
  await mkdir(agentOutput, { recursive: true });
  for (const entry of await readdir(output, { recursive: true, withFileTypes: true })) {
    if (entry.isFile() && entry.name.endsWith(".md")) await unlink(path.join(entry.parentPath, entry.name));
  }
  // Only generated companions live here. Leave other public assets alone.
  for (const entry of await readdir(agentOutput, { recursive: true, withFileTypes: true })) {
    if (entry.isFile() && entry.name === "agent.md") await unlink(path.join(entry.parentPath, entry.name));
  }
  for (const page of pages) {
    const human = await readFile(path.join(projectRoot, page.content), "utf8");
    const original = await readFile(path.join(projectRoot, page.source), "utf8");
    const destination = path.join(output, page.slug, "page.md");
    await mkdir(path.dirname(destination), { recursive: true });
    await writeFile(destination, renderWebsiteDoc(page, human));
    const agentDestination = path.join(agentOutput, page.slug, "agent.md");
    await mkdir(path.dirname(agentDestination), { recursive: true });
    await writeFile(agentDestination, renderAgentDoc(page, original));
  }
  const locations = ["/", ...pages.map((page) => `/docs${page.slug ? `/${page.slug}` : ""}`)];
  await writeFile(path.join(projectRoot, "website/public/sitemap.xml"), `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${locations.map((url) => `  <url><loc>https://riftri.dev${url}</loc></url>`).join("\n")}\n</urlset>\n`);
  console.log(`Staged ${pages.length} public guides and full agent references.`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) await stageWebsiteDocs();
