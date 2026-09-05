Type: implement
Status: open

## Question

Ticket 03's own writeup identified a small, already-fully-specified fix:
salsa's `FileTextInput.text` is currently a `String`
(`crates/apex-binder/src/db.rs`), and `apex_parser::Parse::text` (added by
this effort's own ticket 01) is built via `Arc::from(src)` where `src`
borrows from `FileTextInput`'s own `String` -- this copies the source bytes
a second time rather than sharing the same allocation.

Change `FileTextInput.text`'s storage to `Arc<str>` (with whatever
`#[returns(...)]` accessor attribute salsa requires, per the diagnostics
map's ticket 24's own already-discovered mechanical details) so
`Parse::text` can become a cheap `Arc::clone` of the same allocation instead
of a fresh copy.

Verify via a targeted before/after dhat run (`mem_profile.rs`) that the
second copy is gone, and via `cargo bench -p apex-binder` that
`corpus/bind_npsp_full` and `corpus/warm_rebind_after_one_file_edit` show no
statistically significant regression. This is a small, low-risk,
already-decided change -- ship it directly, no separate decision ticket
needed.
