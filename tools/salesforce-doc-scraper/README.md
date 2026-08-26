# Salesforce documentation scraper

Fetches Apex standard-library method signatures (Apex Reference Guide) and
standard SObject/field schema (Object Reference) directly from Salesforce's
own public documentation API — `developer.salesforce.com`'s
`get_document`/`get_document_content` endpoints, the same ones the doc
site's own frontend uses — rather than depending on a third-party project's
derived stub library or a live org connection.

A standalone, manually-run, offline tool: `apexls` itself never makes a
network call at runtime. Re-run this after a Salesforce release to refresh
the checked-in data it produces.

## Usage

```
cargo run -p salesforce-doc-scraper -- scrape-apex-reference --out out/apex_reference.json
cargo run -p salesforce-doc-scraper -- scrape-object-reference --out out/standard_objects.json
```

Add `--limit N` to process only the first `N` pages (useful for a quick
smoke test before committing to a full run, which fetches 500+ pages).

Responses are cached on disk under `.cache/` (gitignored) so repeat runs
don't re-fetch unchanged pages — delete that directory to force a fresh
scrape.

## Scope

This tool only fetches and parses; it does not decide how `apex-binder`
consumes the output. See the project's own planning notes for that
follow-on work.
