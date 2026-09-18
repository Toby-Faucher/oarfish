//! Applies the regex mask bundle that replaces variables with typed
//! placeholders before clustering.
//!
//! The bundle is synthesized once at install and is immutable at runtime, so
//! this crate only ever *applies* masks - it never writes them. Replacing
//! Drain's own "a token with a digit is a variable" heuristic is the single
//! largest accuracy win in the pipeline.
//!
//! In: `&str` body. Out: masked body plus the placeholders that matched.

#![forbid(unsafe_code)]

mod bundle;
mod curated;
mod masker;

pub use bundle::{Bundle, BundleError, BundleHash, RESERVED_SLOT, SlotDef};
pub use curated::curated;
pub use masker::{Masked, SlotMatch};
