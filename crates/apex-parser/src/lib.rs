//! Hand-written recursive-descent parser producing an `apex-syntax` CST.
//!
//! Error recovery is a first-class design goal: on a syntax error the
//! parser should resynchronize at a statement/member boundary and keep
//! producing a best-effort tree for the rest of the file.
