# Gigapipe

How root relates to [Gigapipe](https://github.com/metrico/gigapipe) — what is built, and
the one thing root should not do.

Everything below that was a gap is now closed or deliberately declined. This is kept as a
record of the reasoning rather than as a plan, because the interesting part was never the
list: it was which of these root should *not* have built.

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

## The five gaps, as they were

Four are closed. The fifth is declined, on purpose, and says why.

### 1. Persistence — closed, both halves

`history.rs`: *"History is kept in memory only, like the log buffer. It is deliberately
not persisted."* That is still true of root's own buffer, and still deliberate — there
is no retention policy to manage because there is nothing to retain. The gap was that
nothing else held it either. It had two halves, and both are now closed.

**Read half — done.** `promquery.rs` was already endpoint-shaped rather than
Prometheus-shaped, and `commands.rs` resolves a history source by profile id without
checking its kind. Gigapipe serves PromQL at the same `/api/v1/query_range`. So the only
change needed was the frontend filter that offered Prometheus connections as candidates,
which now offers gigapipe too. A ClickHouse-backed store holds years where a Prometheus
holds weeks, so `forecast.rs` gets the window it has always wanted.

Logs got the same treatment through LogQL in `fetch_logs`.

**Write half — built, behind an opt-in.** root's *own* judgement of a service used to
die with the process: the derived health, the latency of the check, every metric it
computed. `influx.rs` now writes that reading out.

The mechanism is **Influx line protocol at `/influx/api/v2/write`.** Gigapipe also
accepts OTLP at `/v1/metrics` and Datadog at `/api/v2/series`, but line protocol is plain
text. Emitting it is `format!`; OTLP would have meant protobuf and gRPC, and Prometheus
remote-write protobuf plus snappy. Zero new crates against two substantial dependencies,
for a payload gigapipe stores identically either way.

One line per successful poll:

```
prod-docker,profile=p1,kind=docker root_health=2i,root_latency_ms=42,memory=9.4e9 …
```

Measurement per connection, defaulting to its name. Tagged with the profile id — the
identifier that survives a rename — and the service kind, which is read from serde rather
than matched a second time, so a new collector cannot ship without its tag. Fields are
root's own verdict under a `root_` prefix, then the collector's metrics under the keys
the rest of root already uses for them. A metric that is not a finite number is left out
rather than written as zero: a gap reads as "not measured", a zero reads as "measured,
and it was nothing".

Health is a field, not a tag, and ordered by severity so `max()` over a window is the
worst the connection was. A tag whose value changes would start a new series every time
health did, which is the one shape a time-series store handles badly.

**The decision that gated this was not technical**, and it was made as recommended: an
explicit per-connection opt-in, off by default, named plainly in Settings under "Write
snapshots out". An estate that never turns it on behaves exactly as it did before.

Best-effort throughout. The write is detached from the poll cycle with one outstanding
write per connection, and a failure is logged and swallowed — a service marked degraded
because the place root files its opinions is unreachable would be root reporting on
itself and calling it infrastructure. The cost of that is that the poll loop cannot tell
anyone whether it is working, so Settings has a **Write one now** button that returns the
exact line it sent, or the warehouse's own refusal. That is the only place a write
failure is allowed to be loud.

What is still local: the alert lifecycle and the change markers. Both are event streams
rather than samples, and `/loki/api/v1/push` is the honest destination for them, not a
measurement with a count in it.

`tests/influx_write.rs` pins the body, the path, both auth schemes and every failure mode
to the wire under wiremock; ten unit tests in `influx.rs` cover the encoding, including
the escaping and the every-kind-has-a-tag property.

### 2. Traces and profiling — both built

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

5. `Correlate` places spans on its lanes. Each lane lists the requests that spent time
   inside it, self time first, amber once the majority of a request's life was spent on
   that one connection — and a lane that logged nothing during the window but held a
   request for two seconds is no longer dropped for being empty.

**Profiling — built.** `/pyroscope/render`, decoded in `profiles.rs`, drawn by
`ProfileDetail`.

The wire format is Pyroscope's flamebearer: one array of symbol names and one array per
row of the graph, each row a flat run of `[x offset, total, self, name index]`. The
offset is **delta encoded against the end of the previous frame on that row**, which is
the one detail that matters: read as an absolute position it produces a graph that is
subtly, plausibly wrong — every frame shifted left, the wide ones still wide. Both
implementations encode it the same way (Pyroscope's `deltaEncoding`, gigapipe's `BFS`),
and neither promises the rows arrive in ascending order, so parents are resolved by
containment against a sorted copy of the row above rather than by walking both in step.

`from` and `until` are **milliseconds** — gigapipe parses them with `ParseInt` and
divides by a thousand. Sending seconds asks for a window in 1970 and gets a 200 with an
empty profile, which is indistinguishable from a service that was idle. Asserted on the
URL in `tests/profile_read.rs` for that reason.

**The ranking leads, not the picture.** The same function usually appears in a dozen
places in a graph, once per call path, each sliver too thin to notice and together the
reason the process is slow. So self samples are summed per symbol and ranked above the
graph, with a count of how many call sites contributed. The graph is how you check the
ranking. Colour is self time rather than width, so a frame that is wide only because it
waited on its children stays cool.

**Declared, like everything else.** `profileQuery` on the connection names the profile
type and label set; nothing infers which of a dozen profile types an operator meant. A
connection that declares one gets a `Profiles` section whose selector column is marked
`ColumnSpec::profile()` — the third such marking after `revision` and `trace_ref`, and
for the same reason each time: the frontend never learns which service kinds hold
profiles. A declared profile is also probed each poll like the other three query APIs,
so a store that stops serving profiles degrades the connection.

**Not built:** the diff view. `/pyroscope/render-diff` answers in the `double` format,
seven integers per frame instead of four, which `decode` refuses by name rather than
misreading. It is a different question and would be a different screen.

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

**And the same surface is served outward.** `src/mcp.rs` plus the `root-mcp` binary make
root an MCP server over stdio, advertising these nine under `gigapipe-mcp`'s own names —
the prefix stripped, since nothing else is served there to be ambiguous with. It reads
`GIGAPIPE_HOST` / `USERNAME` / `PASSWORD` exactly as that server does, so a client
configuration works with the binary swapped; with none set it serves root's own
configured gigapipe connections and their keychain secrets instead. Only non-mutating
tools are ever advertised, because a pipe has no confirmation dialog.

`tests/gigapipe_read.rs` pins all nine to the wire under wiremock. The surface is
entirely URLs, and `/label` against `/labels`, or a missing `/json`, fails by returning
nothing rather than by erroring — which looks exactly like a store with nothing in it.

---

## Order, as it was planned

Kept because the sequence was the argument: each item made the next one worth having, and
profiling went last because a flame graph with no trace to open it from is a curiosity.

1. ~~Connection type, LogQL tail, history source~~ — done.
2. ~~Traces: model, section, waterfall~~ — done.
3. ~~Spans on `Correlate`'s lanes~~ — done. Each lane lists the requests that spent time
   inside it, self time first, and a lane that only appears in a trace is no longer
   dropped for having logged nothing.
4. ~~Assistant access to LogQL, PromQL and TraceQL~~ — done, as nine `gigapipe_*` tools.
5. ~~Snapshot write-out over Influx line protocol, opt-in~~ — done, off by default.

~~Then profiling: one flame graph screen~~ — done, and reachable from the connection's
own Profiles section rather than from a destination of its own.

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
