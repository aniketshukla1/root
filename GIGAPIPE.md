# Gigapipe

How root relates to [Gigapipe](https://github.com/metrico/gigapipe) — what is built, what is
specified, and the one thing root should not do.

Gigapipe is an AGPL-3.0 Go warehouse that stores logs, metrics, traces and profiles in
ClickHouse and serves them back through four query languages. It is a drop-in replacement
for Loki, Prometheus, Tempo and Pyroscope, and it accepts data over OpenTelemetry, Loki,
Influx, Datadog and Elastic protocols.

It is not a competitor. It is a storage layer, and root is a client with no storage layer
at all.

---

## Three roles, one connection

A `gigapipe` connection is three things at once, and it is worth being explicit about
which of them is in play, because they fail differently.

| Role | What root does | Failure it catches |
| --- | --- | --- |
| **Monitored service** | Polls `/ready`, `/metrics`, `/config` | The warehouse itself is down, rejecting payloads, or ingesting nothing |
| **Log source** | Tails a LogQL selector into the shared log stream | Nothing — this is root *reading*, and a query that fails is reported as a collector error |
| **History source** | `promquery.rs` runs the operator's PromQL against it | Nothing — same |

The first role is why a gigapipe connection can be `Down`. The other two are why one is
worth having.

### The failure the first role exists for

`/ready` answers from the process. A ClickHouse that has gone away leaves gigapipe
answering `/ready` happily while every single query fails — the dashboards go flat, the
graphs keep drawing, and nothing says why. So the collector probes the read APIs per
signal rather than trusting readiness:

| Signal | Probe |
| --- | --- |
| logs | `GET /loki/api/v1/labels` |
| metrics | `GET /api/v1/labels` |
| traces | `GET /api/echo` |

A store that is ready but cannot serve any of them is `Down` with the reason named.
One that serves some is `Degraded`.

Profiles are not probed. `/pyroscope/render` requires a query, a from and an until, and a
probe that has to invent all three is testing its own parameters rather than the store.

---

## What is built

`ServiceKind::Gigapipe`, `collectors/gigapipe.rs`, a registry descriptor, and the
history-source widening. `cargo test` covers the parts worth breaking.

### Collected

**Metrics** — ingest rate (rows/s), bytes written, rejected payloads, backend connection
resets, resident memory, goroutines, query APIs up.

**Sections** — *Query APIs* (one row per signal, with root's own measured latency);
*Ingest pipelines* (one row per `service` label on `sent_rows`, `tracked()` so each gets
its own chart); *Build* (version, marked `revision()` so an upgrade draws a deploy marker).

**Logs** — an `Event` line when the build version moves. And, when `logQuery` is set,
every line matching that LogQL selector, as `Runtime`.

### Counters are not gauges

`sent_rows` and `sent_bytes` are monotonic counters, so the useful number has to be
derived between polls. A counter that went *down* means the process restarted; that
interval is dropped rather than reported. Drawing `(0 − 1000) / 10` on the ingest chart
would render a restart as a large negative spike and read as data loss, which is the
opposite of what happened.

### Options

| Key | Default | What it does |
| --- | --- | --- |
| `logQuery` | empty | LogQL selector whose lines join root's shared stream |
| `traceQuery` | empty | TraceQL selector listing traces, each of which opens as a waterfall |
| `traceSlowMs` | `0` (off) | Past this, a listed trace is degraded and alerted on |
| `traceLimit` | `20` | Traces per search |
| `traceLookbackMinutes` | `15` | How far back the trace search looks |
| `idleIngestMinutes` | `0` (off) | Degrade when nothing has been ingested for this long |
| `signalsDegrade` | `true` | Degrade when a query API stops answering |

`traceSlowMs` defaults off because the operator's TraceQL already said what it wanted;
grading every returned row again would raise an alert for exactly the traces they asked
to see. `idleIngestMinutes` defaults off because a warehouse waiting for its first write is idle,
not broken. Idleness is measured from when root first saw the store, not from the
counter's own start — a store that went quiet long before root launched has not just
gone quiet.

---

## The five gaps

### 1. Persistence — root keeps nothing

`history.rs`: *"History is kept in memory only, like the log buffer. It is deliberately
not persisted."* Quit root and every metric and every log line is gone. This is the
biggest gap and it has two halves.

**Read half — done.** `promquery.rs` was already endpoint-shaped rather than
Prometheus-shaped, and `commands.rs` resolves a history source by profile id without
checking its kind. Gigapipe serves PromQL at the same `/api/v1/query_range`. So the only
change needed was the frontend filter that offered Prometheus connections as candidates,
which now offers gigapipe too. A ClickHouse-backed store holds years where a Prometheus
holds weeks, so `forecast.rs` gets the window it has always wanted.

Logs got the same treatment through LogQL in `fetch_logs`.

**Write half — specified, not built.** root does not push its own snapshots anywhere, so
root's *own* judgement of a service — the derived health, the alert lifecycle, the change
markers — still dies with the process.

The mechanism, when it is built: **Influx line protocol at `/influx/api/v2/write`.**
Gigapipe also accepts OTLP at `/v1/metrics` and Datadog at `/api/v2/series`, but line
protocol is plain text. Emitting it is `format!`; OTLP would mean protobuf and gRPC, and
Prometheus remote-write would mean protobuf plus snappy. Zero new crates against two
substantial dependencies, for a payload gigapipe stores identically either way.

Shape: one measurement per connection, one field per metric key, tagged with profile id
and service kind. Written after each successful poll, best-effort — a warehouse that is
down must never hold up a poll cycle or mark a healthy service unhealthy.

This needs a deliberate decision first, and it is not a technical one. root currently
writes nothing anywhere except its own config and journal. Writing snapshots to a shared
warehouse makes root a producer of observability data rather than only a reader of it,
which is a different product. **Recommended: build it behind an explicit per-connection
opt-in, off by default, named plainly in Settings.**

Size: about two days, once that decision is made.

### 2. Traces — built. Profiling — not

Gigapipe serves TraceQL at `/api/search` and `/api/traces/{traceId}`, and Pyroscope at
`/pyroscope/render`.

A span is not a component: it has a duration, a parent, and a position in a tree, and
none of `HealthSnapshot`, `Section` or `Row` can express that. So traces got their own
type rather than being forced into a table.

**Built:**

1. `traces.rs` — `Span`, `TraceNode`, `Trace`, and `Trace::assemble`, which turns a flat
   span list into a tree. The tree is built in Rust, once, rather than in the frontend on
   every render.
2. `traceQuery` on a gigapipe connection, and a `traces` section listing what it matched.
   That table renders itself, because `Section` already knew how.
3. `ColumnSpec::trace()` — a column marked this way holds a trace id, and the table turns
   the cell into an opener. Declared the same way `revision()` is, so the frontend never
   learns which service types have traces and a future collector gets this free.
4. `TraceDetail` — the waterfall, which leads with *where the time went* rather than with
   the duration the operator already saw in the list.

**Three things deliberately kept rather than dropped.** A span whose parent is missing is
attached at the root and flagged, because it is where the request was when the trace went
dark. A parent cycle terminates instead of recursing. A duplicate span id is a retried
export, not a second occurrence. A span that vanishes silently is worse than one in the
wrong place: the operator cannot tell "it did not happen" from "we did not show it".

**Self time, not total time.** A span that took 900 ms because its child took 890 did not
spend 900. Ranking the per-service breakdown by total duration would point at the wrong
service every time.

**Not built:** `Correlate` does not yet place spans on its lanes. That is where traces
stop being a separate feature and start being the thing that explains an incident — about
three days, and the obvious next move.

Profiling is untouched: a flame graph is one component, one screen, one query, about a
week.

### 3. Push ingest — closed by proxy

Gigapipe accepts OTLP, Loki, Influx, Datadog and Elastic. root polls.

root should not grow a receiver. It binds `127.0.0.1` and holds credentials in the OS
keychain; a desktop app that also listens for other people's telemetry is a different
security posture, and the deploy-ingest endpoint already documents why that boundary
exists.

It does not need one. Anything shipped into gigapipe is readable out of it, so the
`logQuery` selector already brings OTLP-shipped application logs into root's shared
stream, and the history source already brings metrics from anything scraping into it.
Traces arrive the same way, through TraceQL.

**Status: closed for logs, metrics and traces.**

### 4. Multi-user — a genuine gap that root should not close this way

Gigapipe is a server. root is a desktop app that binds loopback only and stores secrets
in the OS keychain.

`team.rs` exists and models roles and an SSO server, so this is not unexplored ground.
But making root multi-user by making root a server would contradict the stance the
credentials and deploy-ingest sections of the README are built on, and would trade the
best thing about root — that it needs no deployment — for parity with a product that has
already won that particular argument.

**Recommended shape instead: shared backend, separate clients.** Several operators each
run root; all of them point at the same gigapipe. They see the same history and the same
logs because the *data* is shared, not the application. What stays local is what should
stay local: credentials, the assistant's authority level, and the judgement record in
`journal.rs`.

Under that reading the gap closes as a consequence of gap 1's write half, not as work of
its own.

If genuine shared state is wanted later — one operator acknowledging an alert and another
seeing it acknowledged — that is a real feature and it belongs in `team.rs`, sized on its
own merits. It is not a gigapipe gap.

**Status: addressed by architecture, not by code. Say so plainly rather than claiming
parity.**

### 5. Query languages — mostly already there

| Language | Status |
| --- | --- |
| PromQL | Already built, in `promquery.rs`, before gigapipe existed here |
| LogQL | Built, in `gigapipe.rs::fetch_logs` |
| TraceQL | Built, in `gigapipe.rs::search_traces` |
| Pyroscope | Not built — see gap 2 |

**Built.** The assistant reaches all three. `DetailQuery::GigapipeRead` carries a closed
set of nine reads — the same surface `gigapipe-mcp` exposes over MCP — and each is its
own tool under a `gigapipe_` prefix:

| Tool | Endpoint |
| --- | --- |
| `gigapipe_prometheus_query` | `/api/v1/query_range` |
| `gigapipe_prometheus_labels` | `/api/v1/labels` |
| `gigapipe_prometheus_label_values` | `/api/v1/label/{name}/values` |
| `gigapipe_loki_query` | `/loki/api/v1/query_range` |
| `gigapipe_loki_labels` | `/loki/api/v1/label` |
| `gigapipe_loki_label_values` | `/loki/api/v1/label/{name}/values` |
| `gigapipe_tempo_query` | `/api/traces/{id}/json` |
| `gigapipe_tempo_tags` | `/api/search/tags` |
| `gigapipe_tempo_tag_values` | `/api/search/tag/{name}/values` |

The limits are the Elasticsearch tool's: read-only, and a fixed set of endpoints rather
than a path the model composes. Label and tag names are percent-encoded before they
reach a URL, so a name carrying a slash addresses its own segment and not another
resource. A rejected query carries the store's own parse error back rather than a bare
status code, because the reason is the only part an operator can act on.

The prefix is load-bearing: root monitors Prometheus directly as well, so an unprefixed
`prometheus_query` would not say which connection it meant.

`tests/gigapipe_read.rs` pins all nine to the wire under wiremock. The surface is
entirely URLs, and `/label` against `/labels`, or a missing `/json`, fails by returning
nothing rather than by erroring — which looks exactly like a store with nothing in it.

---

## Order

1. ~~Connection type, LogQL tail, history source~~ — done.
2. ~~Traces: model, section, waterfall~~ — done.
3. Spans on `Correlate`'s lanes. About three days, and the thing that makes traces
   explanatory rather than merely present.
4. ~~Assistant access to LogQL, PromQL and TraceQL~~ — done, as nine `gigapipe_*` tools.
5. Snapshot write-out over Influx line protocol, opt-in. Two days, after the product
   decision in gap 1.

Then profiling: one flame graph screen, about a week.

Multi-user is deliberately absent. It closes as a consequence of 3, in the only form
root should want.

---

## Verified endpoint reference

Read from `metrico/gigapipe@master` rather than from documentation, because the reader's
`/ready`, `/config` and `/metrics` routes are commented out in `reader/router/misc.go` and
registered in `shared/commonroutes/routes.go` instead.

**Query**

```
/loki/api/v1/query_range      /loki/api/v1/labels        /loki/api/v1/series
/loki/api/v1/query            /loki/api/v1/tail          /loki/api/v1/patterns
/api/v1/query_range           /api/v1/labels             /api/v1/series
/api/v1/query                 /api/v1/label/{name}/values
/api/search                   /api/traces/{traceId}      /api/search/tags
/api/echo                     /api/metrics/query_range
/pyroscope/render             /pyroscope/render-diff
```

**Ingest**

```
/loki/api/v1/push       /v1/logs       /v1/metrics
/influx/api/v2/write    /api/v2/logs   /api/v2/series    /cf/v1/insert
```

**Operational**

```
/ready    /config    /metrics    /api/status/buildinfo    /api/v1/status/buildinfo
```

**Self-metrics** exposed on `/metrics`, declared in `writer/metric/variabels.go`:
`sent_rows{service}`, `sent_bytes{service}`, `json_parse_errors_count`,
`connection_reset_by_peer_count`, `tx_close_time_ms`, `send_time_ms`, plus the Go
runtime's own.

**Default port** 3100. **Auth** basic, when configured.

---

## Licensing

AGPL-3.0 binds gigapipe's own source. root talks to it over HTTP and links nothing, so
root's licensing is unaffected. This changes only if root ever bundles, modifies or
redistributes gigapipe itself — which the write-out design above deliberately avoids by
speaking a wire protocol instead.
