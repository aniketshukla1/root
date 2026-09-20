# Running root as a server

```bash
cd deploy
cp .env.example .env          # put a real token in it
docker compose up -d
open http://localhost:7777
```

The first person to open it claims the server and becomes its administrator.
Every account after that is created by one.

## What you get

Two containers. `server` runs the engine — the same collectors, rules and
commands the desktop app runs — and serves the client. `db` is PostgreSQL with
TimescaleDB, and is where everything the engine learns is kept.

Nothing else is required. No broker, no cache, no sidecar: the engine is one
process that polls, and the database is one place it writes.

## Configuration

| Variable | | |
| --- | --- | --- |
| `ROOT_TOKEN` | **required** | The shared token, for scripts and scrapes. No default, and the server exits rather than starting without one. `openssl rand -hex 32` |
| `POSTGRES_PASSWORD` | **required** | Only reachable from inside the compose network. |
| `ROOT_PORT` | `7777` | |
| `RUST_LOG` | `info` | |

Two things are deliberately absent.

**The database is not published.** Nothing outside the compose network needs
it, and a Postgres on a public port is the most common way an estate's history
becomes somebody else's.

**There is no default token.** A compose file that shipped a working one would
put the same token on every deployment of this that anyone ever ran.

## Where your data is

| | |
| --- | --- |
| Connections | the `config` volume, as `config.json` — the same file the desktop app writes |
| Alerts, incidents, deploys, verdicts | Postgres, kept for years |
| Metrics | Postgres. Raw for 30 days, hourly rollups forever |
| Logs | Postgres, 7 days, compressed after 7 |

The hourly rollup is the row that outlives its own samples, and it is what
makes a forecast mean something — root's in-memory buffer only ever held a few
hours.

TimescaleDB does the rollup, the compression and the expiry. Plain PostgreSQL
works too: the extension statements are optional by design, and without them
you get the same answers without compression or automatic rollup.

## Reaching your infrastructure

The server polls from inside a container, so `localhost` means the container.
`host.docker.internal` is mapped for you and reaches the host. For anything
else, put root on the same network as the things it watches.

## Upgrading

```bash
docker compose pull && docker compose up -d --build
```

The schema is applied on every start and is additive, so a newer server reads
an older database.
