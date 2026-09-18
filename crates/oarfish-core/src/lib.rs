//! Domain types shared by every stage of the pipeline.
//!
//! Depends on nothing else in the workspace, by design: every other crate
//! depends on this one, so a cycle here would be a cycle everywhere.
//!
//! Owns `Event` (a normalized log line), `TemplateId`, `Severity`, `Verdict`,
//! and `Alarm`. Nothing here does I/O.

#![forbid(unsafe_code)]
