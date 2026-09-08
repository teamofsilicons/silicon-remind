import { build } from "esbuild";
await build({
  entryPoints: ["server/node.ts"],
  bundle: true,
  platform: "node",
  target: "node24",
  format: "esm",
  outfile: "dist/server/node.js",
});
