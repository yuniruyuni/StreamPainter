import { copyFile, mkdir, readdir, rm } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const packageDir = join(dirname(fileURLToPath(import.meta.url)), "..");
const sourceDir = join(packageDir, "src");
const outputDir = join(packageDir, "static");

await mkdir(outputDir, { recursive: true });
for (const entry of await readdir(outputDir)) {
  if (entry !== ".gitkeep") {
    await rm(join(outputDir, entry), { recursive: true, force: true });
  }
}

await Promise.all([
  copyFile(join(sourceDir, "index.html"), join(outputDir, "index.html")),
  copyFile(join(sourceDir, "index.css"), join(outputDir, "index.css")),
]);

const result = await Bun.build({
  entrypoints: [join(sourceDir, "index.tsx")],
  outdir: outputDir,
  naming: "index.js",
  target: "browser",
  minify: true,
  sourcemap: "none",
  define: {
    "process.env.NODE_ENV": JSON.stringify("production"),
  },
});

if (!result.success) {
  for (const log of result.logs) console.error(log);
  process.exit(1);
}
