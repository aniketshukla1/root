# Frontend architecture

root is an infrastructure monitoring console. The frontend is a view over the
backend's state: it holds no domain state of its own, and it knows about no
service type in particular.

## Navigation

Seven destinations, flat. Ownership, category, and environment are *properties*
of a connection — they are columns, filters, and group-by keys, not screens.

| Screen | Answers |
| --- | --- |
| **Overview** | What is the state of everything, right now? |
| **Services** | Which connections exist, and which need attention? |
| **Alerts** | What has fired, what is unhandled, what recovered? |
| **Metrics** | How has a measure moved, across connections? |
| **Logs** | What was written, and when did the rate change? |
| **Assistant** | Investigate across all of the above. |
| **Settings** | Connections, credentials, ownership, model, display. |

Three drill-downs are reachable from several screens and are deliberately not in
the nav: **service detail**
(`Dashboard · Components · Alerts · Metrics · Logs · Configuration`),
**component detail** — one container, node, or job — and **correlate**, one
moment across every connection, from any log line or alert.

### Three levels

```
Services  →  prod-docker  →  checkout-api
             logs-cluster →  es-data-1
```

A component page carries every cell the collector reported, the logs and alerts
naming it, its row actions, and a strip of its siblings so you can move sideways
without going back up. The identifying cell in any section table is the drill-in;
the whole row is not clickable, because that would swallow the action buttons at
its other end.

### The service dashboard

A connection opens on its **Dashboard** tab: one scrolling canvas — signals,
time series, log rate, components — where every panel shares a single `domain`.
The other tabs remain for the full filtered and sortable tables.

What makes it a dashboard rather than a stack of cards is that the panels agree
on time:

- **One time domain.** `TimeSeries` and `LogHistogram` position marks by
  timestamp against an explicit `[from, to]`, never by array index. Index
  positioning stretches a short series across the full width, so two panels
  silently disagree about where a moment is — invisible on an isolated card and
  wrong on a page you read downward.
- **One crosshair.** `hoverTime` is controlled by the dashboard and passed to
  every panel, so hovering any chart reads every chart at that instant.
- **Annotations.** Alerts (`firstSeen`) and deploys (`ChangeEvent.at`) are drawn
  as markers on every chart. They wear deliberately different colours: alerts
  take their status hue because something is wrong, deploys take the interaction
  accent because they are neutral facts. "The error rate rose right after that
  deploy" is the sentence the whole feature exists to make readable.
- **Brush to zoom** narrows the shared domain. It only changes the view — the
  fetch window is unchanged, and the error tail below re-filters to match.

A histogram bar is sized by the time it *covers*, not by `1 / bucketCount`.
Sizing by count looks correct at the default window and collapses to hairlines
with gaps the moment the domain is narrower than the data.

### Per-connection first, cross-connection on request

With a dozen collectors reporting at once, a single undifferentiated feed is
unreadable. But deleting the cross-connection view would throw away the thing
this backend is actually for — seeing a Postgres lock wait and an Nginx 502 as
one event. So every global screen defaults to **grouped by connection** and
keeps the mixed view as an explicit mode:

| Screen | Default | Other mode |
| --- | --- | --- |
| **Alerts** | grouped by connection | severity · team · flat |
| **Metrics** | one panel per connection, its own metrics | **Compare** — one chart per metric, a line per connection |
| **Logs** | merged, with a connection rail and identity stripes | **Split** — a separate lane per connection |

The Logs rail multi-selects with live per-connection line and error counts.
Selecting nothing means *all*, so a newly added connection appears without
anyone opting it in, and the first click on a row narrows to that connection
rather than removing it — "show me this" is overwhelmingly the intent.

## Layers

```text
Tauri events + commands
        │
        ▼
App.tsx ─── runtime only
  polling, event subscriptions, bounded client buffers, routing
        │
        ├──────────► catalog.ts    what service kinds exist, and their colours
        │
        ▼
model.ts ─── pure projection
  ownership + alerts + snapshots → entities, issues, groups, counts
        │
        ▼
screens/ ─── one file per destination
        │
        ▼
ui/ ─── primitives, charts, the generic section table
```

The runtime owns transport. Screens never subscribe to backend events and never
reinterpret collector payloads. `model.ts` imports no React and no Tauri, so its
severity ranking, alert/row merge, and grouping are testable on their own.

```text
src/
  App.tsx          runtime composition and routing
  nav.ts           the View union
  types.ts         mirrors the Rust serde representations   ← backend contract
  api.ts           typed invoke wrappers                     ← backend contract
  model.ts         pure cross-connection read model
  catalog.ts       service descriptors + the colour rules
  ranges.ts        what the metric ring can actually answer
  format.ts        value formatting
  density.ts       the compact/comfortable switch
  app.css          Tailwind v4 theme: tokens, density, base
  shell/           Sidebar, ScreenHeader, CommandPalette, Toasts, ConfirmDialog
  screens/         Overview, Services, Alerts, Metrics, Logs, ServiceDetail,
                   ServiceDashboard, Correlate, Assistant, Settings
  ui/              primitives, charts, icons, SectionTable, AlertList
  icons/marks/     brand marks, auto-discovered per kind
```

