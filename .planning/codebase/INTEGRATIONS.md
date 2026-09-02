# External Integrations

**Analysis Date:** 2026-09-01

## APIs & External Services

**Salesforce Documentation:**
- Salesforce documentation portal (developer.salesforce.com)
  - SDK/Client: `ureq` 2.0 HTTP client via `tools/salesforce-doc-scraper`
  - Purpose: Scrapes Apex language reference and SObject schema documentation
  - Endpoints: 
    - Apex reference documentation: `atlas.en-us.apexref.meta` (LLMS index)
    - Standard object reference: `atlas.en-us.object_reference.meta` (LLMS index)
  - Auth: None (public documentation); rate-limited, uses polite HTTP client with User-Agent header and request delays
  - Rate limiting: Enforced via 300ms delay between requests to avoid hammering servers
  - Caching: On-disk cache at `.cache/` (within scraper tool, keyed by URL)

**Salesforce Scratch Org (Oracle Harness):**
- Salesforce deployment validation endpoint for compiler ground truth
  - Located in: `oracle/scratch-org/` (scripts directory, currently empty placeholder)
  - Purpose: True Apex compiler validation via deploy-validate
  - Auth: Requires `sf` CLI with authenticated org connection
  - Rate limiting: Intentionally slow/sparingly used (scheduled runs only, not on every PR)
  - Usage: Adjudicates disagreements between reference parser oracles

## Data Storage

**Databases:**
- None (this is a language server, not a backend service)

**File Storage:**
- Local file system only
  - Project metadata: `SFDX` package.xml and metadata files (.xml format)
  - Source files: Apex files (.cls, .trigger, etc.)
  - Parser output: Temporary files during build (target/*)
  - Scraper cache: `.cache/` (transient, git-ignored)

**Caching:**
- None at runtime (LSP server is stateless between sessions)
- Build-time: GitHub Actions cache via `Swatinem/rust-cache@v2`
- Scraper tool: On-disk HTTP response cache for development convenience

## Authentication & Identity

**Auth Provider:**
- None required for core server operation
- CLI tool (`apexls`/`apexls-server`) requires no authentication
- Optional: Salesforce CLI (`sf`) auth for scratch org oracle (oracle harness only)
  - Uses local ~/.sfdx/config or environment for org connection
  - Not required for normal LSP operation

## Monitoring & Observability

**Error Tracking:**
- None (no external error tracking service)

**Logs:**
- tracing framework (`tracing` 0.1.37 + `tracing-subscriber` 0.3.16)
- Local terminal/file output only (client controls via editor integration)
- No external log aggregation

## CI/CD & Deployment

**Hosting:**
- GitHub (repository hosting only)
- No cloud deployment

**CI Pipeline:**
- GitHub Actions (`ubuntu-latest`)
  - On: pull requests and pushes to main branch
  - Jobs:
    1. **build-test**: fmt check, clippy, build all targets, run test suite
    2. **coverage**: llvm-cov HTML report generation with artifact upload
  - Artifacts: Coverage reports uploaded to GitHub (retention policy default)

## Environment Configuration

**Required env vars:**
- None (fully configured via CLI arguments and workspace metadata)

**Optional env vars:**
- None at runtime
- Build-time: `RUSTFLAGS` for feature toggles (hotpath profiling features)

**Secrets location:**
- None stored in repository
- Optional: `~/.sfdx/config` for Salesforce CLI auth (oracle harness only, user's machine)

## Webhooks & Callbacks

**Incoming:**
- File system watcher via `notify` + `notify-debouncer-full`
  - Watches source directory for changes (debounced)
  - Triggers reparse/rebind when files modified
  - Used by: `apexls-server` for incremental language server updates

**Outgoing:**
- LSP protocol messages to editor client
  - Diagnostics (parse/bind errors)
  - Hover information
  - Code completions
  - Symbol resolution
  - No external service calls

## Reference Parsers & Oracles

**External Parser Implementations (for differential testing):**

Located in: `oracle/adapters/`

- **@apexdevtools/apex-parser** (ANTLR-based)
  - Repository: https://github.com/apex-dev-tools/apex-parser
  - Adapter: `oracle/adapters/antlr-apexdevtools/` (Node.js subprocess)
  - Purpose: Primary oracle for correctness validation
  - Used for: Comparing parse trees, error recovery, CST structure

- **ANTLR grammars-v4 Apex grammar** (independent ANTLR grammar)
  - Repository: https://github.com/antlr/grammars-v4 (`master/apex/apex.g4`)
  - Adapter: `oracle/adapters/antlr-grammars-v4/` (Java subprocess)
  - Purpose: Secondary oracle grammar reference
  - Used for: Grammar rule validation

- **tree-sitter-sfapex** (Tree-sitter grammar)
  - Repository: https://github.com/aheber/tree-sitter-sfapex
  - Adapter: `oracle/adapters/tree-sitter-sfapex/` (subprocess)
  - Purpose: Error recovery and CST comparison
  - Used for: Validating incremental parsing and malformed input handling

**Execution Model:**
- All oracles are subprocess-based (Java/Node.js processes)
- Keeps oracle dependencies out of main Rust crate graph
- Used in test suite for differential testing only (not shipped with binaries)

## Embedded Reference Data

**Standard Library & Schema Snapshots:**

Located in: `crates/apex-stdlib/data/`

- **apex_reference.json** (Apex standard classes/methods/properties)
  - Source: Scraped from Salesforce documentation via `tools/salesforce-doc-scraper scrape-apex-reference`
  - Contains: Standard classes (System, String, List, etc.), methods, signatures, parameters
  - Embedded at compile-time via `include_str!()` macro
  - Loaded on first access via `apex_stdlib::standard_classes()` (OnceLock single initialization)
  - Purpose: Type checking, code completion, semantic diagnostics
  - Updated: Manually by running scraper on new Apex release (not automated)

- **standard_objects.json** (SObject field schema)
  - Source: Scraped from Salesforce documentation via `tools/salesforce-doc-scraper scrape-object-reference`
  - Contains: All standard SObjects (Account, Contact, etc.), field names, types, relationships
  - Embedded at compile-time via `include_str!()` macro
  - Loaded on first access via `apex_stdlib::standard_sobjects()` (OnceLock single initialization)
  - Purpose: SObject field resolution, relationship traversal
  - Updated: Manually by running scraper on new Salesforce release (not automated)

**Runtime Behavior:**
- No network calls at runtime (all data embedded in binary)
- Single parse + cache on first access (zero cost after first use)
- Panics on malformed JSON only at build-time (via tests) or on first access (rare edge case)

## Data Sources Summary

| Data | Source | Collection Method | Update Frequency | Runtime Cost |
|------|--------|-------------------|-------------------|--------------|
| Apex Standard Library | developer.salesforce.com | Web scraper (ureq + scraper) | Manual (per release) | None (embedded) |
| SObject Schema | developer.salesforce.com | Web scraper (ureq + scraper) | Manual (per release) | None (embedded) |
| SFDX Metadata | Local filesystem | File discovery (ignore crate) | File watcher (notify) | Live (incremental parsing) |
| Reference Parser Output | ANTLR/tree-sitter (subprocess) | Subprocess invocation | Test-time only | Test-only overhead |

---

*Integration audit: 2026-09-01*
