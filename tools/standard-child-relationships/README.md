# standard-child-relationships

Regenerates `crates/apex-stdlib/data/standard_child_relationships.json` —
every standard object's child relationships (`Account.Contacts` →
`Contact`), which the binder uses to type a parent's child collection as
`List<Child>` so `.isEmpty()`/`.size()` on it resolve.

```sh
cargo run -p standard-child-relationships -- [org-alias]   # default alias: org
```

Needs the `sf` CLI authenticated against any org. The output is the same
whatever org is used: anything whose name contains `__` — a custom object,
a packaged one, a `__r` relationship — is dropped on both sides, so only
names Salesforce itself ships are bundled.

**Why an org, not the doc scrape.** The rest of the bundled schema comes
from `tools/salesforce-doc-scraper`, but Salesforce's object reference
pages list an object's *fields* and never name the child relationships a
parent exposes. A describe does, and is what the compiler resolves
against. Guessing — pluralizing the child's name — is wrong too often to
use: of `Account`'s 85 child relationships, 23 are not the child's plural
(`ChildAccounts`, `Shares`, `ProvidedAssets`), and six child objects reach
`Account` through more than one relationship.

**Why it shells out per object.** The REST API would batch 25 describes
per call, but the CLI redacts the access token a direct call needs (`sf
org display` prints `[REDACTED]`) and its REST passthrough rejects the
composite path. One `sf sobject describe` costs ~2.8s, so a full run is
spread across 12 threads: a few minutes for ~1,600 objects, rather than
over an hour.

A run against a Developer Edition org produced 4,678 relationships on 580
parents (221 KB).
