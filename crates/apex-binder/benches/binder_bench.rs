//! Binder performance regression suite. Run with `cargo bench -p
//! apex-binder`. See `apex-lexer/benches/lexer_bench.rs` for the
//! `--save-baseline`/`--baseline` comparison workflow.
//!
//! `"corpus"` tracks `BoundProgram::from_files`'s whole-pipeline cost
//! (discover + parse + collect + inherit + resolve, real NPSP source) --
//! `apex-binder` doesn't expose a separate "declarations only" entry
//! point yet (no real caller needs one; Pass 2's cost is bundled with
//! Pass 1's behind the one public `from_files` call), so unlike
//! `apex-parser`'s bench there's no cheaper sub-pipeline variant to
//! isolate here.
//!
//! `"constructs"` isolates specific resolution-cost shapes via tiny,
//! hand-written fixture projects (deep SOQL relationship-field chains,
//! a class with many overloaded same-name methods, a deep `extends`
//! chain) -- written to a temp dir once per bench function, outside the
//! timed closure, and cleaned up afterward. Since `BoundProgram::from_files`
//! parallelizes internally (`rayon`), these 2-3-file fixtures are
//! expected to run measurably *slower* than they did before
//! parallelization was added: thread-pool scheduling overhead is a
//! roughly fixed per-call cost that only pays for itself past a real
//! project's file/symbol count, which these deliberately tiny fixtures
//! never reach. `"corpus"` (real NPSP, ~1070 files) is where
//! parallelization's actual payoff shows up.

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/npsp")
}

