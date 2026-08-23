# Grammar coverage

Tooling to attribute test-run coverage to grammar productions/branches
(not just source lines), so "which parts of the grammar are untested" is a
measured number instead of a guess. Used to gate CI on newly-added grammar
code shipping with zero test coverage, and to prioritize which productions
to go find real-world corpus examples for next.
