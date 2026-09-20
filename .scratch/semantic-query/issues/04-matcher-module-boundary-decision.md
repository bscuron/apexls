Type: grilling
Status: open
Blocked by: 02

## Question

Where does the matcher live, and what is its public interface?

The map's destination is a spec, and a spec that does not say where the code goes leaves the
implementation session to invent a boundary under time pressure. Read the map's Notes for the
dependency graph, and consult the codebase-design skill for the deep-module vocabulary.

Decide, concretely:

1. **New crate or module?** `crates/apexls` depends on `apex-binder`, `apex-discover`,
   `apex-parser`, `apex-printer`, `apex-syntax` and `apexls-server`, so a `query` module inside
   `crates/apexls` can reach everything it needs today with no new crate. Against that: the map's
   fog includes an LSP surface, and `apexls-server` cannot depend on `crates/apexls`. Decide
   whether the matcher is a module in the CLI crate now (and moved later if the LSP wants it), or
   its own crate from the start. Note that a new crate is the kind of scaffolding that is cheap to
   add later and hard to remove, so the burden of proof is on creating one.
2. **The public interface.** What does a caller hand in, and what comes back? A pattern string
   plus a `SyntaxNode`, returning matches with their captures and significant spans, is the
   obvious shape -- but decide whether pattern *compilation* is a separate step with its own type
   (it must be, if the same pattern is applied across thousands of files on a rayon walk) and what
   a compilation error looks like to the user. A bad pattern is the most common failure mode of
   every tool in the survey; Semgrep's "Pattern parse error" is a documented rough edge. The error
   message is part of the interface.
3. **The traversal.** `soql.rs:131` is the precedent: rayon-parallel per file, results collected
   then globally sorted because walk order varies. Confirm `query` follows it, and confirm
   `.trigger` files dispatch to `parse_trigger_unit` the way `soql.rs:163-178` now does (`ast.rs:39`
   still does not -- note whether that is a separate bug worth filing). Per the map's Notes, a
   structural search must not construct a `BoundProgram`.
4. **The shared traversal helper, if any.** `root.descendants().filter_map(T::cast)` appears 40+
   times across the workspace with no shared helper, and there is no visitor trait anywhere. Decide
   whether the matcher introduces one or stays with the existing idiom. The lazy answer is to add
   nothing; say so explicitly if that is the conclusion, so nobody adds a framework later assuming
   it was an oversight.

5. **Settle the word "query".** `crates/apexls/src/soql.rs:216-222` already defines
   `struct Query` meaning "a SOQL query site found in source", and `apexls soql` is built around
   that sense. The subcommand chosen for this map is `apexls query`, meaning "a structural
   pattern matched against source" -- an unrelated sense of the same word, in the same CLI, one
   module apart. Decide whether the new concept takes a different name, the old type is renamed
   (`SoqlSite`?), or the two senses are held apart by module path alone. Whatever is chosen, the
   winning terms go in `CONTEXT.md` when `query` ships -- the glossary is currently scoped to the
   diagnostic/fix pipeline and has no entry for either sense.

Resolution records the crate/module decision with its reasoning, the public types, the error
surface for a malformed pattern, and the resolution of the "query" naming collision.
