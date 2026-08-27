//! Ad-hoc, kept-not-thrown-away diagnostic (same status as `mem_profile.rs`/
//! `cpu_profile.rs`): clusters every `Resolution::Unresolved` reference
//! in the real NPSP corpus by its syntactic context -- the reference's
//! own `SyntaxKind` plus its immediate ancestor chain, up to a nearby
//! "whole body/type" boundary -- ranked by frequency.
//!
//! The idea: a genuine resolver *gap* (a real Apex construct this binder
//! simply never learned to bind, as opposed to a truly external/unknowable
//! name) tends to produce a large, tight cluster of `Unresolved`
//! references sharing one unusual structural shape -- exactly what the
//! SObject constructor field-init sugar and dynamic-SOQL bind-variable
//! gaps both looked like before they were fixed (both would have shown
//! up here as a large cluster with a distinctive ancestor-kind chain,
//! long before a user had to notice and report either by hand). This
//! turns "eyeball the corpus until something looks wrong" into a ranked
//! worklist. It only ever surfaces *missing* resolutions, never *wrong*
//! ones -- see `crates/apex-binder/tests/resolution_consistency.rs` for
//! the complementary automated check that catches a confidently-wrong
//! answer instead of a missing one.
//!
//! Run with `cargo run -p apex-binder --release --example unresolved_clusters`.

use apex_binder::{BoundProgram, Resolution};
use apex_syntax::SyntaxKind;
use rowan::TextRange;
use rustc_hash::FxHashMap;
use std::path::Path;

/// How many ancestor levels (beyond the reference's own kind) to fold
/// into one fingerprint -- enough to distinguish "a bare name read" from
/// "the LHS of `=` inside a `NewExpr`'s `ArgList`" (3 levels: `NameExpr`
/// -> `BinExpr` -> `ArgList` -> `NewExpr`) without climbing so far that
/// unrelated references sharing a distant ancestor (the same enclosing
/// method, say) get lumped together.
const MAX_ANCESTOR_DEPTH: usize = 4;

/// Node kinds that represent a whole statement body or type -- climbing
/// past one of these stops contributing anything more specific to a
/// fingerprint, since two references that only share "both live
/// somewhere in a method body" aren't meaningfully the same shape.
fn is_boundary(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Block
            | SyntaxKind::MethodDecl
            | SyntaxKind::ConstructorDecl
            | SyntaxKind::TriggerBlock
            | SyntaxKind::PropertyAccessor
            | SyntaxKind::ClassBody
            | SyntaxKind::InterfaceBody
            | SyntaxKind::CompilationUnit
    )
}

fn fingerprint(node: &apex_syntax::SyntaxNode) -> Vec<SyntaxKind> {
    let mut kinds = vec![node.kind()];
    let mut current = node.clone();
    for _ in 0..MAX_ANCESTOR_DEPTH {
        if is_boundary(current.kind()) {
            break;
        }
        let Some(parent) = current.parent() else { break };
        kinds.push(parent.kind());
        if is_boundary(parent.kind()) {
            break;
        }
        current = parent;
    }
    kinds
}

fn fingerprint_text(kinds: &[SyntaxKind]) -> String {
    kinds.iter().map(|k| format!("{k:?}")).collect::<Vec<_>>().join(" < ")
}

struct Example {
    path: std::path::PathBuf,
    line: usize,
    snippet: String,
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp");
    assert!(
        root.exists(),
        "no NPSP corpus found at {}; is the submodule checked out? (git submodule update --init --recursive)",
        root.display()
    );

    let program = BoundProgram::from_files(&root);

    let mut clusters: FxHashMap<Vec<SyntaxKind>, (usize, Vec<Example>)> = FxHashMap::default();
    const MAX_EXAMPLES_PER_CLUSTER: usize = 3;

    for (ptr, resolution) in program.all_resolutions() {
        if !matches!(resolution, Resolution::Unresolved) {
            continue;
        }
        let root_node = program.syntax(ptr.file());
        let Some(node) = ptr.to_node(&root_node) else {
            continue;
        };
        let kinds = fingerprint(&node);
        let entry = clusters.entry(kinds).or_insert_with(|| (0, Vec::new()));
        entry.0 += 1;
        if entry.1.len() < MAX_EXAMPLES_PER_CLUSTER {
            let text = root_node.text().to_string();
            let offset = usize::from(ptr.range().start());
            let line = text[..offset].matches('\n').count() + 1;
            let snippet = line_snippet(&text, ptr.range());
            entry.1.push(Example {
                path: program.file_path(ptr.file()).to_path_buf(),
                line,
                snippet,
            });
        }
    }

    let mut ranked: Vec<(Vec<SyntaxKind>, usize, Vec<Example>)> = clusters
        .into_iter()
        .map(|(k, (count, examples))| (k, count, examples))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));

    let total: usize = ranked.iter().map(|(_, count, _)| count).sum();
    println!("{total} total Unresolved references, in {} distinct shapes\n", ranked.len());

    for (kinds, count, examples) in ranked.iter().take(40) {
        println!("[{count:>6}] {}", fingerprint_text(kinds));
        for ex in examples {
            println!("         {}:{}  {}", ex.path.display(), ex.line, ex.snippet);
        }
    }
}

/// The trimmed source line containing `range`, with the reference itself
/// left in place -- enough for a human skimming the report to recognize
/// the pattern without opening the file.
fn line_snippet(text: &str, range: TextRange) -> String {
    let start = text[..usize::from(range.start())].rfind('\n').map_or(0, |i| i + 1);
    let end = text[usize::from(range.end())..]
        .find('\n')
        .map_or(text.len(), |i| usize::from(range.end()) + i);
    text[start..end].trim().to_string()
}
