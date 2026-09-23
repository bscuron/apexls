//! Generates `crates/apex-stdlib/data/standard_child_relationships.json`:
//! every standard object's child relationships, as Salesforce itself
//! reports them.
//!
//! **Why an org and not the doc scrape.** The rest of the bundled schema
//! comes from `tools/salesforce-doc-scraper`, but Salesforce's object
//! reference pages list an object's *fields*; they never name the child
//! relationships a parent exposes (`account.Contacts`). A describe does,
//! and is the canonical source: it is what the compiler itself resolves
//! against. Guessing instead -- pluralizing the child's name -- is wrong
//! often enough to be useless: measured against a real org, `Account`'s 85
//! child relationships include 23 that are not the child's plural
//! (`ChildAccounts`, `Shares`, `ProvidedAssets`), and six child objects
//! reach `Account` through more than one relationship.
//!
//! **Standard only.** An org's describe also reports every custom object
//! and every installed package's objects. Anything whose name contains
//! `__` (a custom object, or a namespaced package one) is dropped on both
//! sides of a relationship, so the bundled result is the same for every
//! org and carries nothing org-specific.
//!
//! **Through the `sf` CLI, in parallel.** The REST API would batch 25
//! describes per call, but the CLI redacts the access token a direct call
//! would need (`sf org display` prints `[REDACTED]`), and its own REST
//! passthrough rejects the composite path. So this shells out to
//! `sf sobject describe` once per object -- ~2.8s each, over an hour
//! sequentially -- across [`WORKERS`] threads, which brings a full run to
//! a few minutes.
//!
//! Usage: `cargo run -p standard-child-relationships -- [org-alias]`
//! (default alias `org`), with the `sf` CLI already authenticated.

use std::collections::BTreeMap;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Enough to keep the network busy without flooding the org: each worker
/// is one `sf` process, and the work is entirely wait-on-Salesforce.
const WORKERS: usize = 12;

fn main() {
    let alias = std::env::args().nth(1).unwrap_or_else(|| "org".to_string());
    let objects = standard_object_names(&alias);
    eprintln!("standard objects: {}", objects.len());

    // parent -> relationship name -> child object
    let out: Mutex<BTreeMap<String, BTreeMap<String, String>>> = Mutex::new(BTreeMap::new());
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(parent) = objects.get(i) else {
                    return;
                };
                let relationships = child_relationships(&alias, parent);
                let finished = done.fetch_add(1, Ordering::Relaxed) + 1;
                if finished % 25 == 0 {
                    eprint!("\rdescribed {finished}/{}", objects.len());
                }
                if relationships.is_empty() {
                    continue;
                }
                out.lock()
                    .expect("no panics while holding this")
                    .entry(parent.clone())
                    .or_default()
                    .extend(relationships);
            });
        }
    });
    eprintln!();

    let out = out.into_inner().expect("no panics while holding this");
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/apex-stdlib/data/standard_child_relationships.json"
    );
    let json = serde_json::to_string_pretty(&out).expect("serializable");
    std::fs::write(path, json).expect("write the bundled data");
    let total: usize = out.values().map(BTreeMap::len).sum();
    eprintln!("{total} relationships on {} parents -> {path}", out.len());
}

/// A name Salesforce ships, rather than one this org happens to have: a
/// custom object or a packaged one always carries `__` (`__c`, `__mdt`,
/// `ns__Thing__c`, and `__r` on a relationship name).
fn is_standard(name: &str) -> bool {
    !name.contains("__")
}

/// `sf`, whichever way this platform spells it: on Windows the CLI is a
/// `sf.cmd` shim, which a bare `sf` never finds.
fn sf(args: &[&str]) -> Option<serde_json::Value> {
    let out = ["sf", "sf.cmd", "sf.exe"]
        .iter()
        .find_map(|exe| Command::new(exe).args(args).output().ok())?;
    serde_json::from_slice(&out.stdout).ok()
}

fn standard_object_names(alias: &str) -> Vec<String> {
    let json = sf(&["sobject", "list", "-o", alias, "-s", "all", "--json"])
        .expect("run `sf sobject list` -- is the sf CLI installed and the org authenticated?");
    json["result"]
        .as_array()
        .expect("an object list")
        .iter()
        .filter_map(|name| name.as_str())
        .filter(|name| is_standard(name))
        .map(str::to_string)
        .collect()
}

/// One object's child relationships, standard ones only. An object this
/// org cannot describe is skipped rather than failing the run.
fn child_relationships(alias: &str, object: &str) -> BTreeMap<String, String> {
    let Some(json) = sf(&["sobject", "describe", "-o", alias, "-s", object, "--json"]) else {
        return BTreeMap::new();
    };
    json["result"]["childRelationships"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|relationship| {
            let name = relationship["relationshipName"].as_str()?;
            let child = relationship["childSObject"].as_str()?;
            (is_standard(name) && is_standard(child))
                .then(|| (name.to_string(), child.to_string()))
        })
        .collect()
}
