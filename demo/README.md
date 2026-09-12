# The demo estate

A small estate that is **wrong on purpose**, for exercising root against
something real.

```bash
docker compose -f demo/compose.yml up -d      # start
docker compose -f demo/compose.yml down -v    # stop and remove its data
```

## Why it exists

root's claim is that configuration read against telemetry says things neither
half says alone. That claim is untestable against healthy infrastructure: a
correctly-sized Redis produces no findings, and a demo of nothing happening
demonstrates nothing.

Every service here is misconfigured in a way that is realistic, common, and
specifically what one of root's rules exists to catch. Load generators keep the
metrics moving, because a forecast needs a trend and a fresh container has
none.

One service — `cache-healthy` — is configured correctly, and root must stay
quiet about it. An estate where everything is flagged teaches the same wrong
lesson as one where nothing is.

## What is wrong, and what should be said about it

| Service | Port | Wrong on purpose | Rule |
|---|---|---|---|
| `cache` | 6399 | `maxmemory-policy noeviction`, filling | `maxmemory-noeviction` |
| `cache-healthy` | 6398 | nothing | *must stay silent* |
| `db` | 5442 | `max_connections=25`, sessions leaking | `max-connections` |
| `proxy` | 8088 | nothing — proves `stub_status` parses | — |
| `queue` | 5673 / 15673 | nothing yet | — |
| `aws` (Floci) | 4566 | buckets with no lifecycle, a queue nobody reads | `bucket-no-lifecycle`, `queue-unattended`, `queue-no-dlq` |
| containers | — | `cache` has no memory limit | `container-no-memory-limit` |

Ports are deliberately unusual so this cannot collide with anything real on the
machine.

## Verifying it

```bash
cargo test --test demo_estate -- --ignored --nocapture
```

These are `#[ignore]`d because they need the estate up. They **fail rather than
skip** when a service is missing — the entire point is that the assertions ran
against something real, and a silent skip would let the suite go green having
proved nothing.

This is the only test layer that catches a whole class of bug. Every unit test
and every scenario builds its own snapshot by hand, so a rule can read a metric
key no collector emits and still pass everything. Two rules were doing exactly
that when this estate was first pointed at them:

- the Redis rule read `memory`; the collector emits `used_memory`
- the Postgres rule read `max_connections`; `pg_settings` nests it as
  `{value, source}`, so no bare key existed

Neither could ever have fired on a real instance.

## Registering it in root

The compose file only starts the services. To have root watch them, add
connections pointing at the ports above. Their credentials go in the OS
keychain the way any connection's do.

Every credential in `compose.yml` is a literal, obviously-fake development
value committed on purpose. Nothing here is a secret and nothing here should
ever be reused.

## The AWS part

`floci/floci` is a local AWS emulator — a LocalStack alternative that needs no
account and no auth token. The seeder creates two S3 buckets, two SQS queues
and a DynamoDB table, then keeps uploading objects and sending messages so the
resources have a shape rather than being empty.

Worth knowing what an emulator does and does not give you: **API surface, not
telemetry**. A fresh one has no history, no load and no drift, so the parts of
root that matter most have nothing to work on until something is actually using
the services. The seeder exists to be that something.

root's AWS collector reads S3 and SQS through their own APIs. It deliberately
does not read CloudWatch — that is where the interesting numbers live for
Lambda, RDS and DynamoDB, and it is a metered API where a careless poll loop is
a line item, so it wants its own design rather than being tacked on.

One measurement is missing because of that, and stays missing on purpose. The
number you want for a queue is the age of its oldest message, and SQS exposes
it only through CloudWatch. The obvious workaround — peek at the head with
`ReceiveMessage` — increments every message's receive count even at a
visibility timeout of zero, so a queue with `maxReceiveCount: 3` polled every
fifteen seconds would have its entire contents in the dead-letter queue inside
a minute. A monitor may not change the thing it observes. root reports messages
waiting with nothing in flight instead, which costs nothing and says the same
thing about whether a consumer is running.
