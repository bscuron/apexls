//! Renders an `apex-syntax` CST back to source text.
//!
//! `render(parse(source)) == source` (byte-for-byte) is the core round-trip
//! correctness property this crate exists to make checkable.
