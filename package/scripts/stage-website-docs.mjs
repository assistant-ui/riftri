import { mkdir, readFile, readdir, unlink, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));
const groups = JSON.parse(await readFile(path.join(root, "website/content/docs.json"), "utf8"));
export const pages = groups.flatMap((group) => group.pages);
const urls = new Map(pages.map((page) => [page.source, `/docs${page.slug ? `/${page.slug}` : ""}`]));
urls.set("docs/README.md", "/docs");

export function rewriteDocLinks(markdown, source) {
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
      return `${urls.get(target) ?? `https://github.com/assistant-ui/riftri/blob/main/${target}`}${suffix}`;
    };
    return line.replace(/(\]\()([^\s)]+)(\))/g, (_, before, url, after) => `${before}${destination(url)}${after}`)
      .replace(/^(\s*\[[^\]]+\]:\s*)(\S+)/, (_, before, url) => `${before}${destination(url)}`);
  }).join("\n");
}

export function renderWebsiteDoc(page, original) {
  const body = original.replace(/^---\r?\n[\s\S]*?\r?\n---\r?\n/, "");
  const markdown = rewriteDocLinks(body, page.source).trim();
  const markdownUrl = `/docs${page.slug ? `/${page.slug}` : ""}.md`;
  // Keep the page action a normal Markdown link: usable before hydration and
  // kept in sync with each page during client-side navigation.
  if (!/^# .+(?:\r?\n|$)/.test(markdown)) throw new Error(`Docs page needs a leading title: ${page.source}`);
  const withAction = markdown.replace(/^(# .+)(\r?\n|$)/, `$1\n\n[View .md](${markdownUrl} "View this page as Markdown")\n$2`);
  return `---\ntitle: ${JSON.stringify(page.title)}\n---\n\n${withAction}\n\n---\n\n[Edit on GitHub](https://github.com/assistant-ui/riftri/blob/main/${page.source} "Edit this page on GitHub")\n`;
}

export async function stageWebsiteDocs() {
  // This directory contains generated Markdown only. Do not touch canonical docs.
  const output = path.join(root, "website/src/app/docs");
  await mkdir(output, { recursive: true });
  for (const entry of await readdir(output, { recursive: true, withFileTypes: true })) {
    if (entry.isFile() && entry.name.endsWith(".md")) await unlink(path.join(entry.parentPath, entry.name));
  }
  for (const page of pages) {
    const original = await readFile(path.join(root, page.source), "utf8");
    const destination = path.join(output, page.slug, "page.md");
    await mkdir(path.dirname(destination), { recursive: true });
    await writeFile(destination, renderWebsiteDoc(page, original));
  }
  const locations = ["/", ...pages.map((page) => `/docs${page.slug ? `/${page.slug}` : ""}`)];
  await writeFile(path.join(root, "website/public/sitemap.xml"), `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${locations.map((url) => `  <url><loc>https://riftri.dev${url}</loc></url>`).join("\n")}\n</urlset>\n`);
  console.log(`Staged ${pages.length} documentation pages from their canonical sources.`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) await stageWebsiteDocs();
