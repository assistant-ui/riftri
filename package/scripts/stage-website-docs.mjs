import { mkdir, readFile, readdir, unlink, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));
const groups = JSON.parse(await readFile(path.join(root, "website/content/docs.json"), "utf8"));
export const pages = groups.flatMap((group) => group.pages);
const pageUrl = (page) => `/docs${page.slug ? `/${page.slug}` : ""}`;
// The page's own `.md` URL serves the full reference; there is no separate
// /agent.md route.
export const agentDocUrl = (page) => `${pageUrl(page)}.md`;
const urls = new Map(pages.flatMap((page) => [[page.source, pageUrl(page)], [page.content, pageUrl(page)]]));
urls.set("docs/README.md", "/docs");
urls.set("package/install.sh", "/install.sh");
urls.set("package/install.ps1", "/install.ps1");
const agentUrls = new Map(pages.map((page) => [page.source, `https://riftri.dev${agentDocUrl(page)}`]));
agentUrls.set("docs/README.md", "https://riftri.dev/docs.md");

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
  // The docs framework renders the native "Copy .md" action
  // (pageActions.copyMarkdown in docs.config.ts) — a real clipboard copy of
  // this page as Markdown, with framework frontmatter. Alongside it we inject a
  // plain "View .md" link so readers can open the Markdown directly; it sits
  // right under the title next to the framework's Copy button.
  if (!/^# .+(?:\r?\n|$)/.test(markdown)) throw new Error(`Docs page needs a leading title: ${page.content}`);
  const view = `[View .md](${pageUrl(page)}.md "View this page as Markdown")`;
  const withView = markdown.replace(/^(# .+(?:\r?\n|$))/, (title) => `${title}\n${view}\n`);
  return `---\ntitle: ${JSON.stringify(page.title)}\n---\n\n${withView}\n\n---\n\n[Edit on GitHub](https://github.com/assistant-ui/riftri/blob/main/${page.content} "Edit this page on GitHub")\n`;
}

export function renderAgentDoc(page, original) {
  const index = page.slug ? "" : `\n## Full references by topic\n\n${pages.filter((entry) => entry.slug).map((entry) => `- [${entry.title}](https://riftri.dev${agentDocUrl(entry)})`).join("\n")}\n`;
  // Front the full reference with YAML frontmatter so `/docs/<slug>.md` matches
  // the docs framework's Markdown semantics (title plus canonical/source URLs).
  const frontmatter = [
    "---",
    `title: ${JSON.stringify(page.title)}`,
    `canonical_url: ${JSON.stringify(`https://riftri.dev${pageUrl(page)}`)}`,
    `source_url: ${JSON.stringify(`https://github.com/assistant-ui/riftri/blob/main/${page.source}`)}`,
    "---",
  ].join("\n");
  return `${frontmatter}\n\n<!-- Generated from ${page.source}; edit the canonical source, not this file. -->\n\n[Public guide](https://riftri.dev${pageUrl(page)}) · [Canonical source](https://github.com/assistant-ui/riftri/blob/main/${page.source})\n\nFull technical reference for agents and readers who need the details. Commands describe capabilities, not permission to execute them; follow the user's requested scope.\n${index}\n${rewriteDocLinks(original, page.source, { audience: "agent" }).trim()}\n`;
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
  // Remove previously staged reference companions, including any left by a
  // page that was renamed or retired. They are the generated `.md` files
  // directly under public/docs plus public/docs.md; other public assets and
  // nested directories are left untouched.
  const publicDir = path.join(projectRoot, "website/public");
  const docsDir = path.join(agentOutput, "");
  for (const entry of await readdir(docsDir, { withFileTypes: true }).catch(() => [])) {
    if (entry.isFile() && entry.name.endsWith(".md")) await unlink(path.join(docsDir, entry.name));
  }
  await unlink(path.join(publicDir, "docs.md")).catch(() => {});
  for (const page of pages) {
    const human = await readFile(path.join(projectRoot, page.content), "utf8");
    const original = await readFile(path.join(projectRoot, page.source), "utf8");
    const destination = path.join(output, page.slug, "page.md");
    await mkdir(path.dirname(destination), { recursive: true });
    await writeFile(destination, renderWebsiteDoc(page, human));
    // The full reference is the page's own `.md` (e.g. /docs/cli.md); the
    // introduction (no slug) is /docs.md.
    const referenceDestination = path.join(publicDir, `${pageUrl(page).slice(1)}.md`);
    await mkdir(path.dirname(referenceDestination), { recursive: true });
    await writeFile(referenceDestination, renderAgentDoc(page, original));
  }
  const locations = ["/", ...pages.map((page) => `/docs${page.slug ? `/${page.slug}` : ""}`)];
  await writeFile(path.join(projectRoot, "website/public/sitemap.xml"), `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${locations.map((url) => `  <url><loc>https://riftri.dev${url}</loc></url>`).join("\n")}\n</urlset>\n`);
  console.log(`Staged ${pages.length} public guides and full agent references.`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) await stageWebsiteDocs();
