import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { resolve, extname } from "node:path";
import { createGateway } from "./gateway";
const origin = process.env.FRONTEND_ORIGIN;
if (!origin || !process.env.SESSION_KEY)
  throw Error("Set FRONTEND_ORIGIN and SESSION_KEY before starting");
const gateway = createGateway({
  origin,
  upstream:
    process.env.REMIND_UPSTREAM || "https://backend.remind.teamofsilicons.com",
  key: process.env.SESSION_KEY,
  authOrigin: process.env.IAM_AUTH_ORIGIN,
  directory: process.env.SESSION_DIRECTORY || ".sessions",
});
const root = resolve("dist/client");
const types: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "application/javascript",
  ".css": "text/css",
  ".svg": "image/svg+xml",
  ".woff2": "font/woff2",
};
createServer(async (req, res) => {
  try {
    res.setHeader("X-Content-Type-Options", "nosniff");
    res.setHeader("Referrer-Policy", "same-origin");
    res.setHeader(
      "Content-Security-Policy",
      "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
    );
    if (await gateway(req, res)) return;
    if (!["GET", "HEAD"].includes(req.method || "")) {
      res.writeHead(405);
      res.end();
      return;
    }
    const path = decodeURIComponent(new URL(req.url || "/", origin).pathname);
    const file = resolve(root, "." + path);
    if (!file.startsWith(root + "/")) {
      if (path !== "/") throw Error("Not found");
    }
    let bytes: Buffer;
    let ext = extname(file);
    try {
      bytes = await readFile(file);
    } catch {
      if (ext) {
        res.writeHead(404);
        res.end();
        return;
      }
      bytes = await readFile(resolve(root, "index.html"));
      ext = ".html";
    }
    res.setHeader("Content-Type", types[ext] || "application/octet-stream");
    res.setHeader(
      "Cache-Control",
      path.startsWith("/assets/")
        ? "public, max-age=31536000, immutable"
        : "no-cache",
    );
    res.end(req.method === "HEAD" ? undefined : bytes);
  } catch {
    res.writeHead(500);
    res.end("Unable to serve request");
  }
}).listen(
  Number(process.env.PORT || 4330),
  process.env.HOST || "127.0.0.1",
  () => console.log("Remind frontend listening at " + origin),
);
