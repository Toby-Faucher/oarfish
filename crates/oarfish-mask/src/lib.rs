//! Replaces the variable parts of a log line with typed placeholders, before
//! anything tries to cluster it.
//!
//! ```
//! let masked = oarfish_mask::curated().mask("Failed password from 10.0.0.5 port 22");
//! assert_eq!(masked.template(), "Failed password from <VAR:IP4> port <VAR:NUM>");
//! ```
//!
//! A bundle is an ordered list of named regexes. **Order is precedence**: the
//! whole bundle compiles into one alternation, and the first alternate to match
//! at a position wins, so specific patterns are declared above general ones. A
//! reserved first alternate passes existing placeholders through untouched,
//! which is what makes masking idempotent.
//!
//! The curated default bundle ships embedded in this crate and covers what a
//! homelab actually emits. Replacing Drain's own "a token containing a digit is
//! a variable" heuristic is the single largest accuracy win in the pipeline,
//! which is why this runs before clustering rather than inside it.
//!
//! This crate is on the every-line path: it is pure, it allocates one string
//! per line, and it makes no network call. It applies bundles and never writes
//! them.
//!
//! In: a `&str` body. Out: a `Masked` holding the raw line, its template, and
//! every `SlotMatch` that filled it. Depends on `regex`, `toml`, `serde`,
//! `blake3` and `thiserror`, and on nothing else in the workspace.

#![forbid(unsafe_code)]

mod bundle;
mod curated;
mod masker;
mod synth;

pub use bundle::{Bundle, BundleError, BundleHash, RESERVED_SLOT, SlotDef};
pub use curated::curated;
pub use masker::{Masked, SlotMatch};
pub use synth::{Candidate, find_candidates, merge};
