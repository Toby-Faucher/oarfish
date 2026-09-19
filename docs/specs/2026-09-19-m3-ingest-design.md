# M3 — `oarfish-ingest`, three listeners and the first running pipeline

**Status:** accepted, pre-implementation
**Date:** 2026-09-19
**Parent:** `docs/specs/2026-09-17-oarfish-design.md` — milestone M3 in §11, mechanism in §5.1–5.2

---

## 1. What this milestone is for

§11 gives M3 the contract *lines from all three arrive as `Event`*. Taken literally that
is a library with no consumer: nothing reads an `Event` until M5's windows exist. M3
therefore ends one step further on — the daemon runs, and a line arriving on a socket
comes out the far side as a `TemplateId` from a shared Drain table.

That extra step is not scope creep. M1 and M2 each built a stage of the every-line path
and validated it against a corpus on disk. Neither has ever run against a live socket,
and the first time those stages meet is the first time their interfaces are tested at
all. Deferring the wiring to M5 would mean discovering interface problems underneath a
window implementation rather than on their own.

## 2. Boundaries

| In | Out |
|---|---|
| `Event` in `oarfish-core`, exported via `ts-rs` | Anything reading `Event` back: windows, alarms, storage |
| Syslog UDP + TCP, both on 514, RFC 5424 and 3164 | Syslog over TLS |
| OTLP logs over gRPC on 4317 | OTLP over HTTP/protobuf — deferred to M6, when a real collector is pointed at it |
| systemd journal behind the `journald` feature | Journal cursor persistence across restarts |
| `--syslog` / `--otlp` / `--journal` clap flags | `figment` config merging — M4, behind the same flag names |
| ingest → mask → drain, logging the assignment | Verdicts, severity, notification |

Metrics and traces stay out permanently. OTLP is a log transport here and nothing more.

## 3. `Event`

Lives in `oarfish-core` beside `Alarm`, following its conventions: `serde`, `ts-rs`
export to `oarfish.ts`, timestamps pinned to RFC 3339 with `#[ts(type = "string")]`.

```rust
pub struct Event {
    pub raw: Bytes,
    pub received_at: OffsetDateTime,
    pub timestamp: Option<OffsetDateTime>,
    pub host: String,
    pub source: Source,          // Syslog | Journal | Otlp
    pub attrs: BTreeMap<String, String>,
}
```

**`raw` is `Bytes`, not `String`.** Invariant 3 says the operator gets the bytes that
arrived, and hardware that is already misbehaving emits invalid UTF-8. A `String` field
would force a lossy conversion at intake and break that promise for exactly the devices
worth reading at 3am. `Bytes` in process and in the store; an inherent
`Event::raw_lossy(&self) -> Cow<'_, str>` on the core type, called at the mask
boundary; the `ts-rs` export declares `string` through a lossy serde adapter,
because the board cannot render invalid UTF-8 and that boundary is the one place the
conversion is unavoidable.

This adds `bytes` to `oarfish-core`, against the standing rule. Accepted deliberately:
the alternative is breaking a stated invariant about log data to protect a rule about
dependencies, and the invariant is the one that gets someone out of bed. Invariant 1 —
core depends on no other workspace crate — is untouched.

**Two timestamps, and windows use ours.** `received_at` is oarfish's clock and is what
M5 windows on. `timestamp` is what the source claimed. Cheap hardware has bad clocks; a
switch reporting 1970 must not be able to distort a five-minute window. The discrepancy
between the two is itself diagnostic, so both are kept.

**Syslog severity goes in `attrs`, never into `Severity`.** They are different things:
`Severity` is the engine's verdict about whether to wake someone, the syslog number is a
value a vendor's firmware picked. Mapping one onto the other would let any device on the
network set its own alarm severity, which is the thing the cached-verdict design exists
to prevent.

`BTreeMap` rather than `HashMap`: deterministic ordering for snapshots and for postcard.

## 4. Structure

```
oarfish-ingest/src/
  lib.rs       re-exports and the crate doc
  syslog.rs    UDP + TCP, both on 514, RFC 5424/3164 via syslog_loose
  otlp.rs      tonic LogsService, gRPC on 4317
  journal.rs   #[cfg(feature = "journald")]
  shed.rs      counted-drop bookkeeping, shared
```

Each listener is a concrete type with a two-phase lifecycle. No trait and no registry:
there are exactly three, they are not pluggable, and a trait would buy indirection
nobody calls through.

```rust
let listener = SyslogUdp::bind(addr).await?;
let addr = listener.local_addr();
tracker.spawn(listener.run(tx.clone(), cancel.child_token()));
```

Bind and run are separate for one reason: a test binds `127.0.0.1:0` and needs the
ephemeral port before anything starts reading.

## 5. Flow and overload

Listeners write into one bounded `mpsc<Event>`. One pipeline task owns the `Bundle` and
the `Drain` and drains it. There is exactly one consumer because `Drain::train` takes
`&mut self` — the type mandates it, so this is not a tuning choice.

