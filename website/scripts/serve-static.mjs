import { createServer } from "node:http";
import { readFile, realpath } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

// Preview the actual finalized build, including its public file response headers.
const output = fileURLToPath(new URL("../.vercel/output/", import.meta.url));
const root = await realpath(path.join(output, "static"));
const config = JSON.parse(await readFile(path.join(output, "config.json"), "utf8"));
const port = Number(process.env.RIFTRI_PREVIEW_PORT || 4318);
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".png": "image/png", ".woff2": "font/woff2", ".json": "application/json", ".md": "text/plain", ".sh": "text/plain", ".ps1": "text/plain" };

const server = createServer(async (request, response) => {
  if (!["GET", "HEAD"].includes(request.method)) {
    response.writeHead(405, { Allow: "GET, HEAD" }).end();
    return;
  }
  try {
    const pathname = decodeURIComponent(new URL(request.url, "http://localhost").pathname);
    const file = await realpath(path.join(root, pathname === "/" ? "index.html" : pathname));
    const relative = path.relative(root, file);
    if (relative.startsWith("..") || path.isAbsolute(relative)) {
      response.writeHead(403).end();
      return;
    }
    const bytes = await readFile(file);
    const headers = { "Content-Type": types[path.extname(file)] || "application/octet-stream" };
    for (const route of config.routes || []) {
      if (route.src && route.headers && new RegExp(route.src).test(pathname)) Object.assign(headers, route.headers);
    }
    response.writeHead(200, { ...headers, "Content-Length": bytes.length });
    response.end(request.method === "HEAD" ? undefined : bytes);
  } catch {
    response.writeHead(404, { "Content-Type": "text/plain" }).end("Not found");
  }
});

server.listen(port, "127.0.0.1", () => console.log(`Riftri preview: http://127.0.0.1:${port}`));
