//! Directory names that can be pruned outright when searching a
//! Salesforce repo for `.cls`/`.trigger` files -- or, via
//! [`should_skip_dir_keep_metadata_dirs`], the same list minus
//! `objects`/`fields`/`pages`, for callers that also want
//! `.object-meta.xml`/`.field-meta.xml`/`.page` files out of the same
//! walk.
//!
//! Two categories:
//!
//! - Known Metadata API type folders (`ApexClass`/`ApexTrigger` are the
//!   *only* two types whose source lives in `.cls`/`.trigger` files; every
//!   other metadata type is XML, or in the `aura`/`lwc` bundle cases,
//!   JS/HTML/CSS). Cross-checked against a real ~1000-file corpus
//!   (SalesforceFoundation/NPSP) rather than memory alone; see
//!   `crates/apex-discover/tests/matches_naive_walk.rs` for the empirical
//!   proof that pruning these never drops a real `.cls`/`.trigger` file.
//! - Tooling/VCS directories (`node_modules`, hidden dot-directories like
//!   `.git`/`.sfdx`/`.vscode`, ...) that are never part of the metadata
//!   tree at all, and can be large enough that skipping them matters more
//!   for performance than any single metadata folder does. Hidden
//!   directories are pruned as a general rule -- no real Salesforce
//!   metadata type folder is ever dot-prefixed -- rather than as an
//!   enumerated list, since new dot-directories (`.husky`, `.circleci`,
//!   editor/tooling config, ...) show up constantly and shouldn't need a
//!   list update to be pruned.
//!
//! Matching is case-insensitive: Salesforce folder names are
//! conventionally an exact fixed casing, but filesystems (notably
//! Windows) don't reliably preserve or enforce it, and there's no
//! correctness cost to leniency here -- no metadata folder name collides
//! with an Apex-source-bearing one under a different case.

use phf::phf_set;

const MAX_NAME_LEN: usize = 40;

static SKIP: phf::Set<&'static str> = phf_set! {
    // ---- Metadata API types that can never contain ApexClass/ApexTrigger ----
    "aura",
    "businessprocesses",
    "certs",
    "compactlayouts",
    "components",
    "connectedapps",
    "csptrustedsites",
    "custommetadata",
    "custompermissions",
    "dashboards",
    "documents",
    "duplicaterules",
    "email",
    "featureparameters",
    "fields",
    "fieldsets",
    "flexipages",
    "flows",
    "globalvaluesets",
    "globalvaluesettranslations",
    "groups",
    "homepagelayouts",
    "labels",
    "layouts",
    "letterhead",
    "listviews",
    "lwc",
    "matchingrules",
    "namedcredentials",
    "networks",
    "notificationtypes",
    "objects",
    "objecttranslations",
    "pages",
    "permissionsets",
    "profiles",
    "prompts",
    "quickactions",
    "queues",
    "recordtypes",
    "remotesitesettings",
    "reporttypes",
    "reports",
    "roles",
    "searchlayouts",
    "settings",
    "sharingreasons",
    "sharingrules",
    "sites",
    "standardvaluesets",
    "standardvaluesettranslations",
    "staticresources",
    "tabs",
    "translations",
    "validationrules",
    "weblinks",
    "workflows",

    // ---- Tooling/VCS directories, never part of the metadata tree ----
    "node_modules",
};

/// Should `name` (a single path component, not a full path) be pruned
/// rather than recursed into? True for every hidden (dot-prefixed)
/// directory, plus anything in [`SKIP`].
pub(crate) fn should_skip_dir(name: &str) -> bool {
    is_hidden(name) || in_skip_set(name)
}

/// Same as [`should_skip_dir`], except `objects`/`fields`/`pages` are
/// never pruned -- for callers that also need `.object-meta.xml`/
/// `.field-meta.xml`/`.page` files, which is the entire reason those
/// three directories are in [`SKIP`] in the first place (Apex source can
/// never live there, but SObject/field metadata and Visualforce markup
/// only ever live there respectively).
pub(crate) fn should_skip_dir_keep_metadata_dirs(name: &str) -> bool {
    if is_hidden(name) {
        return true;
    }
    if name.eq_ignore_ascii_case("objects")
        || name.eq_ignore_ascii_case("fields")
        || name.eq_ignore_ascii_case("pages")
    {
        return false;
    }
    in_skip_set(name)
}

fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

fn in_skip_set(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_NAME_LEN || !bytes.is_ascii() {
        return false;
    }
    let mut buf = [0u8; MAX_NAME_LEN];
    let lower = &mut buf[..bytes.len()];
    for (dst, &src) in lower.iter_mut().zip(bytes) {
        *dst = src.to_ascii_lowercase();
    }
    let lower = std::str::from_utf8(lower).expect("ASCII input is valid UTF-8");
    SKIP.contains(lower)
}