## The extension contract

`ServiceDescriptor` is the plugin contract for infrastructure types. A collector
declares its label, monogram, category, configuration fields, capabilities, and
typed inventory sections; the UI renders whatever it is handed.

When a new collector appears, with no frontend change:

- its category becomes a group in Services and the connection picker;
- its connection joins the ownership, alert, and count projections;
- failing rows become issues;
- it gets its own Metrics panel, its own Logs lane and rail entry, and its own
  Alerts group — the per-connection views are driven by the model, not by a
  per-kind registration;
- its metrics also join the Compare view, grouped by metric key;
- its sections render through `ui/SectionTable.tsx`.

Two things make this hold rather than being a convention:

- `ServiceKind` is an open `string`, never a union. Do not narrow it.
- Every catalog lookup degrades: an unknown kind gets a synthesized descriptor
  with a derived label, monogram, and a stable colour.

Operational context lives in each `ConnectionProfile`'s open `metadata`
envelope, separate from collector `options`. The `operations` namespace holds
`team`, `owner`, `contact`, `environment`, `criticality`, and `runbookUrl`. It is
open by design so cost allocation, CMDB IDs, or paging integrations can be added
without migrating the profile shape.

## Read-model rules

- Health is the worst trustworthy level. A healthy snapshot cannot hide a down row.
- Alerts and failing rows are merged when they describe the same entity; a
  service-level alert with no failing row stays a separate signal.
- Issue priority is severity, then unacknowledged, then criticality, then age.
- Acknowledgement is not resolution. Acked work stays listed, de-emphasised,
  until the collector reports recovery.
- Ownership is never guessed. Missing ownership renders as **Unassigned** and
  lowers the coverage figure.
- Config failure must not block observability — snapshots still render.

## Visual rules

**Colour has exactly three jobs, and they never mix.**

1. **Status** (`--color-ok / warn / serious / crit`) is reserved. Nothing else may
   use those hues, so a colour that looks like an alarm always is one.
2. **Identity** (`--color-series-1..8`) marks which service or series something
   is. `markColor()` gives a kind a *stable* colour; `seriesColor(index)` hands
   chart slots out in fixed order to the series actually plotted, so filtering
   one out never repaints the rest. The two never share a code path.
3. **Everything else is neutral ink.** Text never wears a series colour.

   A chart that plots **one** series is in that third group, not the second: it
   draws in `DATA_INK` (`--color-data`). Identity on a single-series chart buys
   nothing — the panel title, badge and name already say whose line it is — and
   it costs something real, because three of the eight identity hues sit beside
   a status hue. A healthy MongoDB connection used to draw a red sparkline next
   to the word "Healthy".

Both palettes are validated against the panel surface (`#101317`) for the dark
lightness band, chroma floor, protanopia/deuteranopia/tritanopia separation, the
normal-vision floor, and 3:1 contrast. **Re-run the validator before changing any
value in the `@theme` block** — those hexes are computed, not chosen.

**Every other value is a token too.** The colour rules above were followed from
the start; type, spacing, corners and motion were not, and drifted into eleven
display sizes, three dead density tokens and five spellings of a 3px corner.
They are now one scale each:

| Axis | Tokens | Rule |
| --- | --- | --- |
| Type — chrome | `2xs` 10.5 · `xs` 11 · `sm` 12 · `base` 13 | labels, captions, table cells |
| Type — display | `lg` 15 · `xl` 17 · `2xl` 19 · `3xl` 22 · `4xl` 25 · `5xl` 34 · `6xl` 44 | figures and headings |
| Density | `--row-h`, `--ui-size`, `--mono-size`, `--pad-x`, `--panel-pad`, `--gutter` | reached through `.panel-pad`, `.cell-pad-x`, `.screen-pad` — never re-hardcoded |
| Corners | `xs` 1 (marks) · `sm` 2 · `md` 3 (everything else) | bare `rounded` is Tailwind's 4px and is not ours |
| Motion | `--dur-fast`, `--dur-base`, `--ease-ui` for states; `ui/motion.ts` for entrances | |

An arbitrary value (`text-[18px]`, `p-[7px]`) is a bug unless it carries a
comment saying why — the splash wordmark is the one that does.

**Components own their states.** `Button` has four variants, three sizes and a
`loading` that keeps its width; `Input` has `invalid`; `Field` binds the error
to the control; `Chip` is the pressable sibling of `Tag`; `Stat` is the only
place a figure is typeset. Reaching past them — a raw `<button>` with a height
in `className`, a hand-built pill — is how the system erodes, and how six
control heights and seven pill spellings happened.

