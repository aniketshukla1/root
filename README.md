<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="website/media/logo-dark.svg">
    <img src="website/media/logo-light.svg" alt="" width="64" height="64">
  </picture>
</p>

<h1 align="center">root</h1>

<p align="center"><b>Monitoring tells you something is wrong. root tells you why.</b></p>

<p align="center">
  Live health, metric history, one log stream, alerts and an assistant that can actually investigate,<br>
  across 31 kinds of infrastructure, from one window. Open source. Desktop or server.
</p>

<p align="center">
  <a href="https://aniketshukla1.github.io/root/"><b>Website</b></a> ·
  <a href="#the-films"><b>Films</b></a> ·
  <a href="#run-it"><b>Run it</b></a> ·
  <a href="docs/REFERENCE.md"><b>Reference</b></a>
</p>

<p align="center">
  <a href="https://github.com/aniketshukla1/root/actions/workflows/ci.yml"><img src="https://github.com/aniketshukla1/root/actions/workflows/ci.yml/badge.svg" alt="CI status"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-0a84ff" alt="MIT license"></a>
</p>

<p align="center">
  <a href="https://aniketshukla1.github.io/root/"><img src="website/media/root-hero.gif" alt="root: an incident on the Overview, its root cause analysis, then a config finding that says writes will start failing in about 3 hours" width="860"></a>
</p>

---

## Monitoring is half of it

Watching tells you a thing is wrong. Reading the configuration that governs it tells you why, and which number to change.

A Redis at 1.8 GB of a 2 GB `maxmemory` with `maxmemory-policy noeviction` is not a memory graph to keep an eye on. It is a decision that starts refusing writes in about three hours, and neither the metric nor the config file says so alone. root reads both, and says:

> **Writes will start failing in about 3 hours.** Memory is at 1.8 GB of a 2.0 GB ceiling and rising. `maxmemory-policy` is `noeviction`, so when it arrives Redis does not evict anything: it returns OOM on every write and keeps serving reads.
>
> ```
> redis-cli CONFIG SET maxmemory-policy allkeys-lru   # cache semantics: evict rather than refuse
> ```

## What it does

<table>
  <tr><td width="30%"><b>One incident, not a list of alerts</b></td><td>Alerts group by onset time, declared dependencies and observed changes, with no model involved. You see what failed first, what changed and how far it spread.</td></tr>
  <tr><td width="30%"><b>It reads your config</b></td><td>You declare the paths: on this machine, inside a Docker container, over SSH or inside a Kubernetes pod. Secrets are redacted in the reader, and findings come from rules that always fire on the same input.</td></tr>
  <tr><td width="30%"><b>An estate graph</b></td><td>Every connection, container, outside service, team and file as one graph you can walk. Every link says how root knows it: declared, reported, observed or inferred.</td></tr>
  <tr><td width="30%"><b>Grafana dashboards, imported</b></td><td>Paste an export and its PromQL panels run. Explore PromQL, LogQL and traces. Alert rules replay the last week before you save them.</td></tr>
  <tr><td width="30%"><b>An assistant with real access and deliberate limits</b></td><td>Read-only by default. With actions allowed, every call stops for approval. No shell tool and no arbitrary SQL. Bring Anthropic, any OpenAI-compatible endpoint, or Ollama, where nothing leaves the machine.</td></tr>
  <tr><td width="30%"><b>It keeps score</b></td><td>A Scorecard of which findings were right, which were noise and which problems it missed.</td></tr>
</table>

## The films

Each one is the real app, filmed with its built-in demo data. Click through to watch with sound.

<table>
  <tr>
    <td width="50%"><a href="https://aniketshukla1.github.io/root/#top"><img src="website/media/thumb-launch.jpg" alt="Watch the launch film"></a><br><b>The launch film</b> · 0:25<br>The whole idea, from the splash to the end card.</td>
    <td width="50%"><a href="https://aniketshukla1.github.io/root/#incident"><img src="website/media/thumb-incident.jpg" alt="Watch From first alert to root cause"></a><br><b>From first alert to root cause</b> · 0:34<br>Alerts, how it spread, the analysis, the assistant, the record.</td>
  </tr>
  <tr>
    <td><a href="https://aniketshukla1.github.io/root/#config"><img src="website/media/thumb-config.jpg" alt="Watch Monitoring is half of it"></a><br><b>Monitoring is half of it</b> · 0:32<br>A finding, the fix, the evidence, redaction, where files live.</td>
    <td><a href="https://aniketshukla1.github.io/root/#graph"><img src="website/media/thumb-estate-graph.jpg" alt="Watch What is this connected to?"></a><br><b>What is this connected to?</b> · 0:24<br>The service map and the estate graph.</td>
  </tr>
  <tr>
    <td><a href="https://aniketshukla1.github.io/root/#dashboards"><img src="website/media/thumb-dashboards.jpg" alt="Watch Bring your Grafana dashboards"></a><br><b>Bring your Grafana dashboards</b> · 0:28<br>Import, Explore, a trace waterfall, a rule replayed.</td>
    <td><a href="https://aniketshukla1.github.io/root/#assistant"><img src="website/media/thumb-ask.jpg" alt="Watch Select it. Ask about it."></a><br><b>Select it. Ask about it.</b> · 0:18<br>The answer sits on the chart window or log lines you asked about.</td>
  </tr>
