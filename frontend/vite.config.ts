import { defineConfig, loadEnv } from "vite";
import solid from "vite-plugin-solid";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { randomBytes } from "node:crypto";
import { createGateway } from "./server/gateway.ts";
export default defineConfig(({ mode }) => {
  const env = { ...loadEnv(mode, process.cwd(), ""), ...process.env };
  const directory = env.SESSION_DIRECTORY || ".sessions";
  let sessionKey = env.SESSION_KEY;
  if (!sessionKey) {
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    const path = resolve(directory, "dev.key");
    try {
      sessionKey = readFileSync(path, "utf8").trim();
    } catch (e: any) {
      if (e.code !== "ENOENT") throw e;
      sessionKey = randomBytes(32).toString("base64url");
      writeFileSync(path, sessionKey, { mode: 0o600, flag: "wx" });
    }
  }
  const gateway = createGateway({
    origin: env.FRONTEND_ORIGIN || "http://127.0.0.1:4330",
    upstream:
      env.REMIND_UPSTREAM || "https://backend.remind.teamofsilicons.com",
    key: sessionKey,
    authOrigin: env.IAM_AUTH_ORIGIN,
    directory,
  });
  return {
    plugins: [
      solid(),
      {
        name: "remind-gateway",
        configureServer(server) {
          server.middlewares.use((req, res, next) => {
            if (!req.url?.startsWith("/ui/")) return next();
            void gateway(req, res);
          });
        },
      },
    ],
    build: { outDir: "dist/client" },
  };
});
