# M5 — `oarfish-engine` and `oarfish-api`, an alarm that raises and reaches the board

**Status:** implemented, on branch `m5-engine`
**Date:** 2026-09-19
**Parent:** `docs/specs/2026-09-17-oarfish-design.md` — milestone M5 in §11, mechanism in §5.5, §5.7, §5.9, §5.10

---

## 1. What this milestone is for

§11 gives M5 the contract *an alarm raises, dedupes, clears, and reaches SSE*. That is
the first point at which oarfish does the thing it is named for: everything before it
has been turning bytes into templates and templates into judgements.

M5 is also where the question set stops being a test fixture. M4 took it as a
parameter and hashed it; here §5.5's six questions become the real policy, owned by
`oarfish-engine` as invariant 5 requires.

All six are asked, including `contextual`, whose only consumer is the check deferred to
M5.5. Asking it now is free — §5.5's whole point is that questions evaluate in parallel
— and it means M5.5 does not invalidate every verdict M5 cached by changing the question
set, which under M4 §4's key would re-judge the entire table.

## 2. Boundaries

The milestone as written in §11 spans §5.5 through §5.10 — two cold-path Jev passes and
a hot path. That is more than its own acceptance asks for, so it is split.

| In (M5) | Out (M5.5, before M6) |
|---|---|
| The real static question set (§5.5) | Merge review (§5.6) |
| Windows (§5.7) | The contextual check (§5.8) |
| The alarm state machine (§5.9) | `wake_someone`, and with it the page lane |
| SSE and the JSON surface (§5.10) | ntfy delivery — M6 |
| `alarms` keyspace, which M4 left unopened | `merges`, `corrections` |

**The page lane is computed but inert.** §5.9 routes on `wake_someone`, which comes from
the contextual check in M5.5. Until then the routing table is implemented as written and
can only ever reach dashboard-only. Nothing is lost by this: ntfy delivery is M6, so the
page lane has no delivery mechanism in M5 regardless, and M5.5 lands the value it routes
on before M6 needs it. The alternative — deriving a provisional lane from the static
verdict — means a second routing rule that exists only to be deleted, and §5.9's
thresholds tuned against a signal they are not for.

## 3. Structure

One engine task owns the windows, the state machine and the `DelayQueue`. The M3
pipeline sends it `(Event, Assignment)` over a channel; it publishes `AlarmChange` on a
`broadcast` channel that SSE handlers subscribe to.

A single owner is not a preference. §5.8 requires that "the state machine is the only
writer of open-alarm state", and §5.9 rules out a task per alarm by name. One owning
task gives both without locks, and keeps contention off the every-line path.

**The verdict lookup belongs to the engine, not the pipeline.** `Verdicts::verdict_for`
is synchronous and cheap, and gating is a decision-layer concern. The pipeline continues
to do mask → drain and nothing else.

## 4. The window

§5.7 asks for count, rate, distinct hosts, first-seen and a delta against the trailing
hour, per template, in memory. It does not say how, and the obvious implementation is
the wrong one: a `VecDeque` of timestamps is unbounded per template, which fails at the
100k lines/sec case CLAUDE.md calls a normal homelab failure.

**Bucketed counters instead.** The five-minute window is 30 buckets of 10 seconds; the
trailing hour is 60 buckets of a minute. Advancing the clock overwrites the buckets it
has moved past. Per line the cost is one index and one increment — O(1), with memory
fixed per template rather than proportional to traffic.

Distinct hosts is a `HashSet` per window, bounded by fleet size rather than event count.
A homelab has tens of hosts; a template seen on six machines is a different signal from
one seen six times on one, which is why §5.7 asks for it.

**The window table is bounded**, evicting templates idle for over an hour. Drain's table
is bounded for the same reason and it applies here with more force: 65_536 templates
times 90 buckets is memory nothing would ever reclaim, and a runaway container minting
new templates is the case the bound exists for.

## 5. Raising

§5.9 says "raise, dedupe against open alarms, suppress flapping, auto-clear after
silence" without ever saying what causes a raise. Three rules, in order:

1. **The verdict gates.** A template may alarm only if judged actionable above a
   severity floor. No verdict yet means no raise — M4's unjudged lane, unchanged.
2. **High severity bypasses the window.** A template judged high-severity raises on
   first sight. An EXT4 error must not wait for a second occurrence to be believed.
3. **Otherwise the window triggers**, on count or rate over threshold.

Starting values, all config, all to be tuned against real data in the spirit of §5.9:
the gate floor is `minor`, the bypass fires at `critical`, and the window triggers at 5
events in the 5-minute window, or at three times the baseline rate. They are written
down so the tests have something concrete to assert; they are not claims.

**The rate rule needs a real baseline, and "three times the trailing hour" is not one.**
An earlier draft of this section said exactly that, and it is wrong at the only moment
it matters: the first event of a template computes `1 >= 3 × (1/12)` against an assumed
full hour and raises immediately, turning the rate rule into "raise on first sight" for
every template regardless of severity. A count test caught it.

The rule therefore divides the long total by the template's *actual* history span,
capped at the hour, and refuses to fire until that span covers a full short window. A
young template is then never an infinite multiple of nothing, and steady traffic sits at
a ratio of exactly 1 at any history length rather than drifting with the assumed
denominator.

Rule 1 is what keeps the cold path load-bearing: a purely statistical trigger would page
on a noisy harmless template and stay silent on a single catastrophic one. Rule 2 is
what stops the window from delaying the cases that matter most. Rule 3 is what stops a
genuinely actionable but chatty template from opening an alarm every few minutes.

