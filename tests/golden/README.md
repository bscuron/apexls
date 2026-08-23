# Golden tests

Snapshot tests organized to mirror the grammar structure, one directory per
construct. Each subdirectory holds minimal `.cls`/`.trigger`/snippet inputs
plus their expected CST/AST snapshot.

- `declarations/class`, `declarations/interface`, `declarations/enum`, `declarations/trigger`
- `statements/`
- `expressions/` (one or more cases per precedence level)
- `soql/`, `sosl/`
- `annotations/`
- `generics/`
- `anonymous-blocks/`
- `malformed/` — deliberately invalid inputs with expected diagnostics and
  expected error-recovery shape (the rest of the file should still produce a
  best-effort tree)
