//! Protocol-agnostic candidate-fix production and conflict resolution,
//! shared between the LSP's own `textDocument/codeAction` handlers
//! (`capabilities::dead_code_actions`) and `apexls fix`'s batch CLI
//! command -- see `CONTEXT.md`'s "Candidate fix"/"Fixable diagnostic"
//! entries. A fixable diagnostic earns that label only once its own fix
//! ticket argues the *edit* is safe, independent of the diagnostic's own
//! zero-false-positive detection bar; v1 covers only `dead_code_diagnostics`,
//! since removing a symbol nothing references is always the correct edit
//! for a diagnostic that already proves nothing else references it -- the
//! same edit `dead_code_actions` already shipped as an LSP quick-fix.

use apex_binder::{dead_symbols_in_file, kind_label, BoundProgram, FileId};
use rowan::TextRange;

/// One concrete, protocol-agnostic edit proposed for a fixable diagnostic.
/// Becomes an applied fix once it survives `resolve_fix_conflicts`, or a
/// subsumed/conflicting fix if it doesn't.
#[derive(Clone)]
pub(crate) struct CandidateFix {
    /// The edit itself -- what gets replaced with `new_text`, and what
    /// `resolve_fix_conflicts` classifies overlaps over.
    pub range: TextRange,
    /// The narrower range an interactive request (an LSP `codeAction`'s own
    /// requested range) must overlap for this fix to be offered -- e.g. a
    /// dead symbol's own *name*, not its whole multi-line deletion span, so
    /// a cursor placed anywhere else inside the soon-to-be-deleted
    /// declaration (a parameter, a body statement) doesn't also spuriously
    /// trigger it. `apexls fix`'s batch application ignores this entirely;
    /// it applies to the whole file regardless of any one "trigger" spot.
    pub trigger_range: TextRange,
    pub new_text: String,
    pub description: String,
}

/// Every fixable diagnostic's candidate fix for `file`, independent of any
/// requesting protocol -- an LSP-specific "does this overlap the requested
/// range" filter, if needed, happens on top of this, not inside it.
pub(crate) fn candidate_fixes_for_file(program: &BoundProgram, file: FileId) -> Vec<CandidateFix> {
    dead_symbols_in_file(program, file)
        .into_iter()
        .map(|dead| CandidateFix {
            range: dead.deletion_range,
            trigger_range: dead.name_range,
            new_text: String::new(),
            description: format!(
                "Remove unused {} '{}'",
                kind_label(dead.kind, dead.visibility),
                dead.name
            ),
        })
        .collect()
}

/// The result of running `resolve_fix_conflicts` over one file's candidate
/// fixes: `applied` is safe to write to disk as-is; `conflicts` is every
/// fix skipped because of a genuine crossing overlap, worth reporting so a
/// human can look. A subsumed fix (dropped because another fix's range
/// fully contains it) appears in neither list -- its target text is erased
/// by the containing fix regardless, so there's nothing to report.
pub(crate) struct FixResolution {
    pub applied: Vec<CandidateFix>,
    pub conflicts: Vec<CandidateFix>,
}

/// Classifies every overlap among `fixes` (all assumed to belong to one
/// file) into the two shapes this map settled on: a *nested* overlap (one
/// fix's range fully contains another's -- e.g. a whole-declaration
/// dead-code deletion containing a smaller duplicate-modifier deletion
/// inside that same dead declaration) drops the inner fix silently, while a
/// *crossing* overlap (ranges partially intersect, neither containing the
/// other) skips both fixes and reports them, since applying either would
/// edit text the other's own range was computed against. Generic over
/// every candidate fix regardless of which diagnostic produced it -- this
/// is the one place overlap policy lives, never special-cased per
/// diagnostic pair.
///
/// Two order-independent phases: first, drop every fix strictly contained
/// by some other fix (containment is transitive, so a fix nested three
/// deep still gets dropped correctly no matter which iteration order finds
/// it first). Second, among only the fixes that survive -- none of which
/// contains another, by construction -- any remaining intersection can
/// only be a genuine crossing overlap.
pub(crate) fn resolve_fix_conflicts(fixes: Vec<CandidateFix>) -> FixResolution {
    let n = fixes.len();
    let contained: Vec<bool> = (0..n)
        .map(|i| {
            (0..n).any(|j| {
                j != i
                    && fixes[j].range.contains_range(fixes[i].range)
                    && fixes[j].range != fixes[i].range
            })
        })
        .collect();

    let survivors: Vec<usize> = (0..n).filter(|&i| !contained[i]).collect();
    let mut conflicted = vec![false; n];
    for (pos, &i) in survivors.iter().enumerate() {
        for &j in &survivors[pos + 1..] {
            let crosses = fixes[i]
                .range
                .intersect(fixes[j].range)
                .is_some_and(|overlap| !overlap.is_empty());
            if crosses {
                conflicted[i] = true;
                conflicted[j] = true;
            }
        }
    }

    let mut applied = Vec::new();
    let mut conflicts = Vec::new();
    for (idx, fix) in fixes.into_iter().enumerate() {
        if contained[idx] {
            // Subsumed: dropped silently, nothing to report.
        } else if conflicted[idx] {
            conflicts.push(fix);
        } else {
            applied.push(fix);
        }
    }
    FixResolution { applied, conflicts }
}