</table>

## Run it

**Look at the interface in a browser, with demo data and no Rust.** It needs Node 22.

```bash
git clone https://github.com/aniketshukla1/root
cd root
npm install
npm run dev
# then open http://localhost:1420/?estate=incident
```

**The desktop app.** Nothing to configure and nobody to sign in to. It reads a config file you own, holds credentials in the OS keychain and binds loopback only. It needs Node 22, a Rust toolchain and the [Tauri 2 prerequisites](https://tauri.app/start/prerequisites/), plus CMake for the bundled Kafka client and, on Linux, curl's headers (`libcurl4-openssl-dev`).

```bash
npm install
npm run tauri dev
```

Nothing to connect yet? Start the **live demo** from the first-run screen: a simulated shop of thirteen connections, where every twelve minutes something goes wrong.

**The server.** Collectors run somewhere that stays up, history goes to Postgres, and the client is a browser on any device, with accounts, roles and single sign-on. It refuses to start without a token. See [`deploy/README.md`](deploy/README.md).

```bash
cd deploy
cp .env.example .env   # put real values in it
docker compose up -d
open http://localhost:7777
```

## 31 kinds of infrastructure

<table>
  <tr><td width="30%"><b>Containers and orchestration</b></td><td>Docker, Podman, Kubernetes</td></tr>
  <tr><td width="30%"><b>Hosts and storage</b></td><td>This computer, Linux host, SSH server, NFS exports</td></tr>
  <tr><td width="30%"><b>Web and endpoints</b></td><td>HTTP endpoint, Nginx, Apache httpd, Apache Tomcat, HAProxy, Traefik</td></tr>
  <tr><td width="30%"><b>Databases</b></td><td>PostgreSQL, MySQL, MongoDB, ClickHouse</td></tr>
  <tr><td width="30%"><b>Caching and search</b></td><td>Redis, Memcached, Elasticsearch</td></tr>
  <tr><td width="30%"><b>Messaging and coordination</b></td><td>Kafka, RabbitMQ, NATS, ZooKeeper, Consul, etcd</td></tr>
  <tr><td width="30%"><b>CI, observability and cloud</b></td><td>Jenkins, Prometheus, Gigapipe, OpenTelemetry, AWS</td></tr>
</table>

Adding a kind is a backend-only change: the frontend knows about no service type in particular. [How to add one](docs/REFERENCE.md#adding-a-service-type).

## How it fits together

```mermaid
flowchart LR
  estate["Your estate<br/>31 kinds of system"] -->|polled, read-only| engine
  files["Declared config and log files<br/>local, container, SSH, pod"] -->|redacted in the reader| engine
  otlp["Apps, over OTLP"] --> engine
  subgraph engine["The engine (Rust)"]
    direction TB
    rules["Alerts, incidents, config rules, forecasts"]
    knows["Estate graph, history, logs, traces"]
  end
  engine --> desktop["Desktop app<br/>Tauri + React"]
  engine --> server["Server<br/>browser client, Postgres"]
  engine --> mcp["MCP server"]
  engine <-->|fixed tools| ai["Assistant<br/>your model, or none"]
```

The desktop app and the server run the same engine. Everything works with no model configured; the assistant adds to it.

## Status

Version 0.1, built from source: there are no signed installers yet. The unit and integration suites run in CI, many against real services ([list](docs/REFERENCE.md#tests)). Reading files inside a container, inside a pod and from a journal over SSH have tests but have not yet been exercised against a live engine or cluster, so reports are welcome.

## More

- [The reference](docs/REFERENCE.md): every collector, the assistant's limits, the MCP server, credentials, the layout
- [Frontend architecture](FRONTEND_ARCHITECTURE.md), [Gigapipe](GIGAPIPE.md), [Releasing](RELEASING.md)
- [Contributing](CONTRIBUTING.md) and [Security](SECURITY.md)

## License

[MIT](LICENSE)
