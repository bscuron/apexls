mod apex_reference;
mod http;
mod llms_index;
mod model;
mod object_reference;
mod toc;

use http::Client;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

const APEXREF_DOC_SET: &str = "atlas.en-us.apexref.meta";
const OBJECT_REFERENCE_DOC_SET: &str = "atlas.en-us.object_reference.meta";

struct Args {
    out: PathBuf,
    limit: Option<usize>,
}

fn parse_args(rest: &[String]) -> Args {
    let mut out = PathBuf::from("out.json");
    let mut limit = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--out" => {
                out = PathBuf::from(rest.get(i + 1).expect("--out needs a value"));
                i += 2;
            }
            "--limit" => {
                limit = Some(
                    rest.get(i + 1)
                        .expect("--limit needs a value")
                        .parse()
                        .expect("--limit must be a number"),
                );
                i += 2;
            }
            other => {
                eprintln!("warning: ignoring unrecognized argument {other:?}");
                i += 1;
            }
        }
    }
    Args { out, limit }
}

fn main() {
    let mut args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: salesforce-doc-scraper <scrape-apex-reference|scrape-object-reference> [--out PATH] [--limit N]");
        std::process::exit(1);
    }
    let command = args.remove(1);
    let opts = parse_args(&args[1..]);
    let client = Client::new(".cache", Duration::from_millis(300));

    match command.as_str() {
        "scrape-apex-reference" => scrape_apex_reference(&client, &opts),
        "scrape-object-reference" => scrape_object_reference(&client, &opts),
        other => {
            eprintln!("unknown command {other:?}");
            std::process::exit(1);
        }
    }
}

fn scrape_apex_reference(client: &Client, opts: &Args) {
    eprintln!("fetching page index...");
    let mut pages = llms_index::fetch_pages(client, APEXREF_DOC_SET).expect("failed to fetch page index");
    if let Some(limit) = opts.limit {
        pages.truncate(limit);
    }
    eprintln!("found {} distinct Apex Reference pages", pages.len());

    let mut classes = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        eprintln!("[{}/{}] {} ({})", i + 1, pages.len(), page.title, page.page_id);
        match fetch_content(client, "apexref", &page.page_id) {
            Ok((title, content)) => {
                let class = apex_reference::parse_class_page(&page.page_id, &title, &content);
                classes.push(class);
            }
            Err(e) => eprintln!("  skipping {}: {e}", page.page_id),
        }
    }

    eprintln!("reattaching orphaned members via the TOC hierarchy...");
    let parents = toc::fetch_parent_map(client, APEXREF_DOC_SET).expect("failed to fetch TOC for reattachment");
    let before = classes.len();
    let classes = apex_reference::reattach_orphaned_members(classes, &parents);
    eprintln!("  {} orphaned member page(s) merged back into their real class/interface/enum", before - classes.len());

    let json = serde_json::to_string_pretty(&classes).expect("serialize failed");
    fs::write(&opts.out, json).expect("write failed");
    eprintln!("wrote {} classes to {}", classes.len(), opts.out.display());
}

fn scrape_object_reference(client: &Client, opts: &Args) {
    eprintln!("fetching page index...");
    let mut pages =
        llms_index::fetch_pages(client, OBJECT_REFERENCE_DOC_SET).expect("failed to fetch page index");
    if let Some(limit) = opts.limit {
        pages.truncate(limit);
    }
    eprintln!("found {} distinct Object Reference pages", pages.len());

    let mut objects = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        eprintln!("[{}/{}] {} ({})", i + 1, pages.len(), page.title, page.page_id);
        match fetch_content(client, "object_reference", &page.page_id) {
            Ok((title, content)) => {
                let object = object_reference::parse_object_page(&page.page_id, &title, &content);
                objects.push(object);
            }
            Err(e) => eprintln!("  skipping {}: {e}", page.page_id),
        }
    }

    let json = serde_json::to_string_pretty(&objects).expect("serialize failed");
    fs::write(&opts.out, json).expect("write failed");
    eprintln!("wrote {} objects to {}", objects.len(), opts.out.display());
}

/// Fetches one page's content, discovering the current doc-set version
/// (once, the caller is expected to reuse the client's cache across
/// calls within a run) via the doc-set's own root `get_document` call.
fn fetch_content(client: &Client, deliverable: &str, page_id: &str) -> Result<(String, String), String> {
    let doc_version = doc_version(client, deliverable)?;
    let url = format!(
        "https://developer.salesforce.com/docs/get_document_content/{deliverable}/{page_id}.htm/en-us/{doc_version}"
    );
    let body = client.get_text(&url)?;
    let parsed: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("invalid JSON for {page_id}: {e}"))?;
    let title = parsed["title"].as_str().unwrap_or_default().to_string();
    let content = parsed["content"].as_str().unwrap_or_default().to_string();
    Ok((title, content))
}

fn doc_version(client: &Client, deliverable: &str) -> Result<String, String> {
    let doc_set = match deliverable {
        "apexref" => APEXREF_DOC_SET,
        "object_reference" => OBJECT_REFERENCE_DOC_SET,
        other => return Err(format!("unknown deliverable {other:?}")),
    };
    let url = format!("https://developer.salesforce.com/docs/get_document/{doc_set}");
    let body = client.get_text(&url)?;
    let parsed: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("invalid JSON for doc-set root: {e}"))?;
    parsed["version"]["doc_version"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "doc-set root JSON missing version.doc_version".to_string())
}
