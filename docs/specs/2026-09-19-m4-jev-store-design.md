# M4 — `oarfish-jev` and `oarfish-store`, judged once and cached

**Status:** accepted, pre-implementation
**Date:** 2026-09-19
**Parent:** `docs/specs/2026-09-17-oarfish-design.md` — milestone M4 in §11, mechanism in §5.11 and §7

---

## 1. What this milestone is for

§11 gives M4 the contract *a template is judged once and cached, proven against
wiremock*. That sentence contains the whole cost model: the expensive thing happens
per new template and never again, and the cheap thing happens per line.

The gate in §5.11 has already run and passed. Its findings are recorded there; three
of them constrain this design and are carried through below — confidence is not
derivable from the probabilities, the model pin does not pin, and the endpoint is
`alpha`.

## 2. Boundaries

| In | Out |
|---|---|
| `oarfish-jev`: one call against the decisions endpoint | Streaming, batching, any second endpoint |
| `Verdict` in `oarfish-core`, `ts-rs` exported | Rendering it — the template panel is M6 |
| `verdicts` and `records` keyspaces | `merges`, `alarms`, `corrections` |
| `Verdicts`: cache-aside over moka + fjall | The alarm engine, windows, routing |
| The off-path `Judge` task | Anything deciding *which* questions to ask |

The question set itself is policy and belongs to `oarfish-engine` in M5. M4 takes it as
a parameter, hashes it, and keys on the hash. The tests supply their own.

## 3. Orchestration lives in `oarfish-store`

`Verdicts` holds the moka cache, the fjall keyspaces and a jev client, and exposes the
cache-aside flow. This puts the policy next to the cache it manages, and makes M4
provably complete on its own rather than waiting for M5 to join two libraries.

The cost is a dependency edge from `oarfish-store` to `oarfish-jev`. That is a chain
where the rest of the workspace is a star, and it is accepted deliberately: the
alternative is opening `oarfish-engine` a milestone early, where the boundary of "just
the caching part" would blur under the first thing that needed it. Invariant 1 is
untouched — `oarfish-core` still depends on no workspace crate — and invariant 5 is
untouched, because `oarfish-jev` still holds no policy about which questions to ask.

## 4. The verdict key

§7 keys `verdicts` by `TemplateId` alone and caches "forever". That is wrong in two
directions: a verdict is only valid for the question set that produced it, and only for
the model that answered. The gate proved the second concretely — a request for
`typesafe/jev-1.13` was answered by `typesafe/jev-1.13-20260917`.

The key is therefore three components, fixed-width parts leading:

```
template_id (32B) ++ questions_hash (16B) ++ resolved_model_id (variable)
```

`questions_hash` is blake3 over the canonicalised question set, truncated to 16 bytes — the discipline
`bundle_hash` already gives masking. A reworded question misses the cache and re-judges,
rather than silently serving an answer to a question nobody asks any more.

The ordering is load-bearing. With the fixed-width components first, a prefix scan on
`template_id` returns every verdict that template has ever received, across question
revisions and model builds. "Why did this wake me three weeks ago" is then answerable at
the template level, not only by finding the individual decision record.

Re-judging on a question change costs roughly two cents per thousand templates, from the
gate's measured $0.0000205 per call. Affordable enough that correctness wins.

## 5. Cache miss, and the every-line path

Invariant 2 forbids a network call on the every-line path, so `Verdicts::verdict_for` is
synchronous and returns `Option<Verdict>` rather than a `Result`:

1. moka hit — return it.
2. moka miss — read fjall, populate moka, return it.
3. fjall miss — `try_send` the template to the `Judge` and return `None` **immediately**.

A fjall read error is logged and treated as a miss. The every-line path has no useful
response to a storage fault, and degrading to "unjudged, so dashboard-only" is the same
safe lane a genuine miss takes.

A fjall read does occur on the every-line path for a template this process has not seen.
That is within invariant 2, which forbids the network and not the disk, and it happens
once per template per process — exactly the cost moka is positioned to absorb.

The moka layer is bounded by entry count, defaulting to the same 65_536 as the Drain
table: both are sized by "templates a homelab has", and a shared number is one fewer
thing to tune. Eviction costs a fjall read, never a re-judge.