fn corpus_total_bytes(root: &Path) -> u64 {
    apex_discover::find_apex_files(root)
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

fn bench_corpus(c: &mut Criterion) {
    let root = corpus_root();
    assert!(
        root.exists(),
        "no NPSP corpus found at {}; is the submodule checked out? (git submodule update --init --recursive)",
        root.display()
    );

    let mut group = c.benchmark_group("corpus");
    group.throughput(Throughput::Bytes(corpus_total_bytes(&root)));
    // A whole-corpus bind (discover + parse + collect + inherit +
    // resolve over ~1070 real files) is far slower than a single parse
    // -- trade sample count for a suite that stays fast to run
    // regularly, matching `apex-parser/benches/parser_bench.rs`'s own
    // corpus-group tradeoff.
    group.sample_size(10);
    group.bench_function("bind_npsp_full", |b| {
        b.iter(|| {
            std::hint::black_box(apex_binder::BoundProgram::from_files(std::hint::black_box(
                &root,
            )))
        });
    });
    group.finish();
}

/// Isolates `BACKLOG.md` §2 Step 2's payoff: `ParseCache` is warmed once
/// (outside the timed loop, matching a just-opened project before the
/// user's first keystroke), then each timed iteration simulates one more
/// single-file edit -- every *other* file's content is still exactly
/// what's cached, so `from_files_cached` should skip re-lexing/
/// re-parsing them, paying only for the one changed file's parse plus a
/// full project-wide Pass 1/1.5/2 rebind (rebind itself is *not* yet
/// incremental -- that's `BACKLOG.md` §2's still-open "incremental
/// rebind" item -- so this number is today's real baseline, not the
/// eventual best case, which is exactly what Step 3's decision needs).
/// Each iteration edits with a unique suffix (via `iter_batched`'s
/// untimed setup) so the cache never coincidentally already matches --
/// a real edit every time, not a no-op after the first sample.
fn bench_warm_single_edit(c: &mut Criterion) {
    let root = corpus_root();
    assert!(
        root.exists(),
        "no NPSP corpus found at {}; is the submodule checked out? (git submodule update --init --recursive)",
        root.display()
    );
    let target = apex_discover::find_apex_files(&root)
        .into_iter()
        .next()
        .expect("corpus has at least one file to simulate editing");
    let original = std::fs::read_to_string(&target).unwrap();

    let cache = RefCell::new(apex_binder::BindCache::default());
    apex_binder::BoundProgram::from_files_cached(&root, &HashMap::new(), &mut cache.borrow_mut());

    let mut group = c.benchmark_group("corpus");
    group.sample_size(10);
    group.throughput(Throughput::Elements(1));
    let edit_counter = Cell::new(0u32);
    group.bench_function("warm_rebind_after_one_file_edit", |b| {
        b.iter_batched(
            || {
                let n = edit_counter.get();
                edit_counter.set(n + 1);
                let mut overrides = HashMap::new();
                overrides.insert(target.clone(), format!("{original}\n// edit {n}"));
                overrides
            },
            |overrides| {
                std::hint::black_box(apex_binder::BoundProgram::from_files_cached(
                    std::hint::black_box(&root),
                    std::hint::black_box(&overrides),
                    &mut cache.borrow_mut(),
                ))
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

/// Writes `files` under a fresh temp dir named `apex-binder-bench-{name}`,
/// returning the dir. Left in place across `b.iter()` calls (each
/// `from_files` call just re-reads the same small, static fixture) and
/// removed once the caller is done benchmarking it.
fn write_fixture(name: &str, files: &[(&str, String)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("apex-binder-bench-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (file_name, src) in files {
        let path = dir.join(file_name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, src).unwrap();
    }
    dir
}

/// A `Contact.Account.Owner.Manager.Name`-style relationship chain,
/// `depth` hops deep, stressing `crate::soql`'s hop-by-hop
/// `SchemaIndex::field` traversal. Backed by real, tiny SFDX metadata
/// (each `Obj{i}__c` has one lookup field, `Next__c`, pointing at
/// `Obj{i+1}__c`) rather than standard objects, since this crate has no
/// bundled standard-object schema to hop through.
fn deep_field_chain_fixture(depth: usize) -> PathBuf {
    let mut files: Vec<(&str, String)> = Vec::new();
    let mut owned_names = Vec::new();
    for i in 0..=depth {
        owned_names.push(format!("Obj{i}__c"));
    }

    let mut object_files: Vec<(String, String)> = Vec::new();
    for i in 0..depth {
        let this_obj = &owned_names[i];
        let next_obj = &owned_names[i + 1];
        object_files.push((
            format!("objects/{this_obj}/{this_obj}.object-meta.xml"),
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomObject xmlns=\"http://soap.sforce.com/2006/04/metadata\"><label>{this_obj}</label></CustomObject>"
            ),
        ));
        object_files.push((
            format!("objects/{this_obj}/fields/Next__c.field-meta.xml"),
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomField xmlns=\"http://soap.sforce.com/2006/04/metadata\"><fullName>Next__c</fullName><type>Lookup</type><referenceTo>{next_obj}</referenceTo></CustomField>"
            ),
        ));
    }
    let last_obj = &owned_names[depth];
    object_files.push((
        format!("objects/{last_obj}/{last_obj}.object-meta.xml"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomObject xmlns=\"http://soap.sforce.com/2006/04/metadata\"><label>{last_obj}</label></CustomObject>"
        ),
    ));
    object_files.push((
        format!("objects/{last_obj}/fields/Name__c.field-meta.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomField xmlns=\"http://soap.sforce.com/2006/04/metadata\"><fullName>Name__c</fullName><type>Text</type></CustomField>".to_string(),
    ));

    let chain: String = owned_names
        .iter()
        .map(|n| n.replace("__c", "__r"))
        .collect::<Vec<_>>()
        .join(".");
    let query = format!("SELECT {chain}.Name__c FROM {} LIMIT 1", owned_names[0]);
    let cls = format!(
        "public class DeepChainQuery {{ public void run() {{ List<{}> rows = [{query}]; }} }}",
        owned_names[0]
    );
    files.push(("DeepChainQuery.cls", cls));

    let mut all_files: Vec<(&str, String)> = files;
    for (path, content) in &object_files {
        all_files.push((path.as_str(), content.clone()));
    }
    write_fixture("deep_field_chain", &all_files)
}

fn wide_overload_set_fixture(overload_count: usize) -> PathBuf {
    let mut methods = String::new();
    for i in 0..overload_count {
        methods.push_str(&format!("public void handle(Integer a{i}) {{ }}\n"));
    }
    let base = format!("public virtual class WideBase {{ {methods} }}");
    let caller =
        "public class WideCaller extends WideBase { public void run() { handle(1); } }".to_string();
    write_fixture(
        "wide_overload_set",
        &[("WideBase.cls", base), ("WideCaller.cls", caller)],
    )
}

fn deep_extends_chain_fixture(depth: usize) -> PathBuf {
    let mut files: Vec<(String, String)> = Vec::new();
    files.push((
        "Level0.cls".to_string(),
        "public virtual class Level0 { public Integer x; }".to_string(),
    ));
    for i in 1..=depth {
        files.push((
            format!("Level{i}.cls"),
            format!(
                "public virtual class Level{i} extends Level{prev} {{ }}",
                prev = i - 1
            ),
        ));
    }
    files.push((
        format!("Level{}Caller.cls", depth),
        format!(
            "public class Level{depth}Caller extends Level{depth} {{ public void run() {{ x = 1; }} }}"
        ),
    ));
    let refs: Vec<(&str, String)> = files.iter().map(|(n, s)| (n.as_str(), s.clone())).collect();
    write_fixture("deep_extends_chain", &refs)
}

fn bench_constructs(c: &mut Criterion) {
    let mut group = c.benchmark_group("constructs");

    let deep_field_chain_dir = deep_field_chain_fixture(8);
    group.bench_function("deep_field_chain_resolution", |b| {
        b.iter(|| {
            std::hint::black_box(apex_binder::BoundProgram::from_files(std::hint::black_box(
                &deep_field_chain_dir,
            )))
        });
    });

    let wide_overload_dir = wide_overload_set_fixture(200);
    group.bench_function("wide_overload_set_lookup", |b| {
        b.iter(|| {
            std::hint::black_box(apex_binder::BoundProgram::from_files(std::hint::black_box(
                &wide_overload_dir,
            )))
        });
    });

    let deep_extends_dir = deep_extends_chain_fixture(50);
    group.bench_function("deep_extends_chain", |b| {
        b.iter(|| {
            std::hint::black_box(apex_binder::BoundProgram::from_files(std::hint::black_box(
                &deep_extends_dir,
            )))
        });
    });

    group.finish();

    let _ = std::fs::remove_dir_all(deep_field_chain_dir);
    let _ = std::fs::remove_dir_all(wide_overload_dir);
    let _ = std::fs::remove_dir_all(deep_extends_dir);
}

criterion_group!(
    benches,
    bench_corpus,
    bench_warm_single_edit,
    bench_constructs
);
criterion_main!(benches);
