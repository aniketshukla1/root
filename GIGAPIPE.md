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
| `idleIngestMinutes` | `0` (off) | Degrade when nothing has been ingested for this long |
| `signalsDegrade` | `true` | Degrade when a query API stops answering |

`idleIngestMinutes` defaults off because a warehouse waiting for its first write is idle,
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

### 2. Traces and profiling — root has neither

Gigapipe serves TraceQL at `/api/search` and `/api/traces/{traceId}`, and Pyroscope at
`/pyroscope/render`.

This is the largest gap and the only one that needs new concepts in root rather than new
endpoints. A span is not a component: it has a duration, a parent, and a position in a
tree, and none of `HealthSnapshot`, `Section` or `Row` can express that. Forcing a trace
into a table would produce something technically present and useless.

What it needs:

1. A `Span` type and a trace tree in `model.rs` — the honest part of the work.
2. A `traceQuery` option and a `traces` section listing slow or failing traces, which
   `Section`/`Row` *can* express: one row per trace, with duration, span count, and the
   service where it spent its time.
3. A trace-detail drill-down screen. New UI, not a variation on an existing one.
4. `Correlate` learning to place spans on its lanes, which is where traces stop being a
   separate feature and start being the thing that explains an incident.

Profiling is a smaller version of the same problem: a flame graph is one component, one
screen, and one query.

Size: two to three weeks for traces end to end. Roughly one for profiling.
**This is the one to build after gap 1's write half, because it is the only gap where
gigapipe can do something root structurally cannot.**

### 3. Push ingest — closed by proxy

Gigapipe accepts OTLP, Loki, Influx, Datadog and Elastic. root polls.

root should not grow a receiver. It binds `127.0.0.1` and holds credentials in the OS
keychain; a desktop app that also listens for other people's telemetry is a different
security posture, and the deploy-ingest endpoint already documents why that boundary
exists.

It does not need one. Anything shipped into gigapipe is readable out of it, so the
`logQuery` selector already brings OTLP-shipped application logs into root's shared
stream, and the history source already brings metrics from anything scraping into it.
The remaining hole is traces, which is gap 2.

**Status: closed for logs and metrics. Closed for traces when gap 2 lands.**

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
| LogQL | Built now, in `gigapipe.rs::fetch_logs` |
| TraceQL | Gap 2 |
| Pyroscope | Gap 2 |

One piece is missing that is worth more than the languages themselves: **the assistant
cannot reach any of them.** root's advantage over gigapipe is an assistant that
investigates; an assistant that can also ask "what did this look like last Tuesday"
is a different tool from one limited to the last few hours in memory.

That means a `DetailQuery::LogqlQuery { expr, limit }` variant and a matching tool
definition, with the same deliberate limits the Elasticsearch tool has — read-only, one
endpoint, no arbitrary SQL. Size: about a day. **Highest value per hour of anything in
this document.**

---

## Order

1. ~~Connection type, LogQL tail, history source~~ — done.
2. Assistant access to LogQL and PromQL. One day. Highest value per hour.
3. Snapshot write-out over Influx line protocol, opt-in. Two days, after the product
   decision in gap 1.
4. Traces: model, section, detail screen, correlation lanes. Two to three weeks.
5. Profiling: one flame graph screen. About a week.

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