Overload policy is **backpressure where the transport allows it, counted shedding where
it does not**:

| Transport | Full channel | Why |
|---|---|---|
| Syslog TCP, OTLP gRPC | `tx.send().await` — the read loop stalls | The sender's window closes on its own; nothing is lost and nothing needs coordinating |
| Syslog UDP | `tx.try_send()`, shed and count on `Full` | A datagram socket has no backpressure to apply; the only honest options are counting the loss or hiding it |

`governor` does two jobs: a quota on UDP intake, and damping the shed warning itself. A
100k lines/sec flood must produce one warn per interval carrying an accumulated count,
not 100k warns — the log about the firehose must not become the firehose.

Shedding is never silent. Every dropped datagram increments a per-source counter that is
logged and, from M5, is available as a signal in its own right: an operator who is losing
UDP wants to be told, not left to infer it from a quiet board.

## 6. Shutdown

`CancellationToken` and `TaskTracker`, per the dependency table. Cancel stops the
listeners; listeners drop their `Sender`s; the channel closes; the pipeline task drains
what is still queued before `recv()` yields `None`.

The useful property falls out of the ordering for free: a clean stop loses nothing that
was already accepted. It is tested rather than assumed.

## 7. Malformed input is data, not an error

When `syslog_loose` cannot parse a frame, the listener still emits an `Event`: raw bytes
verbatim, `host` from the socket peer, `attrs["parse"] = "failed"`.

This is the same reasoning as invariant 3 one level up. A device emitting frames our
parser rejects is a device worth an alarm; discarding those lines because the firmware
disagrees with the RFC would delete exactly the signal the milestone exists to carry.

`IngestError` (thiserror) therefore covers only fatal setup: bind, journal open. A bind
failure exits the daemon with a clear `anyhow` message. Per-connection faults — peer
reset, truncated frame — log at debug and drop that connection, never the listener.

## 8. The feedback loop

The daemon's own `tracing` output goes to journald, and the journal listener would read
it straight back in, emitting a log line per log line.

`journal.rs` filters its own `_SYSTEMD_UNIT` by default. This is recorded here because it
cannot fail in CI: it needs a real systemd box, a real unit, and the daemon's own writes
going to the journal, which is precisely the configuration nobody tests until it is
running in anger.

## 9. Testing

| Area | Approach |
|---|---|
| Syslog parse → `Event` | `insta` snapshots over RFC 5424 and 3164 fixtures. Parser-shaped work, so a threshold or field change shows exactly what moved |
| Syslog TCP framing | One frame split across two writes, reassembled by `LinesCodec` |
| OTLP | Stand the tonic server up, drive it with the generated client |
| journald | FFI quarantined behind a pure `record -> Event` function, tested with no libsystemd. CI compiles the feature; a local smoke test exercises a real journal |
| Overload | Capacity-1 channel: UDP sheds *and counts*, TCP stalls instead of shedding. The §5 policy made executable |
| Shutdown | Cancel mid-flight, assert queued events still drain |
| Invariant 3 | `proptest`: for arbitrary bytes in, `event.raw` equals them exactly |

No `wiremock` here — it belongs to `oarfish-jev`. Sockets on loopback are the real thing
and cost nothing.

## 10. The daemon

`crates/oarfish` gains clap flags defaulting to §9's values:

```
--syslog 0.0.0.0:514   --otlp 0.0.0.0:4317   --journal
```

`--journal` is rejected at startup with a clear message when the binary was built without
the `journald` feature. A flag that silently does nothing is how an operator ends up
believing a source is being read when it is not.

`figment` lands in M4 behind these same names, so the daemon's surface does not move when
it arrives. `tracing-appender` non-blocking for the daemon's own writes; `anyhow` at the
top; the binary stays wiring only and owns no pipeline logic, per its doc comment.

**Acceptance:** a line sent by `logger` over syslog and a line sent by an OTLP client both
emerge as a logged `TemplateId` from one shared Drain table. On a systemd host the same
holds for a journal line, checked by the local smoke test in §9 rather than by CI.

## 11. Deferred, deliberately

| Deferred | Until | Why |
|---|---|---|
| OTLP over HTTP/protobuf | M6 | No consumer proves it until a real collector points at oarfish |
| `figment` config | M4 | Flags cover M3; merging is a second subsystem inside an already large milestone |
| Journal cursor persistence | M4 | It is a storage concern and `oarfish-store` does not exist yet |
| Syslog over TLS | Unscheduled | A homelab's syslog is on the trusted side of the network |
| Backpressure signals on the board | M5 | The counter exists in M3; nothing renders it yet |

## 12. Changes to existing documents

- `README.md` — tick M3 once the acceptance in §10 holds.
- `CLAUDE.md` — record that `oarfish-core` now carries `bytes`, and why, so the standing
  rule is not read as having been broken by accident.
- `crates/oarfish-core/src/lib.rs` — the doc comment says `Event` arrives with
  `oarfish-ingest`; update it to say `Event` lives here and why.