Other rules:

- **A role is a promise.** `role="tab"` means arrow keys and an `aria-controls`
  panel; a filter that has neither is a `radiogroup`, which is what `Segmented`
  now is. Anything that appears unasked — a toast — carries `aria-live`.
- **Status is never colour alone.** Every health signal carries a distinct
  silhouette (circle / triangle / square / ring) and, wherever there is room, the
  word. Dense grids that omit the word carry it in `title`.
- **One y-axis per chart.** Two units means two charts. `Metrics` groups series
  by `key:unit` for exactly this reason.
- **Uppercase is for table column headers only** (`.label`). Headings are
  sentence case, and an identifier is never uppercased at all — a connection
  shown as `ORDERS-API` is a different string from the one in every other
  column.
- **The quiet inks clear 4.5:1 on the lightest surface they print on**, not the
  darkest: `--color-ink-4` is used on `raised` and `overlay` as well as `panel`.
- **`--text-2xs` is for labels and captions.** Anything that is a sentence takes
  `--text-xs` or larger.
- **A healthy cell is a ring, not a fill** (12% fill / 45% ring, ≥3:1 against
  its own ground). A page of healthy components must not out-shout the few that
  are not.
- Thin marks, recessive grid, 2px surface gaps between stacked segments, and a
  hover layer on everything that plots.

## Where the model may speak unasked

Exactly one place: the component tooltip, and only over a component that is
already failing. Three rules make that affordable and checkable.

- **A hover is not a request.** Nothing is sent until the tooltip has been open
  on the same component for 550ms. Dragging across one block touches a dozen
  cells in a second, and there are 364 of them.
- **A reading is computed once per state.** The backend caches by the
  component's health, summary and alert, so the same broken container asked
  about twice is one call; the frontend keeps the answer for the session, so a
  re-hover does not even make the round trip.
- **The facts travel with the text.** `ComponentReading.grounds` is every line
  the model was given, and the tooltip says how many there were. A generated
  sentence that cannot be checked against its inputs is the one thing this UI
  will not show — see the evidence panel, which exists for the same reason.

`explain_component` is deliberately not `send_message`: no tools, no
conversation, 160 tokens. A healthy component is refused rather than answered,
because a model asked what is wrong with something that is fine will find
something.

## Time ranges are bounded by samples, not duration

`src-tauri/src/history.rs` keeps a fixed **`MAX_POINTS = 720`** per metric — not
a fixed duration. How far back a connection reads therefore depends entirely on
how often it polls: three hours at 15s, twelve at 60s.

A fixed 15m/1h/6h/24h control ignores that, and the failure is silent — asking
for 24h over a three-hour ring returns the three hours with no error, so two
buttons draw the same chart under different labels. `src/ranges.ts` owns this:

- `rangesFor(pollSecs)` offers only windows the ring can satisfy;
- `rangesAcross(...)` does the same for a screen spanning several connections;
- `spanOf(series)` / `describeSpan(...)` report what is *actually* on screen, which
  the dashboard prints beside the range control ("120 samples · 29m of history").

**Keep `MAX_POINTS` in `ranges.ts` in step with the Rust constant.** Raising it
there without raising it here just re-introduces the silent version.

Longer windows need real retention work in Rust — a bigger ring, or downsampled
rollups so 24h is answerable without holding every sample.

## Component history: tracked vs live

Per-component history used to be refused outright, and the comment in
`history.rs` said why: containers and jobs come and go, so the cardinality is
unbounded and the series would be mostly holes. That objection is now answered
rather than avoided.

**Collectors opt a section in** with `Section::tracked()`, the same shape as
`ColumnSpec::revision()`. Only sections whose entities are *bounded* and
*long-lived* qualify:

| Tracked | Not tracked |
| --- | --- |
| Docker containers | Elasticsearch indices (hundreds) |
| Elasticsearch nodes | Kubernetes pods (thousands, replaced constantly) |
| Jenkins jobs, agents | Jenkins build queue (transient) |
| Kubernetes nodes, workloads | Database active queries (seconds-lived) |
| RabbitMQ queues, nodes; Kafka brokers, groups; Redis keyspace | Redis slowlog |

Three guards make that safe:

- **Only numeric cells** are recorded — `metric` and `gauge` kinds. A state
  string or tag list has no series.
- **`MAX_TRACKED_ENTITIES`** caps admission per connection. Existing entities
  keep recording, so a cluster that outgrows the cap degrades by stopping at the
  edge rather than thrashing.