**The severity score becomes an X.733 severity here.** §5.5 asks `severity` as a score
0–3; §6 defines `Severity` as the X.733 subset the board renders. The mapping is the
engine's, and it is a rounding of the score: `3 -> critical`, `2 -> major`,
`1 -> minor`, `0 -> info`. `cleared` is not reachable from a score — it is a state the
machine assigns, never a judgement Jev makes.

**Dedupe keys on `(template_id, host)`.** An open alarm for that pair takes a `count`
bump and a timer reset, not a second alarm. This follows from §6's single `host` field
and §5.8 scoping correlation to the burst's host, while the window stays per template so
the fleet-wide view survives.

**Auto-clear** is one `DelayQueue` entry per open alarm, reset on each new event, firing
after silence.

**Flap suppression** is a cooldown after clear: a re-raise inside it updates the alarm
that just closed rather than opening a new one. Without it a template oscillating around
the threshold produces an alarm per cycle, which is precisely the 3am experience this
product exists to prevent.

## 6. Restart

Open alarms persist to the `alarms` keyspace. Windows do not — §5.7 says in memory, and
they rebuild from live traffic in minutes.

On boot the engine reloads open alarms and **re-arms their `DelayQueue` timers**. The
asymmetry is deliberate: a restart cannot lose an open alarm, but it can delay an
auto-clear by up to the silence interval. Losing an open alarm is a silent failure;
delaying a clear is visible, self-correcting and harmless.

## 7. The API

| Route | Job |
|---|---|
| `GET /api/alarms` | Open alarms as JSON — §8's server-rendered first paint |
| `GET /api/alarms/stream` | SSE over the engine's `broadcast` |
| `/*` | `tower-http` `ServeDir` over the built board |

**Broadcast lag is handled explicitly, because the failure is silent.** A lagging
receiver returns `Lagged(n)` and skips those messages. If a skipped message was a
`Cleared`, the alarm stays on the board forever — a phantom alarm, which is worse than
no board at all. On `Lagged` the stream emits a `resync` event and the board re-fetches
`/api/alarms`. That is the same path reconnect already uses, reused for the one case
where the stream is knowably incomplete.

**No replay buffer and no `Last-Event-ID`.** The open-alarm set is small, so re-fetching
is cheap and always correct, where a ring buffer would be another thing to get subtly
wrong.

**Keep-alive comments** on the stream: idle connections get killed by proxies, and a
board that silently stopped updating is indistinguishable from a quiet night.

`AlarmChange` is `Raised(Alarm)` | `Updated(Alarm)` | `Cleared(AlarmId)`, plus the
`Resync` marker below.

**It lives in `oarfish-core` and the engine re-exports it.** "Alongside `Alarm`" is not
a stylistic preference: `.cargo/config.toml` records that only `oarfish-core` may export
into `oarfish.ts`, because ts-rs's registry is process-local and `cargo test
--workspace` runs one binary per crate — a second exporting crate truncates what the
first wrote, nondeterministically by test order. Exporting `AlarmChange` from the engine
does exactly that, and the symptom is a bindings file that silently loses `Alarm` rather
than a failing test. With the type in core the bindings diff is purely additive.

It gets a `ts-rs` export alongside `Alarm`, so the island's event handling is
typed from one definition rather than a hand-written guess.

## 8. Daemon wiring

`main.rs` gains the engine task and the axum server behind `--api 127.0.0.1:4000`, which
is what the board's dev server already proxies `/api` to. Both join the existing
`TaskTracker` under the same `CancellationToken`, so shutdown stays one mechanism rather
than three.

## 9. Testing

**`tokio::time::pause()` throughout.** Five-minute windows, silence intervals and flap
cooldowns are untestable in real time. With a paused clock, auto-clear and flap
suppression become fast deterministic assertions, and a test can advance an hour to prove
window eviction.

**Never await a `DelayQueue` under `pause()` and then assert an alarm is still open.**
The idle runtime auto-advances virtual time to the next deadline, so parking on the
queue fast-forwards the clock past the very timer being asserted on, and the alarm
clears underneath the assertion. It reads exactly like `reset()` being broken, and it is
not. Tests drive expiry through a non-parking `expire_ready()` that fires only what is
already past its deadline; the production run loop still parks, which is correct there
and never happens under pause. The gotcha is documented on the method, because the next
person to hit it will also start by suspecting `reset()`.

| Property | Test |
|---|---|
| The gate holds | An unjudged template never raises, however hard it bursts |
| Severity bypasses | A high-severity template raises on first sight |
| The window triggers | A gated template raises only once count or rate breaches |
| Dedupe | A second event on the same `(template, host)` bumps `count`, opens nothing |
| Flap suppression | A re-raise inside the cooldown updates rather than re-raises |
| Auto-clear | Silence past the interval clears, and the change is published |
| Eviction | A template idle over an hour loses its window |
| Restart | Reloaded alarms have armed timers and still auto-clear |
| Lag | A forced `Lagged` emits `resync` rather than skipping a `Cleared` |
| **Acceptance** | One integration test: an alarm raises, dedupes, clears, and every transition arrives over SSE |

## 10. Deferred, deliberately

| Deferred | Until | Why |
|---|---|---|
| Contextual check, `wake_someone` | M5.5 | §2; the page lane has no delivery until M6 |
| Merge review | M5.5 | Same cold-path shape, same milestone |
| ntfy delivery | M6 | §11 |
| Window persistence | Unscheduled | §5.7 says in memory; rebuild is minutes |
| Operator corrections | M6 | Needs the board's feedback flow |

## 11. Changes to existing documents

- `docs/specs/2026-09-17-oarfish-design.md` §5.9 — record what causes a raise; the
  section currently describes every transition except the first.
- `docs/specs/2026-09-17-oarfish-design.md` §11 — M5 is split; add M5.5.
- `README.md` — tick M5 when §9's acceptance holds, and add M5.5 to the list.
