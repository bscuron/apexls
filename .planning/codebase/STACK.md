# Technology Stack

**Analysis Date:** 2026-09-01

## Languages

**Primary:**
- Rust 2021 edition - Core language for the entire workspace; used for lexer, parser, binder, and language server

**Secondary:**
- Shell scripts - CI/CD automation in GitHub Actions

## Runtime

**Environment:**
- Standalone Rust binaries (no external runtime dependency)
- Single-threaded event loop (tokio async runtime) for LSP server
- Multi-threaded parallelism via rayon for batch processing

**Package Manager:**
- Cargo (Rust's built-in package manager)
- Lockfile: `Cargo.lock` present

## Frameworks

**Core LSP:**
- async-lsp 0.2.4 - Language Server Protocol implementation with async support
  - Features: client-monitor, omni-trait, stdio, tracing, tokio
- lsp-types 0.95.0 - Protocol types and data structures
- tower 0.5 - Middleware abstraction for LSP

**Async Runtime:**
- tokio 1.27.0 - Async runtime for server
  - Features: io-std, io-util, macros, process, rt, time
- tokio-util 0.7.8 - Tokio utilities (compat layer)

**Parsing & Syntax:**
- rowan 0.17.0 - Lossless green/red syntax tree (same library rust-analyzer uses)
  - O(1)-clone, structurally-shared immutable trees for incremental reparsing
- Used in `apex-syntax`, `apex-parser`, `apex-binder`

**Testing & Benchmarking:**
- criterion 0.8.2 - Benchmarking framework (with HTML reports)
  - Used in: `apex-parser`, `apex-lexer`, `apex-discover`, `apex-binder`
- insta 1.48.0 - Snapshot testing (with glob feature)
  - Used in: `apex-parser`
- proptest 1.11.0 - Property-based testing
  - Used in: `apex-parser`

**Build & Dev:**
- GitHub Actions (ubuntu-latest) - CI/CD platform
- dtolnay/rust-toolchain@stable - Rust toolchain management
- Swatinem/rust-cache@v2 - Build cache
- taiki-e/install-action@cargo-llvm-cov - Code coverage tool

## Key Dependencies

**Performance & Efficiency:**
- rayon 1.12.0 - Data parallelism for multi-pass binding and discovery
  - Used in: `apexls`, `apex-binder`, `apex-discover`, `apex-metadata`, test suites
- hotpath 0.24.0 - Profiling/instrumentation for hot paths
  - Features: hotpath, hotpath-cpu, hotpath-alloc (optional)
  - Used in: `apex-lexer`, `apex-parser`, `apex-binder`, `apex-discover`
- memchr 2.8.3 - SIMD-accelerated byte/substring search
  - Same crate used by ripgrep and regex
  - Used in `apex-lexer` for comment/string delimiter scanning

**String & Hash Optimization:**
- smol_str 0.3.6 - Small-string optimization to reduce allocations
  - Used in: `apex-stdlib`, `apex-metadata`, `apex-binder`, `apex-syntax`
- rustc-hash 2.1.3 - FxHash (fast, simple hash function)
  - Used in: `apex-binder` for symbol table hashing
- hashbrown 0.17.1 - Faster, more memory-efficient hashmap/hashset
  - Used in: `apex-binder`
- phf 0.11 - Compile-time perfect hash map for keyword lookup
  - Features: macros
  - Used in: `apex-lexer`, `apex-discover` for O(1) collision-free lookup

**Type System & Serialization:**
- serde 1.0 - Serialization/deserialization framework
  - Used in: `apex-stdlib`, `tools/salesforce-doc-scraper`
- serde_json 1.0.95 - JSON support
  - Used in: `apexls-server`, `apex-stdlib`, `tools/salesforce-doc-scraper`
- num_enum 0.7.6 - Derive-based enum-to-int conversion (safe alternative to transmute)
  - Used in: `apex-syntax` for rowan::Language conversion

**File System & Discovery:**
- ignore 0.4.33 - Respects .gitignore for efficient project discovery
  - Used in: `apex-discover`
- notify 8.2.0 - File system event notifications
  - Used in: `apexls-server` for watching source file changes
- notify-debouncer-full 0.7.0 - Debounced file system watcher
  - Used in: `apexls-server` to coalesce rapid file changes

**Other Utilities:**
- futures 0.3.28 - Future combinators and async utilities
  - Used in: `apexls-server`
- parking_lot 0.12 - Faster, more ergonomic synchronization primitives
  - Used in: `apexls-server` for thread-safe coordination
- tracing 0.1.37 - Distributed tracing framework
  - Used in: `apexls-server`
- tracing-subscriber 0.3.16 - Tracing log sink and filters
  - Used in: `apexls-server`

**CLI:**
- clap 4.6.6 - Command-line argument parsing
  - Features: derive
  - Used in: `apexls` for CLI subcommands (server, ast, dead)

**Web Scraping (tools only):**
- ureq 2 - Minimal HTTP client (no async, no dependencies)
  - Used in: `tools/salesforce-doc-scraper` with on-disk response cache
- scraper 0.20 - HTML/CSS selector library
  - Used in: `tools/salesforce-doc-scraper` for DOM parsing

**Metadata Parsing:**
- roxmltree 0.20 - Fast, low-allocation XML parser
  - Used in: `apex-metadata` for reading Salesforce package.xml and metadata files

**Profiling & Testing (dev-only):**
- dhat 0.3.3 - Heap profiler integration with criterion
  - Used in: `apex-binder` benchmarks

## Configuration

**Environment:**
- Workspace members reference each other via path dependencies
- No environment variables required at runtime
- No .env file usage

**Build:**
- `Cargo.toml` - Workspace root with resolver 2.0 (new dependency resolution)
- `Cargo.lock` - Locked versions for reproducible builds
- GitHub Actions CI configured in `.github/workflows/ci.yml`
  - Runs cargo fmt, clippy, build, test on every PR and push to main
  - Separate coverage job generates llvm-cov HTML reports

**Linting & Formatting:**
- `cargo fmt` - Enforced via CI (fail on --check)
- `cargo clippy` - Enforced via CI with `-D warnings` (deny all warnings)

## Platform Requirements

**Development:**
- Rust 1.70+ (stable toolchain, via dtolnay/rust-toolchain@stable)
- rustfmt and clippy components required
- Unix-like shell for build scripts
- Windows 11 / Linux / macOS (development machine runs Windows 11)

**Production:**
- Deployment target: Language server protocol client (VS Code, Sublime, Vim, etc.)
- Distributed as standalone binaries:
  - `apexls` - CLI tool with server/ast/dead subcommands
  - `apexls-server` - Compatible standalone LSP server for editor integration
- No external runtime dependency (statically linked or dynamically linked against glibc on Linux)

**CI/CD:**
- GitHub Actions on ubuntu-latest
- Coverage: llvm-cov with HTML report artifacts

---

*Stack analysis: 2026-09-01*
