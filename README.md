# root

Agentic infrastructure management for your whole estate, from one window — live health,
metric history, a unified log stream, alerting, and an AI assistant that can actually
investigate rather than guess.

Monitoring is half of it. Watching tells you a thing is wrong; reading the configuration
that governs it tells you why, and which number to change. A Redis at 1.9 GB of a 2 GB
`maxmemory` with `maxmemory-policy noeviction` is not a memory graph to keep an eye on —
it is a decision that starts refusing writes in about four hours, and neither the metric
nor the config file says so alone.

Built with Tauri 2 (Rust backend, React + TypeScript frontend).

The frontend is eight flat screens — Overview, Services, Alerts, Metrics, Logs,
Assistant, Scorecard, Settings — with service detail, component detail, a trace
waterfall, and correlation as drill-downs. See
[`FRONTEND_ARCHITECTURE.md`](FRONTEND_ARCHITECTURE.md) for the extension
contract, the read-model rules, and the colour rules.

**The frontend knows about no service type in particular.** Adding a new kind of
infrastructure is a backend-only change; see [Adding a service type](#adding-a-service-type).

---

## What it does

**One dashboard.** Every connection reports a health snapshot on its own interval:
service-level metrics plus each individual component — containers, jobs, agents, cluster
nodes, indices, databases, long-running queries — each with its own health and numbers.

**One log stream.** Lines from every source are normalized into the same shape
(timestamp, connection, source, level, message) and land in a shared in-memory buffer you
can search and filter across. Nothing is written to disk.

**Alerts that derive themselves.** Collectors express intent by setting component health
and metric thresholds; the alert engine reads those. Alerts are keyed by condition, so a
problem that persists updates in place instead of piling up duplicates, and one that
recovers is marked resolved rather than silently vanishing.

**Logs that separate, and correlate.** Every line is classified by stream —
`runtime` (container stdout, ES documents), `event` (build results, endpoint
transitions), `diagnostic` (Redis slow commands, long-running Mongo operations),
and `build`, which is reserved for console output and which no collector emits
today. The shared view shows runtime and events by default; database slow
queries are rows in Active queries rather than duplicate log lines, and a
Jenkins console is fetched for one build rather than pushed at everyone.
Correlation is then a *lane* view: pick any moment and see what every system was
doing, grouped by source and aligned on time, ordered by likely relevance.

**Metric history everywhere.** Every service-level metric is kept as a rolling series, so
each card carries a sparkline, each detail page a set of charts, and the assistant can
answer "is this getting worse?" rather than only "what is it now?".

**Down to the individual component.** Services drill into their parts: one container's CPU
and memory, one Elasticsearch node's heap and disk, one Jenkins job's duration — each with
its own charts, logs, alerts and actions. Collectors opt a section in, so only bounded,
long-lived entities are retained; high-cardinality ones (indices, pods, running queries)
still get a page, showing live values instead of a stored series.

**Deploys on the timeline.** Collectors tag one column as their revision — a container
image, a build number — and the change engine diffs it between polls. A deploy becomes a
marker on every chart on that service's dashboard, so a spike sits next to the thing that
caused it. The engine knows about no service type: tagging a column is the whole opt-in.

A pipeline that already knows it shipped can say so instead of waiting to be noticed, via
an opt-in local ingest endpoint. It is off by default, binds loopback only, and needs a
bearer token; reported deploys are tagged as such, because an assertion is not an
observation. See [Deploy ingest](#deploy-ingest).

**Log analytics, not just a tail.** A severity-stacked activity histogram over the whole
server-side buffer, click-to-zoom on any bucket, and severity facets with live counts.
Nobody can read a log tail, so lines are also clustered with Drain into *templates* — the
message with its variable parts replaced by `<*>` — which can be ranked, counted and
sparklined. The line you care about becomes one row instead of something you scroll past.

**Which metrics actually moved.** Picking a window and eyeballing the charts finds the
loud series, not the relevant one. Metrics are scored against a baseline four times the
window's length immediately preceding it, with a two-sample Kolmogorov–Smirnov test and a
volume heuristic beside it — because KS is blind to the case that matters most in
practice, a metric that was flat at zero and then switched on.

**Traces, as a tree rather than a table.** Everything else root collects is a list of
things with numbers on them, which `Section` plus `Row` renders. A span is not that — it
has a duration, a parent, and a position in a tree, and the whole value of a trace is
seeing that the 900 ms went *there*, three levels down, in a service nobody suspected. So
traces get their own type and their own waterfall. It leads with the per-service breakdown
ranked by *self* time — each span's duration minus what its children accounted for — so a
span that took 900 ms because its child took 890 does not get the blame, and ranking does
not just name the root span every time. Spans whose parent never arrived are drawn at the
root under an `ORPHAN` tag rather than dropped, because an operator cannot otherwise tell
"it did not happen" from "we did not show it".

**Incidents, not a list of alerts.** When orders-api OOMs and three things that talk to
it start complaining, an operator does not have four problems. Alerts are grouped into
incidents from what the collectors already recorded — onset times, the declared dependency
graph, observed changes — with no model involved, so an estate with no LLM configured gets
the whole benefit and a model that is wrong can only mis-describe an incident, never
invent one. Every incident carries the reasons its members were grouped, so a wrong
grouping is arguable rather than mysterious. Dependency edges are *declared*, never
discovered: root watches connections, not the network between them, so it narrows
candidates and still leaves the caller needing evidence.

**What a container is running, and where a connection lives.** One connection, one system
stops holding the moment everything is containerised — a Docker host is twenty systems of
a dozen kinds, and listing twenty containers is true and useless. An image name is the
container's own statement about what it runs, so `postgres:16` is read rather than
guessed. A connection can also *declare* where it runs, which root then reconciles against
its own inference on every poll; the disagreements are the output, because a disagreement
is either root being wrong or the estate having changed.

**An assistant with real access.** It reads your monitoring data through a fixed set of
tools and correlates across services — an alert leads to a snapshot, which leads to logs,
which leads to a container inspect. It cites the exit code and the log line.

---

## Reading configuration

Monitoring says a thing is wrong. Configuration says why, and what to change. Telemetry
alone says a Redis is at 1.9 GB and climbing — a fact with no action attached, because the
metric cannot tell you whether that is fine forever or four hours from an outage. Config
alone says `maxmemory 2gb` and `maxmemory-policy noeviction`, which is a reasonable
setting for an instance that never fills. Together they say: this stops accepting writes
in about four hours, here is the setting that decides it, here is the number to change.

**Declared, never discovered.** An operator names the paths. Nothing crawls a filesystem
looking for things that might be config — a tool that goes looking eventually finds
something it should not have opened, and the first time that is a private key it does not
matter how good the intent was. Reads are read-only and stay that way.

**Redacted in the reader, not in the prompt.** Config files are where credentials live:
`requirepass` in redis.conf, the environment block of a compose file, a datasource
password in application.yml. The assistant sends what it reads to a third-party API, so
redaction happens between the file and *every* consumer — there is no accessor that
returns an unredacted body. A prompt is one caller among many and the next one will
forget. Over-redaction has a real cost too, so this aims at values that are secrets rather
than at settings that merely sound sensitive: a review that cannot see `maxmemory`,
`max_connections` or `worker_processes` is worthless.

**Rules, not prompts.** The assistant could be handed the config and the metrics and asked
to spot this, and it would — often. "Often" is the problem: a recommendation that is
usually right is one nobody can build a process on. The findings that matter are derived
by rules that always fire on the same input, and the assistant's own findings are kept
separate and scored separately (below).

### When it becomes a problem, not just that it is one

Fill level answers whether something is a problem now; capacity planning needs the other
question. That is one derivation applied wherever a limit is declared — a disk filling, a
connection pool filling, an inode table filling, etcd's backend quota filling — rather
than re-implemented per collector.

The hard part is not the arithmetic, it is refusing to answer. A straight line through any
history produces a slope, and a slope always produces a date. On a disk climbing steadily
for a week that date is worth acting on; on CPU oscillating between 20% and 80% all day
the identical arithmetic yields a confident "full in four hours" that is pure artefact.
A projection nobody can trust is worse than none, because the first wrong one teaches
people to ignore the rest.

Which is why root can read from a Prometheus you already run. Its own history is a ring
buffer holding a few hours, deliberately, so there is no retention policy to manage — and
a volume that fills in three weeks is invisible to a forecast built on that. Prometheus is
read here and never written; it is a source, not a store.

---

## Is it any good?

Everything else in this app is disposable: metric history is a ring buffer, logs are a
ring buffer, incidents are derived fresh on every read, and all of it is gone when the
process stops. The judgement record is the exception, and the only thing on disk besides
configuration.

A record of which findings were right, which were noise, and which problems root missed
entirely is the only dataset here that gets **more** valuable with age. It is also the only
thing that can answer the question every step of this product's roadmap depends on. The
**Scorecard** screen is that record: precision from rated findings, recall from incidents
root should have caught and did not.

Three things feed it, for three different reasons:

- **The assistant reports findings through a tool**, not in prose. Prose cannot be scored,
  so the column for the model would have stayed empty forever and the loop this
  measurement exists to run would never have started. The set of things the model found
  that no rule covers, *and that a human confirmed were real*, is the queue of rules worth
  writing next — and each one written moves a class of problem from the half that has to
  be trusted to the half that can be checked.
- **Backtesting** replays today's rules over the estate's own Prometheus history, because
  ratings arrive slowly: one estate produces a handful of findings a week, and waiting for
  a sample large enough to gate a decision on would stall the roadmap for a year. The one
  thing it cannot do is replay configuration — Prometheus stores numbers, not `redis.conf`
  — so a replay uses today's config and says so.
- **Scenarios** are known situations with known right answers. A unit test fails a build,
  which is the right tool for "this rule's arithmetic is correct" and a bad tool for the
  question that actually comes up: somebody widened a threshold or swapped a model, so is
  the product better or worse than it was? A suite reporting 18/20 with the two names that
  regressed answers that directly, and can be run by somebody who is not debugging.

---

## Shared workspaces

Optional, and additive. With no server configured root reads a config file on this machine
and answers to nobody — the mode that needs no trust in anything beyond the OS keychain.

What a server adds is a shared identity: several people looking at the same estate, told
apart, with a role attached. This client stores what the server told it and offers what
that role can do; it does not enforce anything. A cached role is a hint about which
buttons to draw, never a permission check — a client enforcing its own permissions would
be enforcing nothing, since the person it is enforcing against owns the machine it runs
on. Every mutating call still goes to the server.

---

## Adding a service type

The UI enumerates nothing. It asks the backend for a catalog of `ServiceDescriptor`s and
renders whatever it is handed — the connection picker, the settings form, badges, colours,
dashboards, charts, and the sidebar all come from that. A frontend build that has never
heard of a service still renders it correctly, falling back to a derived label, monogram,
and a stable accent colour.

So adding one is three steps, all in Rust:

1. Implement `Collector` (`collect` + `fetch_logs`, optionally `detail` and `run_action`).
2. Add a `ServiceKind` variant and a line in `build_collector`.
3. Append a `ServiceDescriptor` to `registry::catalog()` — label, monogram, accent,
   category, blurb, and the `FieldSpec`s its settings form needs.

Three optional one-line opt-ins come with it, all on the sections a collector already
declares. `ColumnSpec::revision()` marks the column whose value identifies a version, and
the change engine turns a move in it into a deploy marker. `ColumnSpec::trace()` marks a
column as holding a trace id, and every cell in it opens as a waterfall instead of
rendering as text. `Section::tracked()` keeps per-row history for that section's numeric
columns, giving its components charts of their own. None needs any detection code, and a
collector that uses none of them still works.

Nothing in `src/` changes. The **HTTP endpoint** collector was deliberately added after
the UI was finished to prove this: it declares four options of its own
(`expectStatus`, `expectBody`, `slowMillis`, `method`) and picked up a working form,
card, charts, and assistant tooling with no frontend edit.

Two things make this hold rather than merely being a convention:

- `ProfileOptions` is an open JSON map with typed accessors, not a fixed struct, so a new
  collector's options never touch shared types.
- `ServiceKind` is an open `string` in TypeScript, never a union — a registry test walks
  `ServiceKind::ALL` and fails the build if any variant lacks a descriptor the UI could
  render.

---

## Collectors

Twenty-eight service types, grouped by the `category` each one declares in
`registry::catalog()`. A collector that emits no log lines says so — most do not,
because most infrastructure exposes an API and not a stream.

### Containers and orchestration

| Source | What is collected | Logs |
| --- | --- | --- |
| **Docker** | Container inventory, per-container CPU / memory / network, health-check state, restart and exit status, engine version | `docker logs` tailed per running container; stderr with no level of its own is read as a warning |
| **Podman** | The same Engine API Docker serves, from a daemonless, rootless engine. The collector carries its kind rather than hard-coding Docker, so a failed connection names Podman's socket and a working one is labelled with the engine actually running | Container logs, tailed identically |
| **Kubernetes** | Node readiness, workload rollout state, and pods that are crash-looping or stuck pending. Nodes and workloads are listed in full; pods appear only when something is wrong with them, because a table of every pod in a real cluster is the wall of noise it replaced. Auth comes from kubeconfig, so EKS/GKE exec plugins work with no token to paste | Warning events from the cluster event stream |

### Hosts and storage

| Source | What is collected | Logs |
| --- | --- | --- |
| **Linux host** | Disk fill per mount and never averaged — a root filesystem at 99% beside a data volume at 3% averages to comfortable, and only one of them stops the machine. Memory *available* rather than free, because `MemFree` on a healthy box is near zero. Load against the core count from the same scrape | None. node_exporter exposes metrics, not a stream |
| **NFS exports** | Fill *and* inode exhaustion per export, since millions of small files run out of inodes while `df -h` still shows 40% free. Exports are grouped by the server they come from, so one dead server is one row rather than six broken teams. `node_filesystem_device_error` is checked before any fill number is believed — a stale mount reports zeroes that otherwise read as an empty, healthy export | None |

### Endpoints and proxies

| Source | What is collected | Logs |
| --- | --- | --- |
| **HTTP endpoint** | Uptime, status code, response time, and an optional body assertion — catches a service that answers 200 while being unable to reach its own database | State *transitions* only, so a healthy endpoint stays silent |
| **Nginx** | Active connections, request rate, and the reading / writing / waiting split from `stub_status` — plus the gap between accepts and handled, which is nginx dropping connections for want of file descriptors or worker slots, and is invisible if you read only the totals | None |
| **Apache httpd** | Worker saturation from the `mod_status` scoreboard, which carries the denominator `BusyWorkers` alone cannot: 118 busy is fine at 400 slots and fatal at 128. States are kept apart — a wall of `W` is a server under load, a wall of `R` is one held open by slow clients, and `G` piling up is a reload stuck behind long requests | None |
| **Apache Tomcat** | Thread pool saturation per connector, plus request and error rates, from the manager's JMX proxy. Pools are never summed: 200 busy of 400 reads as half capacity and can be one connector completely exhausted while the other sits idle | None |
| **HAProxy** | Frontends, backends, and the servers behind each one counted individually — a BACKEND row reports `UP` on its last surviving server, which is the right answer to "can this serve" and the wrong one to "is this healthy". `qcur`, the queue that fills while there is still time to act, is read as the leading signal | None |
| **Traefik** | Routers, services, and the servers behind them — including routers that fail to build, pointing at a missing service or an unresolved certificate. Traefik answers those requests itself, so an uptime check on the proxy stays green while every request to that route 404s | None |

### Databases, caching and search

| Source | What is collected | Logs |
| --- | --- | --- |
| **PostgreSQL** | Connection saturation, database and table sizes, long-running queries, blocked locks, cache hit ratio, deadlocks, replication role and lag | None. SQL reaches no log stream, and the collector does not invent one: slow queries and lock waits are rows in Active queries carrying the full statement and a terminate action, not synthesized lines duplicating the same fact |
| **MySQL** | Thread and connection counts, schema sizes, long-running queries, slow-query and aborted-connect counters, replication state. Works with MariaDB | None, for the same reason |
| **MongoDB** | Connection count against the limit, replica set health and lag, and database sizes, from four read-only admin commands. `replSetGetStatus` on a standalone and `currentOp` without privileges both fail on perfectly healthy servers, so each degrades to an empty section rather than reporting the database down | Operations running past the threshold, as `diagnostic` lines rather than runtime ones |
| **ClickHouse** | Running queries, merges, and replicated tables — including `is_readonly` ones, which keep answering queries correctly from data they already had while inserts fail and the replica drifts. `queue_size` moves before `absolute_delay` does | None |
| **Redis** | Memory used against the limit, evictions, connected and blocked clients, ops rate, hit rate, and replication link state, all from `INFO`. `KEYS` and `SCAN` are absent by design — a monitor that walks a production keyspace every poll causes the incident it reports | Slow command log entries, as `diagnostic` lines |
| **Memcached** | Hit rate, memory fill, item count, and the eviction *rate*. A cache at 96% hits can still be badly undersized, because the hot keys keep answering while everything else is thrown out to make room. Reclaims are counted apart from `expired_unfetched`, since only the first means pressure | None. The text protocol serves `stats` and nothing else |
| **Elasticsearch** | Cluster colour, node JVM heap / CPU / disk, shard counts (unassigned, relocating, initializing), per-index size and doc count. Works with OpenSearch | Documents tailed from a configured index pattern (ECS-aware); silent until one is set |

### Messaging and coordination

| Source | What is collected | Logs |
| --- | --- | --- |
| **Kafka** | Consumer group lag, offline and under-replicated partitions, topic sizes, and broker membership, over the native protocol. A group behind with *no members* is a stopped pipeline rather than a slow one. Per-poll work is bounded rather than proportional to partition count | Derived events only, each reported once: a partition going offline, or a group with lag and nobody on it |
| **RabbitMQ** | Queue depth and growth, consumer counts, unacknowledged messages, and node memory and disk alarms, via the management API. A queue at 200 with zero consumers is treated as worse than one at 50,000 being drained by twenty | Memory and disk alarms, once each as they are raised |
| **NATS** | Connections, subscriptions, and cluster routes from `/varz`, plus route count — a clustered server that lost its routes looks healthy from its own side. And `slow_consumers`, which is the only trace NATS leaves when it drops a subscriber's pending messages and disconnects it; the publisher is never told | None |
| **ZooKeeper** | Ensemble role, outstanding request backlog, and `zk_synced_followers` — a leader reporting four followers and two synced has already lost its fault tolerance, and nothing about the port or `zk_server_state` shows it. Read with the `mntr` four-letter word, so the monitor is not one more client against the connection limit it is watching | None |
| **Consul** | Raft leadership, peer count, and every check the cluster knows about — with node and service failures counted apart. One dead agent fails `serfHealth` plus every service registered on it, which reads naively as twenty service outages and is one host | None |
| **etcd** | Raft health and, the number that matters, mvcc database size against the backend quota. Reaching it raises `NOSPACE` and turns the cluster read-only — every write in a Kubernetes cluster fails while `/health` keeps answering true. `/metrics` is parsed in-tree rather than pulling in a Prometheus dependency | None |

### CI/CD, observability and cloud

| Source | What is collected | Logs |
| --- | --- | --- |
| **Jenkins** | Job results and durations, build queue depth *and the reason each item is blocked*, executor counts, agent online/offline with cause | Build outcomes as they finish, as `event` lines. Console output is not in the stream at all — it is a per-build detail fetch, because forty lines per failed build in the shared view buried everything else |
| **Prometheus** | Scrape target health, firing alert rules, and TSDB series count. A target that silently stopped being scraped is invisible in a dashboard built on its data; the graph just goes flat. Deliberately not a PromQL client — the question here is whether Prometheus is scraping what it thinks it is | Each alert once, as it starts firing |
| **Gigapipe** | The one connection that is two things: a service root watches, and a store root reads *from*. Query APIs are probed per signal — logs, metrics, traces — because `/ready` answers from the process, and a ClickHouse that has gone away leaves gigapipe answering happily while every query fails. Ingest rate is derived between polls from monotonic counters, and an interval where a counter went *down* is dropped rather than reported as a negative rate, because that means the process restarted. Plus rejected payloads, backend resets, traces matched and slow. See [`GIGAPIPE.md`](GIGAPIPE.md) | Version transitions, and — with a LogQL selector set — real application logs pulled from whatever ships *into* gigapipe, which is why root needs no OTLP receiver of its own |
| **AWS** | One connection is an account and region; buckets and queues are components inside it. S3 object count and stored bytes against lifecycle policy — growth that nothing will ever delete. SQS depth, in-flight count, and dead-letter configuration: depth with *nothing in flight* means no consumer is even trying. Object counts walk a bounded number of pages and say when they stopped rather than reporting a fraction as a total | None. Oldest-message age is left to CloudWatch on purpose: reading it off the queue would increment every message's receive count and dead-letter the backlog the monitor was watching |

### On log delivery

Logs are pulled on each poll using a per-connection high-water mark, not through N
long-lived follow streams. At the default 5–10s interval this is effectively real time,
and it degrades far better: a flapping endpoint costs one failed request instead of
leaking a task per container.

### On Docker socket discovery

bollard's built-in "local defaults" only knows `DOCKER_HOST` and `/var/run/docker.sock`,
which misses the most common desktop setup there is — Docker Desktop on macOS puts its
socket under `~/.docker/run/`, and `/var/run/docker.sock` often does not exist at all.
root probes for a socket that actually exists, covering Docker Desktop, Colima,
Rancher Desktop, and rootless Podman.

### On stopped containers

Any working machine accumulates dozens of long-dead containers. Alerting on all of them
buries the one that just crashed, so a container that exited weeks ago is listed as
history rather than a live fault. Recent exits, non-zero exits with no rendered age, and
failing health checks are all still surfaced.

---

## The assistant

### Bring your own model

Three providers behind one trait:

- **Anthropic** — the Messages API.
- **OpenAI-compatible** — any `/chat/completions` endpoint. OpenAI, Groq, OpenRouter,
  Together, DeepSeek, Fireworks, vLLM, LM Studio, llama.cpp. Point the base URL at the
  service whose key you are using.
- **Ollama** — a local server. No key, nothing leaves the machine.

Tool calling is normalized across all three, which is most of the work: Anthropic has no
`tool` role (results are `tool_result` blocks inside a *user* message, and consecutive
ones must be merged), OpenAI sends tool arguments as a JSON string, and Ollama sends them
as an object with no call id at all.

### Two levels of authority

Set in Settings:

- **Read-only** — the assistant can inspect everything and has no mutating tools
  whatsoever. Remediation is surfaced through `propose_command` as an exact command for
  you to run. Nothing is executed.
- **Allow actions** — adds tools to restart/start/stop containers, trigger Jenkins builds,
  and terminate database queries. **Every single call stops for explicit approval**, in a
  dialog naming the exact target and showing the equivalent shell command. There is no
  "always allow". Declining is fed back to the model as a result, so it adapts instead of
  retrying; a request that times out is treated as declined.

### Deliberate limits

There is no arbitrary-SQL tool and no shell tool. Database access is a fixed set of five
named diagnostics (`connections`, `long_queries`, `table_sizes`, `locks`, `replication`).
The Elasticsearch tool can only reach `_search`. The nine gigapipe store reads each name
one endpoint rather than taking a path. Anything the app cannot do safely is routed
through `propose_command` for a human.

Configuration reaches the model already redacted — the redaction lives in the reader, so
no tool can return an unredacted body. And findings come back through a tool rather than
in a sentence, which is what makes them attributable and rateable beside the rules on the
Scorecard; see [Is it any good?](#is-it-any-good).

### Reading the warehouse

root's own history is a ring buffer of a few hours and its log buffer is in memory, so
"what did this look like last Tuesday" is a question root cannot answer about itself. A
[gigapipe](GIGAPIPE.md) connection can, and the assistant reaches it through nine
read-only tools — the same surface `gigapipe-mcp` exposes over MCP, with root's limits
on it:

- **Metrics** — `gigapipe_prometheus_query` (PromQL over a range), plus `_labels` and
  `_label_values` so a selector can be written against labels that exist rather than
  guessed at.
- **Logs** — `gigapipe_loki_query` (LogQL), plus `_labels` and `_label_values`. This
  searches what was *shipped into* the store, which is a different and far longer-lived
  body of logs than `search_logs`, which searches root's own buffer.
- **Traces** — `gigapipe_tempo_query` for one trace as the store holds it, plus
  `_tags` and `_tag_values`.

Each names a fixed endpoint; none takes a path the model composes. Label and tag names
are percent-encoded before they reach a URL, so a name carrying a slash cannot address a
different resource. A rejected query comes back with the store's own parse error rather
than a bare status code, because the reason is the only part anyone can act on. All nine
are read-only, so they stay available in **Read-only** mode.

The `gigapipe_` prefix matters: root monitors Prometheus directly too, and an unprefixed
`prometheus_query` would not say which connection it meant.

### One reading of one component

Hovering a failing cell in the component matrix asks a different question from the
assistant's: given this restart count, the image that changed and the four log lines
naming this container, *what is this?* That is a sentence, and `explain_component`
answers it without the agent loop — no system prompt listing every tool, no round trips,
no conversation that grows. It builds the facts in-process from the row's cells, the alert
naming it, any deploy in the window and the most recent log lines mentioning it, then
makes one call with no tools and a small budget.

Four rules keep it honest and cheap:

- **A hover is not a request.** Nothing is sent until the tooltip has been open on the
  same component for 550ms; dragging across one block touches a dozen cells in a second.
- **A reading is computed once per state**, not once per hover — cached by a fingerprint
  of the health, the summary and the alert behind it, so the same broken container asked
  about twice is one call and one that breaks differently is a new one.
- **The facts travel with the text.** `grounds` is every line the model was given, and the
  tooltip says how many there were and which model said it. A summary nobody can check is
  a rumour with good typography.
- **Healthy components are refused rather than answered.** A model asked what is wrong
  with something that is fine will find something, and most cells are fine.

---

## Deploy ingest

Settings → Deploy ingest. **Off by default.** While on, root accepts deploy records and
turns each into a marker on that connection's timeline.

```bash
curl -X POST http://127.0.0.1:7717/v1/deploys \
  -H "Authorization: Bearer $ROOT_INGEST_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"service":"checkout-api","version":"1.9.0","connection":"prod-docker"}'
```

`connection` matches a configured connection by id or case-insensitive name, and an
unknown one is rejected — a marker always belongs to something real. `previousVersion`,
`at`, and `note` are optional. `GET /v1/health` needs no token, so a script can check
reachability first.

What it does and does not expose:

- Binds **`127.0.0.1` only**, never `0.0.0.0`. Nothing off the machine can reach it,
  whatever the firewall says.
- The bearer token is generated on first enable, stored in the OS keychain, shown once,
  and compared in constant time. Rotating it invalidates the old one immediately.
- Auth is checked before the body is parsed; bodies are capped at 8 KB; every string is
  trimmed and length-clamped; `at` is clamped to ±24h so a skewed clock cannot smear the
  timeline.
- Reported deploys are tagged `reported` in the UI. A pipeline that posts a deploy and
  then fails still leaves a marker, so an assertion never reads as an observation.

**A cloud CI runner cannot reach your laptop.** The poster has to be on the machine — a
deploy script, a git hook, a self-hosted runner — or arrive through a tunnel you set up.
That is the trade for not binding a credential-holding desktop app to the network.

---

## Credentials

Passwords and API keys go into the OS keychain (Keychain on macOS, Credential Manager on
Windows, D-Bus secret service on Linux). The config file on disk holds only endpoints and
options — never a secret. For databases, the username and password are merged into the
connection URL at connect time, so the password never touches `config.json`.

Config lives at `~/Library/Application Support/com.root.observability/config.json` on macOS
(the platform config dir elsewhere), and is written via a temp file and rename so a crash
mid-write cannot truncate your connection list.

Builds using the former `com.inframonitor.desktop` or `com.inframonitor.app`
identifiers are migrated on first launch by copying their config into the root
directory. Original files and the established keychain namespace are retained
for rollback and credential continuity.

---

## Running it

```bash
npm install
npm run tauri dev
```

Release bundle:

```bash
npm run tauri build
```

Requires Node 18+, a Rust toolchain, and the Tauri 2 platform prerequisites.

### Previewing the UI without a backend

```bash
npm run dev   # then open http://localhost:1420/preview.html
```

`preview.html` stubs the Tauri IPC bridge with representative data — several services in
different health states, metric history, an error burst in the log histogram, and a
service type the frontend has never heard of — so layout, charts, and the dark console can be
inspected in a plain browser. It is dev-only; `vite build` bundles `index.html` alone.

### Tests

```bash
cd src-tauri && cargo test
```

466 unit tests, none ignored. They cover health mapping, alert lifecycle, deploy
detection, component history admission and eviction, log filtering and stream separation,
section renderability, conversation trimming, the per-provider message translation, and
the parts with the most judgement in them — redaction, forecasting's refusals, incident
grouping, config rules, and the journal.

Nine integration suites sit in `src-tauri/tests/`, each against something real:

| Suite | Against | Why it is not a unit test |
| --- | --- | --- |
| `collectors.rs` | a mock HTTP server, plus a live Docker daemon when present | real requests and real parsing of the payload shapes Jenkins and Elasticsearch actually return |
| `databases.rs` | real PostgreSQL and MySQL | the collector falls back rather than erroring, so a wrong column name is a silently-zero metric, not a failure |
| `stores.rs` | real Redis and MongoDB | text and BSON whose shape varies by server version, and both degrade rather than error |
| `http_services.rs` | real Prometheus, RabbitMQ and Nginx | all three parse version-dependent payloads and read a missing key as a permanent zero |
| `kafka.rs` | a real broker | consumer lag needs a per-group offset fetch that only exists over the wire, and a group that never committed looks identical to one fully caught up |
| `kubernetes.rs` | a real cluster | whether the API shapes we ask for exist on a live server; a renamed field is a silently-empty section, never a compile error |
| `promquery.rs` | wiremock always, a real Prometheus when asked | wiremock proves the parser handles documented shapes; a server proves those shapes are what it sends |
| `gigapipe_read.rs` | wiremock, always | the nine store reads are entirely URLs, and `/label` against `/labels` or a missing `/json` fails by returning nothing — which looks exactly like a store holding nothing |
| `demo_estate.rs` | `demo/compose.yml`, wrong on purpose | the only layer that proves the pieces *meet* — see [The demo estate](#the-demo-estate) |

`tests/databases.rs` earns its keep twice over: writing it caught three genuine bugs —
MySQL rejecting bind parameters in `SHOW ... LIKE ?`, `rows` being reserved in MySQL 8,
and the event-scheduler daemon being counted as a permanently long-running query.

Every integration test except `demo_estate.rs` skips cleanly when its dependency is
absent, so `cargo test` passes on a bare machine. The 17 `#[ignore]`d tests want a real
dependency and are opted into explicitly; the `docker run` commands are in each file's
header.

### The demo estate

`demo/compose.yml` starts a small estate that is **wrong on purpose** — a Redis with
`maxmemory-policy noeviction` filling up, a Postgres at `max_connections=25` leaking
sessions, buckets with no lifecycle policy, a queue nobody reads — each misconfigured in
the specific way one of the rules exists to catch. One service is configured correctly and
root must stay quiet about it, because an estate where everything is flagged teaches the
same wrong lesson as one where nothing is.

```bash
docker compose -f demo/compose.yml up -d
cargo test --test demo_estate -- --ignored --nocapture
```

Those tests **fail rather than skip** when a service is missing: the entire point is that
the assertions ran against something real. It is the only layer that catches a whole class
of bug — every unit test and every scenario builds its own snapshot by hand, so a rule can
read a metric key no collector emits and still pass everything. Two rules were doing
exactly that when this estate was first pointed at them. See
[`demo/README.md`](demo/README.md).

---

## Adding a connection

Settings → Add connection. Use **Test connection** before saving; it runs a real
collection cycle and reports what came back.

| Type | Endpoint | Credential |
| --- | --- | --- |
| Docker | empty for the local socket, or `tcp://host:2375` | none |
| Podman | empty for the local socket — rootless, then rootful, then a `podman machine` on macOS | none |
| Kubernetes | empty for the current context, or a context name such as `docker-desktop` | none; kubeconfig already carries it |
| Linux host | `web-1.internal` — a node_exporter listener | optional username + password |
| NFS exports | `build-1.internal` — node_exporter on a client that mounts the exports | optional username + password |
| HTTP endpoint | `https://api.internal/health` | optional password or bearer token |
| Nginx | `http://nginx.internal/nginx_status` — `stub_status` is off by default | optional username + password |
| Apache httpd | `http://apache.internal/server-status?auto` | optional username + password |
| Apache Tomcat | `http://tomcat.internal:8080` — root appends `/manager/jmxproxy` itself | username + password holding the `manager-jmx` role |
| HAProxy | `http://haproxy.internal:8404/stats` | optional username + password |
| Traefik | `http://traefik.internal:8080` | optional username + password |
| PostgreSQL | `postgres://host:5432/dbname` | username + password |
| MySQL | `mysql://host:3306/dbname` | username + password |
| MongoDB | `mongodb://mongo.internal:27017` | optional username + password |
| ClickHouse | `http://clickhouse.internal:8123` — the HTTP port, not the native 9000 | username + password |
| Redis | `redis://cache.internal:6379` | password; username only for Redis 6+ ACL users |
| Memcached | `cache.internal:11211` — port 11211 assumed if omitted | none |
| Elasticsearch | `https://es.internal:9200` | username + password, or leave the username blank to send the secret as an API key |
| Kafka | `broker1:9092,broker2:9092` — comma-separated, no scheme | none for PLAINTEXT; SASL username + password once the security protocol is a SASL one |
| RabbitMQ | `http://rabbit.internal:15672` — the management plugin's port, not AMQP | username + password |
| NATS | `http://nats.internal:8222` — the monitoring port | optional username + password |
| ZooKeeper | `zk-1.internal:2181` | none |
| Consul | `http://consul.internal:8500` — any agent answers for the whole cluster | optional ACL token |
| etcd | `http://etcd-1.internal:2379` — a client port, read via `/metrics` | optional username + password |
| Jenkins | `https://jenkins.internal` | username + API token |
| Prometheus | `http://prometheus.internal:9090` | optional password or bearer token |
| Gigapipe | `http://gigapipe.internal:3100` | optional password or bearer token |
| AWS | empty for real AWS, or a URL for an S3/SQS-compatible service | access key id + secret, or leave both empty to use the default credential chain |

Every type also takes **max log lines per poll**, and every TLS one takes **skip TLS
verification**, both under Advanced. Beyond those, a few options change what a connection
actually watches:

- **Elasticsearch** — set a **log index pattern** (e.g. `logs-*`) to tail documents; leave
  it empty to monitor cluster health only.
- **Kubernetes** — **namespace** defaults to all of them, and **kubeconfig path** is only
  needed when the file is not in the default location.
- **AWS** — **region** defaults to `us-east-1`, and one connection covers one region.
- **Jenkins** — **job filter** narrows to jobs containing a string; empty watches every job
  the credential can see.
- **Gigapipe** — a **LogQL selector** turns the connection into a log source for whatever
  ships into it, and a **TraceQL selector** with a slow-trace threshold decides which
  traces are listed.
- **NFS exports** and **RabbitMQ** — **path prefix** and **virtual host** narrow a busy
  host to the exports or vhost you care about.

Warning thresholds (disk fill, queue depth, eviction rate, consumer lag, and the rest)
are per-connection options on the collectors that have them, so one noisy host does not
have to set the bar for the estate.

---

## Layout

```
src-tauri/src/
  collectors/     Collector trait + 26 modules covering all 28 service kinds
                  (docker serves Podman, database serves PostgreSQL and MySQL),
                  plus promtext, the shared Prometheus text-format parser
  llm/            LlmProvider trait + anthropic, openai, ollama
  agent/          Tool catalog, dispatch, confirmation gate, the agent loop
  scenarios/      library.json — the known-situation cases themselves

  model.rs        Domain types, serialized straight to the frontend as camelCase
  registry.rs     The service catalog — what the UI renders without knowing kinds
  state.rs        Shared state, log ring buffer, alert store + alert engine
  supervisor.rs   Per-connection polling tasks
  commands.rs     The Tauri command surface
  config.rs       Persisted connection profiles and assistant settings
  secrets.rs      OS keychain wrapper
  error.rs        AppError, serialized to a string across the Tauri boundary

  history.rs      Rolling metric history and log bucketing — the data behind charts
  traces.rs       Spans as a tree — the one shape Section/Row cannot express
  changes.rs      Deploy detection by diffing each component's revision column
  ingest.rs       Opt-in loopback listener for externally reported deploys
  incidents.rs    Correlating alerts into incidents, without a model
  topology.rs     The declared dependency graph, and what follows from it
  patterns.rs     Log clustering with Drain — ten thousand lines to a few templates
  stats.rs        KS test + volume heuristic for ranking which metrics moved
  forecast.rs     Where a metric is heading, and when to refuse to say
  promquery.rs    Long-range history read from a Prometheus you already run

  configsource.rs Where a connection's config lives, and reading it safely
  redact.rs       Removing secrets from text before it can leave the process
  advisor.rs      Config read against telemetry, producing actionable findings
  identify.rs     What a container is actually running, from its image name
  runtime.rs      Declared placement, reconciled against root's own inference

  journal.rs      The judgement record — what root said, and whether it was right
  claims.rs       The assistant's findings, structured so they can be scored
  backtest.rs     Today's rules replayed over yesterday's Prometheus data
  scenarios.rs    Known situations with known right answers
  team.rs         Shared workspaces, against a root server you run yourself

src/
  App.tsx         Runtime: polling, event subscriptions, routing
  nav.ts          The View union — every destination in the app
  types.ts        Mirrors the Rust serde representations
  api.ts          Typed invoke wrappers
  model.ts        Pure read model — entities, issues, groups, counts
  incident.ts     Reading a backend incident — home state, and how one presents
  explain.ts      What the operator clicked, and the sentence that explains it
  catalog.ts      Service descriptors and the colour rules
  ranges.ts       What the metric ring can actually answer
  format.ts       Value formatting
  density.ts      The compact/comfortable switch
  theme.ts        Dark/light/system
  events.ts       Event subscription that no-ops outside Tauri
  fixtures.ts     Development fixtures for preview.html, and the isTauri gate
  app.css         Tailwind v4 theme: tokens, density, base
  shell/          Sidebar, ScreenHeader, CommandPalette, AssistantDock,
                  ComponentDialog, ConfirmDialog, Splash, Toasts
  screens/        One file per destination, incl. TraceDetail's waterfall,
                  plus the Onboarding and SignIn gates
  ui/             Primitives, charts, icons, generic section table
  icons/marks/    Brand marks, auto-discovered per kind
```

The backend owns everything stateful; the UI is a view over it, driven by Tauri events
(`monitor://snapshot`, `monitor://logs`, `monitor://alerts`, `monitor://changes`,
`agent://step`, `agent://confirm-request`).

Operational metadata (team, responder, environment, criticality, contact, and
runbook) is stored in each connection's open `metadata.operations` envelope.
It is intentionally separate from collector options, so adding an operating
model or integration never couples it to how a collector gathers data.
