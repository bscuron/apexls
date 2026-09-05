Type: decision
Status: open
Blocked by: 06

## Question

Given [ticket 06](06-rowan-tree-retention-research.md)'s findings on whether
Pass 2/`resolve.rs` and `apexls-server`'s capability handlers need every
file's rowan syntax tree retained in steady state, decide the concrete
architecture for reducing rowan syntax-tree memory: whether to (a) move to
an on-demand/LRU-evicted `GreenNode` retention model, (b) instead or also
shrink rowan's own per-node representation (e.g. more compact tokens,
interned strings), (c) some combination of both, or (d) conclude no safe or
worthwhile change exists here. Lock the exact data-structure changes,
eviction policy (if any), and re-parse trigger points needed, sized for a
follow-on implement ticket. Must account for `cargo bench -p apex-binder`'s
existing `corpus/bind_npsp_full` and `corpus/warm_rebind_after_one_file_edit`
gates (no statistically significant regression) as a hard constraint on any
chosen design.
