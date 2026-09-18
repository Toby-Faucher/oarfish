//! HTTP surface: the JSON API, the SSE alarm stream, and static hosting for
//! the built Astro board.
//!
//! The board is a static shell with one live island; SSE is what keeps it
//! current without a poll or a rebuild.

#![forbid(unsafe_code)]
