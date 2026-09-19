import { createServer } from "node:http";
import { readFile, realpath } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// Preview the actual finalized build, including its public file response headers.
const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".png": "image/png", ".woff2": "font/woff2", ".json": "application/json", ".md": "text/plain", ".sh": "text/plain", ".ps1": "text/plain", ".txt": "text/plain", ".xml": "application/xml" };

export async function createStaticPreview(output = fileURLToPath(new URL("../.vercel/output/", import.meta.url))) {
  const root = await realpath(path.join(output, "static"));
  const config = JSON.parse(await readFile(path.join(output, "config.json"), "utf8"));
  if (config.version !== 3) throw new Error("Expected Build Output API version 3");
  const aliases = new Map(Object.entries(config.overrides || {})
    .filter(([, value]) => typeof value.path === "string")
    .map(([file, value]) => [`/${value.path}`, file]));
  const routes = config.routes || [];
  const errorIndex = routes.findIndex((route) => route.handle === "error");
  const errorRoutes = errorIndex < 0 ? [] : routes.slice(errorIndex + 1);
  const docsRoute = routes.find((route) => route.dest === "/__nitro");
  const docsHandler = docsRoute
    ? (await import(pathToFileURL(path.join(output, "functions/__nitro.func/index.mjs")).href)).default
    : undefined;

  async function resolveFile(pathname) {
    const file = await realpath(path.join(root, aliases.get(pathname) ?? (pathname === "/" ? "index.html" : pathname)));
    const relative = path.relative(root, file);
    if (relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) {
      throw Object.assign(new Error("Outside static root"), { code: "FORBIDDEN" });
    }
    return { file, bytes: await readFile(file) };
  }

  return createServer(async (request, response) => {
    if (!["GET", "HEAD"].includes(request.method)) {
      response.writeHead(405, { Allow: "GET, HEAD" }).end();
      return;
    }
    try {
      const pathname = decodeURIComponent(new URL(request.url, "http://localhost").pathname);
      if (docsHandler && new RegExp(docsRoute.src).test(pathname)) {
        const rendered = await docsHandler.fetch(new Request(new URL(request.url, `http://${request.headers.host}`), { method: request.method, headers: request.headers }));
        const headers = new Headers(rendered.headers);
        // Match the build-output header rules for raw docs as well as static files.
        if (rendered.ok) for (const route of routes) {
          if (route.handle) break;
          if (route.src && route.headers && new RegExp(route.src).test(pathname)) {
            for (const [name, value] of Object.entries(route.headers)) headers.set(name, value);
          }
        }
        for (const [name, value] of Object.entries(docsRoute.headers || {})) headers.set(name, value);
        response.writeHead(rendered.status, Object.fromEntries(headers));
        response.end(request.method === "HEAD" ? undefined : Buffer.from(await rendered.arrayBuffer()));
        return;
      }
      let result;
      let status = 200;
      try {
        result = await resolveFile(pathname);
      } catch (error) {
        if (!["ENOENT", "ENOTDIR", "EISDIR"].includes(error.code)) throw error;
        const route = errorRoutes.find((entry) => entry.status === 404 && entry.src && entry.dest && new RegExp(entry.src).test(pathname));
        if (!route) throw error;
        result = await resolveFile(route.dest);
        status = 404;
      }
      const headers = { "Content-Type": types[path.extname(result.file)] || "application/octet-stream" };
      for (const route of routes) {
        if (route.handle) break;
        if (route.src && route.headers && new RegExp(route.src).test(pathname)) Object.assign(headers, route.headers);
      }
      // A missing installer/Markdown URL must not relabel the error page as text.
      if (status === 404) headers["Content-Type"] = "text/html; charset=utf-8";
      response.writeHead(status, { ...headers, "Content-Length": result.bytes.length });
      response.end(request.method === "HEAD" ? undefined : result.bytes);
    } catch (error) {
      const status = error.code === "FORBIDDEN" ? 403 : error instanceof URIError ? 400 : ["ENOENT", "ENOTDIR", "EISDIR"].includes(error.code) ? 404 : 500;
      response.writeHead(status, { "Content-Type": "text/plain" }).end(request.method === "HEAD" ? undefined : status === 404 ? "Not found" : "Request failed");
    }
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const port = Number(process.env.RIFTRI_PREVIEW_PORT || 4318);
  const server = await createStaticPreview();
  server.listen(port, "127.0.0.1", () => console.log(`Riftri preview: http://127.0.0.1:${server.address().port}`));
}