/// `text` with every one of `fixes` applied. Requires `fixes`' ranges to be
/// pairwise disjoint (exactly what `resolve_fix_conflicts`'s `applied` list
/// already guarantees) -- applying them from the highest starting offset
/// down means each edit lands against a still-untouched prefix of `text`,
/// so no offset remapping is needed.
pub(crate) fn apply_fixes(text: &str, mut fixes: Vec<CandidateFix>) -> String {
    fixes.sort_by_key(|f| std::cmp::Reverse(f.range.start()));
    let mut text = text.to_string();
    for fix in fixes {
        let range = usize::from(fix.range.start())..usize::from(fix.range.end());
        text.replace_range(range, &fix.new_text);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(start: u32, end: u32, label: &str) -> CandidateFix {
        let range = TextRange::new(start.into(), end.into());
        CandidateFix {
            range,
            trigger_range: range,
            new_text: String::new(),
            description: label.to_string(),
        }
    }

    #[test]
    fn disjoint_fixes_are_all_applied() {
        let resolution = resolve_fix_conflicts(vec![fix(0, 5, "a"), fix(10, 15, "b")]);
        assert_eq!(resolution.applied.len(), 2);
        assert!(resolution.conflicts.is_empty());
    }

    #[test]
    fn nested_fix_is_silently_subsumed() {
        // A whole-method deletion containing a smaller nested deletion,
        // matching the real dead-method-with-a-dead-local-inside shape.
        let outer = fix(0, 50, "remove unused method 'helper'");
        let inner = fix(20, 28, "remove unused local 'x'");
        let resolution = resolve_fix_conflicts(vec![outer, inner]);
        assert_eq!(resolution.applied.len(), 1);
        assert_eq!(resolution.applied[0].description, "remove unused method 'helper'");
        assert!(resolution.conflicts.is_empty());
    }

    #[test]
    fn crossing_fixes_are_both_skipped_and_reported() {
        let a = fix(0, 10, "a");
        let b = fix(5, 15, "b");
        let resolution = resolve_fix_conflicts(vec![a, b]);
        assert!(resolution.applied.is_empty());
        assert_eq!(resolution.conflicts.len(), 2);
    }

    #[test]
    fn touching_but_non_overlapping_fixes_are_not_a_conflict() {
        let a = fix(0, 5, "a");
        let b = fix(5, 10, "b");
        let resolution = resolve_fix_conflicts(vec![a, b]);
        assert_eq!(resolution.applied.len(), 2);
        assert!(resolution.conflicts.is_empty());
    }

    #[test]
    fn apply_fixes_handles_multiple_disjoint_deletions() {
        let text = "0123456789";
        let fixes = vec![fix(2, 4, "a"), fix(6, 8, "b")];
        assert_eq!(apply_fixes(text, fixes), "014589");
    }

    #[test]
    fn apply_fixes_handles_a_nested_deletion_by_only_applying_the_outer() {
        let text = "0123456789";
        let resolution = resolve_fix_conflicts(vec![fix(2, 8, "outer"), fix(4, 6, "inner")]);
        assert_eq!(apply_fixes(text, resolution.applied), "0189");
    }
}