**An unjudged template is dashboard-only.** It surfaces, and it never pages: the same
lane §5.9 gives confidence below 0.60. So Jev being unreachable degrades to "nothing new
pages" rather than stalling ingest or, worse, paging on everything unknown. A new noisy
template at 3am must not wake someone *because* oarfish has not learned it yet — that is
the failure this product exists to prevent.

**A dropped enqueue is harmless.** If the queue is full the send fails and nothing
happens; the next line carrying that template misses again and re-enqueues. The queue
needs no unbounded growth, no retry logic and no persistence, because the log stream is
itself the retry mechanism.

## 6. The wire

`oarfish-jev` exposes one call, in the shape the gate observed:

```rust
client.decide(state, &questions) -> Decision {
    answers: BTreeMap<String, Answer>,   // choice | score | noul
    model: String,                       // the resolved, dated id
    usage: Usage,                        // input/output tokens, cost
}
```

Endpoint `https://openrouter.ai/api/alpha/decisions`, body `{model, state, questions}`,
a question being `{type, instructions, criteria}`.

`Decision` and `Verdict` are deliberately different types. `Decision` is the wire: what
the provider returned, owned by `oarfish-jev`, shaped by an `alpha` endpoint. `Verdict`
is the domain type in `oarfish-core` that the rest of oarfish stores, caches and
renders. `Verdicts` maps one to the other on the way in, which is what keeps a change to
an unstable wire format from reaching the board.

**`probabilities` and `confidence` are required fields, never `Option`.** A payload
without a confidence is a parse error, not a default. This turns "never synthesize a
confidence" from a rule someone must remember into one the type system enforces: a
`None` defaulted to `0.0` would route as "record, no surface" and silently swallow a
page. The gate showed confidence is not reconstructible anyway — max probability 0.69
against confidence 0.59, and 0.64 against 0.27, in one response.

Because the endpoint is `alpha`, the wire types stay behind this crate's own types.
Nothing outside `oarfish-jev` names a provider field.

## 7. The judge

A task owning the client and a set of in-flight keys. The queue carries
`(TemplateId, template_text)` — the call needs the state, not just the id.

Write ordering on an answer is **decision record, then fjall verdict, then moka**. A
crash between writes then leaves an orphan record, which is harmless and replayable,
rather than a cached verdict with no provenance — the exact thing §7 calls the trust
feature. The intolerable failure is a verdict at 3am that nothing explains.

Concurrency is bounded at 4–8 in flight. Sequential would be simpler, but a first run
meets a few hundred new templates at once and at 1–2s per call the board would stay
empty for minutes.

There is no retry logic. A failed call drops its key from the in-flight set and does
nothing more; §5 already explains why that is sufficient.

## 8. Testing

`wiremock` throughout — tests never reach the real API. It bills, and the routing needs
*chosen* confidence values rather than whatever the model happens to say today.

The fixtures are transcribed from the payload the gate actually captured, including the
dated `model` field, so they are a record of the wire rather than a guess at it.

| Property | Test |
|---|---|
| Judged once | A second identical template fires **zero** further requests, asserted on wiremock's request count. This is the milestone's acceptance |
| Confidence survives | Chosen `0.93` and `0.61` reach `Verdict` unchanged |
| Never synthesized | A payload missing `confidence` fails to parse, rather than defaulting |
| Re-judge on change | A changed question set, and a changed resolved model, each miss the cache |
| Failure is survivable | A 500 leaves the cache empty and the template re-enqueueable |
| Record before verdict | A record exists for every cached verdict; the reverse may not hold |
| Replay | A `template_id` prefix scan returns every verdict that template has held |

## 9. Deferred, deliberately

| Deferred | Until | Why |
|---|---|---|
| The real question set | M5 | It is policy, and `oarfish-engine` owns it |
| `merges` keyspace and merge review | M5 | Nothing notices two templates are close until windows exist |
| `alarms`, `corrections` | M5, M6 | Their shapes follow the state machine and the board's feedback flow |
| Routing on confidence | M5 | M4 carries the number faithfully; §5.9 acts on it |
| Local-model fallback | Unscheduled | §13; a config change by construction |

## 10. Changes to existing documents

- `crates/oarfish-core/src/lib.rs` — the doc comment says `Verdict` arrives with
  `oarfish-jev`; it lives in core, for the same reason `Event` does.
- `README.md` — tick M4 when §8's acceptance holds.
- `docs/specs/2026-09-17-oarfish-design.md` §7 — the `verdicts` key is three components,
  not one. Update the keyspace table.
