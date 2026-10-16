// Run: node --experimental-strip-types scripts/check-dashboards.ts
import assert from "node:assert/strict";

import { exportJson, movePanel, parseImport, resizePanel, resolveProfile, shortLabels } from "../src/dashboards.ts";

// A Grafana export as the API returns it: a wrapper, a collapsed row with a
// panel inside, interval variables, a text panel and a table root skips.
const grafana = JSON.stringify({
  dashboard: {
    title: "Node exporter",
    panels: [
      { type: "stat", title: "Uptime", gridPos: { x: 0, y: 0, w: 6, h: 4 }, fieldConfig: { defaults: { unit: "s" } },
        targets: [{ expr: "node_time_seconds - node_boot_time_seconds" }] },
      { type: "timeseries", title: "CPU", gridPos: { x: 6, y: 0, w: 18, h: 8 },
        targets: [{ expr: "sum(rate(node_cpu_seconds_total{mode!=\"idle\"}[$__rate_interval]))" }] },
      { type: "text", title: "Notes", gridPos: { x: 0, y: 8, w: 24, h: 3 } },
      { type: "row", title: "Disk", collapsed: true, gridPos: { x: 0, y: 11, w: 24, h: 1 },
        panels: [{ type: "graph", title: "IO", gridPos: { x: 0, y: 12, w: 12, h: 8 }, yaxes: [{ format: "Bps" }],
                   targets: [{ expr: "rate(node_disk_io_time_seconds_total[$__interval])", refId: "A" }] }] },
      { type: "table", title: "Mounts", gridPos: { x: 12, y: 12, w: 12, h: 8 }, targets: [{ expr: "node_filesystem_size_bytes" }] },
    ],
  },
});

const imported = parseImport(grafana, "prom-1");
assert.equal(imported.format, "grafana");
assert.equal(imported.dashboard.title, "Node exporter");
assert.deepEqual(
  imported.dashboard.panels.map((p) => [p.title, p.kind, p.w, p.h]),
  [["Uptime", "stat", 3, 2], ["CPU", "timeseries", 9, 3], ["IO", "timeseries", 6, 3], ["Mounts", "table", 6, 3]],
);
assert.deepEqual(imported.dashboard.panels.map((p) => p.unit), ["seconds", undefined, "bytes_per_second", undefined]);
const cpu = imported.dashboard.panels[1].query;
assert.ok(cpu.source === "promql" && cpu.profileId === "prom-1" && cpu.expr.includes("[5m]"), JSON.stringify(cpu));
assert.ok(imported.dashboard.panels[2].query.source === "promql" && imported.dashboard.panels[2].query.expr.includes("[1m]"));
assert.equal(imported.skipped.length, 1, imported.skipped.join("; "));
assert.match(imported.skipped[0], /Notes — a text panel/);

// The panel types an SRE reads every day each land on their own kind.
const daily = parseImport(
  JSON.stringify({
    title: "Service",
    panels: [
      { type: "timeseries", title: "Latency", gridPos: { x: 0, y: 0, w: 12, h: 8 },
        targets: [{ expr: "histogram_quantile(0.99, sum by (le) (rate(http_request_duration_seconds_bucket[$__rate_interval])))" },
                  { expr: "histogram_quantile(0.5, sum by (le) (rate(http_request_duration_seconds_bucket[$__rate_interval])))" }] },
      { type: "heatmap", title: "Latency spread", gridPos: { x: 12, y: 0, w: 12, h: 8 },
        targets: [{ expr: "sum by (le) (increase(http_request_duration_seconds_bucket[$__interval]))" }] },
      { type: "timeseries", title: "Requests by code", gridPos: { x: 0, y: 8, w: 12, h: 8 },
        fieldConfig: { defaults: { custom: { stacking: { mode: "normal" } } } }, targets: [{ expr: "sum by (code) (rate(http_requests_total[5m]))" }] },
      { type: "graph", title: "Legacy stacked", stack: true, gridPos: { x: 12, y: 8, w: 12, h: 8 }, targets: [{ expr: "rate(x[5m])" }] },
      { type: "bargauge", title: "Disk", gridPos: { x: 0, y: 16, w: 8, h: 6 }, targets: [{ expr: "node_filesystem_avail_bytes" }] },
      { type: "state-timeline", title: "Up", gridPos: { x: 8, y: 16, w: 16, h: 6 }, targets: [{ expr: "up" }] },
    ],
  }),
  "prom-1",
);
assert.deepEqual(
  daily.dashboard.panels.map((p) => p.kind),
  ["percentiles", "heatmap", "stacked", "stacked", "bargauge", "state"],
);
const latency = daily.dashboard.panels[0].query;
assert.ok(latency.source === "promql" && latency.expr === "sum by (le) (rate(http_request_duration_seconds_bucket[5m]))", JSON.stringify(latency));

// root's own format round-trips, with fresh ids so an import never overwrites.
const again = parseImport(exportJson(imported.dashboard), null);
assert.equal(again.format, "root");
assert.equal(again.dashboard.panels.length, 4);
assert.notEqual(again.dashboard.id, imported.dashboard.id);

assert.throws(() => parseImport("{nope", null), /not JSON/);
assert.throws(() => parseImport("{\"title\": \"x\"}", null), /No panels/);

assert.deepEqual(movePanel(["a", "b", "c", "d"], 0, 2), ["b", "c", "a", "d"]);
assert.deepEqual(movePanel(["a", "b"], 1, 0), ["b", "a"]);
const panel = imported.dashboard.panels[0];
assert.equal(resizePanel(panel, 20, 0).w, 12);
assert.equal(resizePanel(panel, -20, -20).w, 2);
assert.equal(resizePanel(panel, 0, -20).h, 1);

assert.equal(resolveProfile("$service", { service: "orders-api" }), "orders-api");
assert.equal(resolveProfile("$service", {}), null);
assert.equal(resolveProfile("p1", {}), "p1");

assert.deepEqual(shortLabels(['{instance="web-1",job="node"}', '{instance="web-2",job="node"}']), ["web-1", "web-2"]);
assert.deepEqual(shortLabels(['{code="500",instance="a"}', '{code="502",instance="b"}']), ["500 · a", "502 · b"]);
assert.deepEqual(shortLabels(["sum(rate(x[5m]))"]), ["sum(rate(x[5m]))"]);
assert.deepEqual(shortLabels(["sum(x)", '{a="1"}']), ["sum(x)", '{a="1"}']);

console.log("dashboards: ok");