- **`EVICT_AFTER_MISSES`** drops an entity that stops appearing, which is what
  actually answers "containers come and go". A container that merely *restarts*
  keeps its series across the gap. A snapshot with no tracked sections at all —
  a failed collection — evicts nothing, because that is a gap in observation,
  not evidence the component is gone.

**Untracked components still get a page.** It shows every current value plus a
sparkline accumulated in the browser session while the page is open, labelled
`Live · this session only` with a `not retained` tag. That is the only honest
way to trend something that may not exist in ten seconds, and the page says so
rather than leaving the reader to infer it from a missing chart.

## Deploy detection

Changes follow the alert rule: **collectors express intent, the platform
derives.** A collector tags one column as its revision —

```rust
ColumnSpec::text("image", "Image").secondary().revision()   // docker
ColumnSpec::metric("build", "Build", MetricUnit::Count).revision()  // jenkins
```

— and `src-tauri/src/changes.rs` diffs that column between consecutive
snapshots. `supervisor` already replaces the stored snapshot on every poll, and
`HashMap::insert` hands back the one it replaced, so the previous state the
engine needs is free.

The engine knows nothing about Docker or Jenkins. **Tagging a column is the
entire opt-in** — a new collector gets deploy markers without writing any
detection code, and one that tags nothing simply has no markers.

Rules worth knowing:

- Only text and number cells count as revisions. Tagging a duration or gauge is
  a no-op rather than a source of phantom deploys every poll.
- The first poll emits nothing — there is no previous snapshot, and otherwise
  every container in the estate would report as a fresh deploy on startup.
- A row that is new emits `Appeared` only if its *section* already existed, so a
  collector that starts reporting a whole new section is not a wave of deploys.
- Ids are deterministic (`profile:section:row:revision`), so the same
  observation seen twice collapses instead of drawing two markers.

Changes are **not alerts**: nothing is wrong, so there is no severity and
nothing to acknowledge. They are surfaced on the service dashboard (markers plus
a Deploys section) and in the correlation view, where "what shipped just before
this?" is usually the first question.

### Reported vs observed

`ChangeEvent.source` separates the two ways a change can be known:

- `observed` — root watched a revision move and is certain it happened.
- `reported` — something external asserted it, via the local ingest endpoint.

They are not equally trustworthy: a pipeline that posts a deploy and then fails
still leaves a marker. The Deploys table tags reported ones so the difference is
visible when you are deciding what to believe.

### Local deploy ingest

`src-tauri/src/ingest.rs` is an axum listener that turns a POST into a
`ChangeEvent`. It opens a socket in an app holding infrastructure credentials,
so the defaults are narrow and are stated in the Settings panel rather than
buried here:

- **Disabled by default**; enabling it is an explicit act.
- **`127.0.0.1` only** — never `0.0.0.0`, regardless of firewall state.
- **Bearer token** minted on first enable, kept in the OS keychain, never
  returned by any read command, and compared with `subtle::ConstantTimeEq` so a
  wrong token cannot be recovered by timing the reply.
- **Auth is checked before the body is parsed**, so an unauthenticated caller
  learns nothing about the schema.
- **8 KB body cap**, every string trimmed and length-clamped, and `at` clamped
  to ±24h so a skewed pipeline clock cannot smear the timeline.
- **Unknown connections are rejected**, so a marker always belongs to something
  real.
- Nothing posted is ever interpreted as an instruction — it is data.

Because it is loopback-only, a cloud CI runner cannot reach a laptop. The poster
has to be on the machine (a deploy script, a git hook, a self-hosted runner) or
arrive through a tunnel the operator sets up. That is a deliberate trade, not an
oversight: the alternative is binding a credential-holding desktop app to the
network.

```
POST http://127.0.0.1:7717/v1/deploys
Authorization: Bearer <token>

{ "service": "checkout-api", "version": "1.9.0", "connection": "prod-docker",
  "previousVersion": "1.8.0", "at": "…", "note": "…" }
```

`connection` resolves by id or case-insensitive name; the last three are
optional. `GET /v1/health` answers without a token so a script can check
reachability.

## Density

`--row-h`, `--ui-size`, `--mono-size`, and padding all derive from
`<html data-density>`. Switching is one DOM write, not a re-render, and the
preference is read in `main.tsx` before first paint. Anything that repeats down
the page must read from these variables rather than hard-coding a height.

## Not yet rebuilt

The previous frontend had an optional Tier-2 registry (`src/views/<kind>/`) for
purpose-built per-kind views, auto-discovered by `import.meta.glob`. It was
removed with the rewrite. Every collector's data still renders in full through
the generic section tables — that is the guarantee the descriptor model exists to
provide — but one genuinely additive capability went with it: the **Jenkins build
console** (`api.getBuildConsole`), which read `Row.extra.builds` to fetch console
output for a failing build. Re-adding it means reintroducing an additive registry
and one view file; no existing file needs to change.
