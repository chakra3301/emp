//! Smoke-test one importer against this machine's real data, read-only, in memory.
//!
//! ```sh
//! cargo run -p emp-import --example scan -- pi            # stats + 5 newest titles
//! cargo run -p emp-import --example scan -- pi --twice    # second run must be all unchanged
//! ```
//!
//! Prints counts, titles and transcript *shapes* only — never transcript text.

use std::collections::HashMap;
use std::time::Instant;

use emp_core::Store;
use emp_protocol::{ImportSourceKind, ThreadsListParams, TranscriptItem};
use emp_import::{ScanCtx, by_kind};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let kind = match args.first().map(String::as_str) {
        Some("pi") => ImportSourceKind::Pi,
        Some("hermes") => ImportSourceKind::Hermes,
        Some("claude") => ImportSourceKind::Claude,
        Some("codex") => ImportSourceKind::Codex,
        Some("antigravity") => ImportSourceKind::Antigravity,
        Some("orion") => ImportSourceKind::Orion,
        _ => anyhow::bail!("usage: scan <pi|hermes|claude|codex|orion> [--twice]"),
    };
    let twice = args.iter().any(|a| a == "--twice");
    let importer = by_kind(kind);
    println!("source {:?} found={} paths={:?}", kind, importer.found(), importer.paths());

    let mut store = Store::open_in_memory()?;
    for round in 1..=(if twice { 2 } else { 1 }) {
        let started = Instant::now();
        let known: HashMap<_, _> = store.fingerprints(kind)?;
        let mut ctx = ScanCtx::new(known);
        importer.scan(&mut ctx)?;
        let outcomes = store.apply_import(&ctx.threads, &ctx.agents)?;
        println!(
            "round {round}: {:?} in {:?}; applied {} upserts",
            ctx.stats,
            started.elapsed(),
            outcomes.len()
        );
    }

    // `SCAN_LIMIT=n` shows more threads.
    let limit = std::env::var("SCAN_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(5);
    let page = store.list_threads(&ThreadsListParams { limit: Some(limit), ..Default::default() })?;
    println!("total threads: {}", page.total);
    for t in &page.items {
        let started = Instant::now();
        let shape = match importer.transcript(t) {
            Ok(items) => {
                let mut counts: HashMap<&str, usize> = HashMap::new();
                for i in &items {
                    let k = match i {
                        TranscriptItem::User { .. } => "user",
                        TranscriptItem::Assistant { .. } => "assistant",
                        TranscriptItem::Thinking { .. } => "thinking",
                        TranscriptItem::ToolCall { .. } => "tool_call",
                        TranscriptItem::ToolResult { .. } => "tool_result",
                        TranscriptItem::System { .. } => "system",
                    };
                    *counts.entry(k).or_default() += 1;
                }
                format!("{} items {:?}", items.len(), counts)
            }
            Err(e) => format!("ERROR {e:#}"),
        };
        println!(
            "- [{}] {:?} cwd={:?} msgs={:?} model={:?} | transcript: {} ({:?})",
            t.harness.as_str(),
            t.title,
            t.cwd,
            t.message_count,
            t.model,
            shape,
            started.elapsed()
        );
    }
    for a in store.list_agents()? {
        println!(
            "agent {:?} harness={} model={:?} cwd={:?} profile={:?} instructions={} chars",
            a.name,
            a.harness.as_str(),
            a.model,
            a.cwd,
            a.harness_profile,
            a.instructions.as_deref().map_or(0, str::len)
        );
    }
    Ok(())
}
