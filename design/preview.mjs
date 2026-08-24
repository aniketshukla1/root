// Renders built .dc.html artboards as a plain HTML page so the design can be
// looked at in a browser before it is seeded onto the canvas.
import { readFileSync, writeFileSync } from "node:fs";
const names = process.argv.slice(2);
const parts = names.map((n) => {
  const src = readFileSync(new URL(`./${n}.dc.html`, import.meta.url), "utf8");
  const style = /<helmet>\s*<style>([\s\S]*?)<\/style>\s*<\/helmet>/.exec(src)[1];
  const body = /<\/helmet>([\s\S]*?)<\/x-dc>/.exec(src)[1];
  return { n, style, body };
});
writeFileSync(new URL("./preview.html", import.meta.url), `<!doctype html><html><head><meta charset="utf-8"><title>root — preview</title><style>${parts[0].style}
body{padding:28px;display:flex;flex-direction:column;gap:34px;align-items:flex-start;background:#000}
.cap{font-family:var(--mono);font-size:11px;color:#5B6979;letter-spacing:.08em;text-transform:uppercase}
.frame{border:1px solid rgba(255,255,255,.12);border-radius:12px;overflow:hidden}
</style></head><body>${parts.map((p) => `<div><div class="cap">${p.n}</div><div class="frame" style="margin-top:10px">${p.body}</div></div>`).join("")}</body></html>`);
console.log("preview.html →", names.join(", "));
