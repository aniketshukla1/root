// node capture.mjs <outdir> <size> <t,t,...|frames:N> <0|1 light> [page]
// Each frame is the page's frame(t, light), written as a PNG with its alpha.
import { spawn } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const [out, size, when, light, page = 'mark.html'] = process.argv.slice(2);
// SUB samples per frame, spread over half a frame's time: a film camera's shutter, so the fast part of the turn blurs instead of stepping.
const SUB = +(process.env.SUB || 1);
const frames = when.startsWith('frames:') ? +when.slice(7) : 0;
const times = frames
  ? Array.from({ length: frames * SUB }, (_, i) => { const f = Math.floor(i / SUB); return (f + ((i % SUB) / SUB) * 0.5) / frames; })
  : when.split(',').map(Number);
mkdirSync(out, { recursive: true });
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const port = 9800 + Math.floor(Math.random() * 180);
const profile = mkdtempSync(join(tmpdir(), 'logo3d-'));
const chrome = spawn(CHROME, ['--headless=new', `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`, '--use-angle=metal', '--enable-gpu', '--ignore-gpu-blocklist', '--no-first-run', 'about:blank'], { stdio: 'ignore' });
// Chrome may still be writing its profile as it dies, and a leftover temp folder is not worth a crash.
process.on('exit', () => { chrome.kill('SIGKILL'); try { rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 }); } catch {} });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let target;
for (let i = 0; i < 80 && !target; i++) {
  await sleep(250);
  try { target = (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find((t) => t.type === 'page'); } catch {}
}
const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((r) => { ws.onopen = r; });
let seq = 0;
const pending = new Map();
ws.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id && pending.has(d.id)) { pending.get(d.id)(d.result ?? d); pending.delete(d.id); } };
const send = (method, params = {}) => new Promise((r) => { const id = ++seq; pending.set(id, r); ws.send(JSON.stringify({ id, method, params })); });
const ev = async (expression) => {
  const r = await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
  return r.result?.value;
};
await send('Page.enable');
await send('Runtime.enable');
await send('Page.navigate', { url: new URL(`./${page}?size=${size}`, import.meta.url).href });
await sleep(1500);
console.log('renderer:', await ev('window.renderer'));
for (const [i, t] of times.entries()) {
  const b64 = await ev(`window.frame(${t}, ${light})`);
  const name = String(frames ? Math.floor(i / SUB) : i).padStart(5, '0') + (SUB > 1 && frames ? `-${i % SUB}` : '');
  writeFileSync(join(out, `${name}.png`), Buffer.from(b64, 'base64'));
}
console.log(`${times.length} frames in ${out}`);
process.exit(0);
