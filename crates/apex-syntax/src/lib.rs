//! Lossless (trivia-preserving) concrete syntax tree for Apex, plus a typed
//! AST layer built as a cheap traversal over it (Roslyn/rust-analyzer style
//! red-green tree). This is what makes exact source round-trip possible.
