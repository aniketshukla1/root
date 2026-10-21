// Run after `npm run build`: node --experimental-strip-types scripts/check-budget.ts
//
// What the app costs to load, gzipped as it travels, against perf-budget.json.
// A budget nobody checks is a wish; this fails the build that breaks it, and
// says by how much, so raising the number is a decision rather than a drift.
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { gzipSync } from "node:zlib";

const budget = JSON.parse(readFileSync("perf-budget.json", "utf8")) as { jsGzipKb: number; cssGzipKb: number };
const dir = "dist/assets";
const kb = (ext: string) =>
  readdirSync(dir)
    .filter((f) => f.endsWith(ext))
    .reduce((sum, f) => sum + gzipSync(readFileSync(join(dir, f)), { level: 9 }).length, 0) / 1024;

let failed = false;
for (const [label, size, limit] of [
  ["JavaScript", kb(".js"), budget.jsGzipKb],
  ["CSS", kb(".css"), budget.cssGzipKb],
] as const) {
  const over = size > limit;
  failed ||= over;
  console.log(`${label}: ${size.toFixed(1)} KB gzipped of ${limit} KB${over ? ` — OVER by ${(size - limit).toFixed(1)} KB` : ` (${(limit - size).toFixed(1)} KB to spare)`}`);
}
if (failed) {
  console.error("budget: over — trim the bundle, or raise perf-budget.json on purpose");
  process.exit(1);
}
console.log("budget: ok");
