# oarfish — working notes for agents

Log-driven alarms for homelabs. Rust daemon + Astro board. Pre-alpha: the scaffold
builds, almost nothing is implemented.

## The one idea

Work is priced by how often it runs. Three rates, kept strictly separate:

| Rate | What | Cached against |
|---|---|---|
| Once, at install | LLM synthesizes the regex mask bundle | `bundle_hash` |
| Per new template | Static verdict (kind, severity, actionable) | `TemplateId` |
| Per close pair | Merge review | `(id_a, id_b)` |
| Per burst, flagged only | Contextual check | not cached |
| Every line | mask → cluster → window → alarm engine | pure Rust |

If you are about to make something run more often than it has to, stop.

## Invariants

1. **`oarfish-core` depends on nothing else in the workspace.** Every other crate
   depends on it; a cycle here is a cycle everywhere.
2. **Nothing on the every-line path makes a network call.** No exceptions. If a
   feature seems to need one, it belongs on a cached path.
3. **The raw log line is preserved verbatim, always.** At 3am the operator wants the
   bytes that arrived, not our interpretation of them.
4. **Drain runs at a conservative similarity threshold (0.90).** It should over-split
   rather than merge `task succeeded` into `task failed` — a merge like that silently
   deletes the alarm. Over-splits are repaired by merge review, not by lowering the
   threshold.
5. **`oarfish-jev` is transport only.** It holds no policy about which questions to
   ask; that lives in `oarfish-engine`.

## The board is a first-class feature

`web/` is not a bolt-on. It is the primary interface. Server-rendered Astro shell,
one live island fed by SSE, minimal client JS. Changes to alarm shape need a
corresponding change to the board.

## Commands

```sh
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings   # CI runs with -D warnings
cargo fmt --all
cargo test --workspace
cargo insta review          # after snapshot changes
cargo bench -p oarfish-drain

cd web && npm run dev      # board on :4321, proxies /api to the daemon on :4000
cd web && npm run build
```

## Testing approach

- **`insta`** for anything parser-shaped: raw lines -> masked -> templates. When you
  tune a mask or a threshold you should see exactly which templates moved. This is
  also how the Drain port gets validated against drain3 — snapshot both over one
  corpus and diff.
- **`wiremock`** for everything touching `oarfish-jev`. Tests never hit the real API:
  it bills, and the engine's routing needs *chosen* confidence values so you can prove
  0.93 pages and 0.61 does not.
- **`proptest`** for invariants — masking is idempotent, template ids are stable.
- **`criterion`** for the every-line path. "Fast" without a regression guard is a
  claim, not a property.

## Conventions

- Rust 1.98, edition 2024, pinned in `rust-toolchain.toml`.
- `#![forbid(unsafe_code)]` at the top of every crate. Keep it there.
- Errors: `thiserror` in libraries, `anyhow` in the binary.
- Each crate's `lib.rs` opens with a doc comment saying what it does, how you use it,
  and what it depends on. Keep it accurate when you change the crate.
- Tests live beside the code they test unless they need fixtures.

## Key dependencies — reach for the right one

| Need | Use | Not |
|---|---|---|
| Framing syslog over TCP | `tokio-util` `codec` (`LinesCodec`, `Framed`) | hand-rolled byte scanning |
| Alarm auto-clear / flap windows | `tokio-util` `time::DelayQueue` | a task per open alarm |
| Coordinated shutdown | `tokio-util` `rt` (`CancellationToken`, `TaskTracker`) | ad-hoc channels |
| OTLP ingest | `tonic` + `prost` + `opentelemetry-proto` | vendored `.proto` files |
| Ingest backpressure | `governor` | dropping on the floor |
| Hot verdict lookups | `moka` (in front of `fjall`) | hitting fjall per line |
| Values stored in fjall | `postcard` | JSON in an LSM store |
| Alarm ids | `ulid` (`Ulid::generate()`) | `uuid` v4 — it will not sort |
| Board-facing types | `ts-rs`, exported from `oarfish-core` | hand-written TS interfaces |
| Applying the mask bundle | `regex::RegexSet` — one pass | looping N regexes per line |
| Config (file + env + CLI) | `figment` | hand-rolled merging |
| The daemon's own log writes | `tracing-appender` (non-blocking) | blocking on the ingest path |

A runaway container emitting 100k lines/sec is a normal homelab failure, not an
edge case. Ingest is rate-limited on purpose.

## Optional features

- `console` — runtime debugging via tokio-console. Needs the cfg flag too:
  ```sh
  RUSTFLAGS="--cfg tokio_unstable" cargo run -p oarfish --features console
  ```

## External APIs

**Jev / TypeSafe System One** — `POST https://api.typesafe.ai/v1/systemone`, bearer
auth via `TYPESAFE_API_KEY`. Body is `{state, model, questions}`; questions are
`choice` / `score` / `noul` and all evaluate in parallel, so ask everything in one
request. Request budget ~32k tokens shared between state and questions. Answers carry
calibrated `probabilities` and `confidence`. Docs: https://docs.typesafe.ai

## Don't

- Don't add a dependency to `oarfish-core`.
- Don't put an LLM call on the every-line path.
- Don't lower the Drain threshold to "fix" fragmented templates.
- Don't summarize or rewrite log lines. Oarfish classifies; it does not narrate.
