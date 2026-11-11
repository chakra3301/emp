//! Runs importers and applies their output to the store.

use std::time::Instant;

use anyhow::Result;
use emp_core::UpsertOutcome;
use emp_import::ScanCtx;
use emp_protocol::{
    ImportProgress, ImportReport, ImportSourceKind, ImportSourceReport, RpcNotification, notifications,
};

use crate::state::State;

/// Imports `kinds` (all sources when empty), one source at a time. A failing source is
/// reported and doesn't stop the others. Concurrent calls wait for the running import.
pub async fn run(state: &State, kinds: &[ImportSourceKind]) -> Result<ImportReport> {
    let _guard = state.import_lock.lock().await;
    let kinds: Vec<ImportSourceKind> =
        if kinds.is_empty() { ImportSourceKind::ALL.to_vec() } else { kinds.to_vec() };

    let mut sources = Vec::new();
    for kind in kinds {
        let started = Instant::now();
        progress(state, kind, "scanning");
        let outcome = run_one(state, kind).await;
        let report = match outcome {
            Ok(stats) => {
                progress(
                    state,
                    kind,
                    &format!(
                        "{} threads ({} new, {} updated), {} agents",
                        stats.threads_seen, stats.threads_added, stats.threads_updated, stats.agents_seen
                    ),
                );
                ImportSourceReport { source: kind, stats, error: None, duration_ms: started.elapsed().as_millis() as i64 }
            }
            Err(e) => {
                tracing::warn!(source = kind.as_str(), "import failed: {e:#}");
                progress(state, kind, &format!("failed: {e:#}"));
                ImportSourceReport {
                    source: kind,
                    stats: Default::default(),
                    error: Some(format!("{e:#}")),
                    duration_ms: started.elapsed().as_millis() as i64,
                }
            }
        };
        sources.push(report);
    }
    let report = ImportReport { sources };
    state.notify(RpcNotification::new(notifications::IMPORT_FINISHED, &report));
    Ok(report)
}

async fn run_one(state: &State, kind: ImportSourceKind) -> Result<emp_protocol::ImportStats> {
    let importer = emp_import::by_kind(kind);
    if !importer.found() {
        return Ok(Default::default());
    }
    let (known, run_id) = state
        .with_store(move |s| Ok((s.fingerprints(kind)?, s.begin_import_run(kind)?)))
        .await?;

    let notify_state = state.clone();
    let scanned = tokio::task::spawn_blocking(move || -> Result<ScanCtx> {
        let mut ctx = ScanCtx::new(known).with_progress(move |msg| progress(&notify_state, kind, msg));
        importer.scan(&mut ctx)?;
        Ok(ctx)
    })
    .await?;

    let mut ctx = match scanned {
        Ok(ctx) => ctx,
        Err(e) => {
            let msg = format!("{e:#}");
            state.with_store(move |s| s.finish_import_run(run_id, "{}", Some(&msg))).await?;
            return Err(e);
        }
    };

    let threads = std::mem::take(&mut ctx.threads);
    let agents = std::mem::take(&mut ctx.agents);
    let outcomes = state.with_store(move |s| s.apply_import(&threads, &agents)).await?;
    for o in outcomes {
        match o {
            UpsertOutcome::Added => ctx.stats.threads_added += 1,
            UpsertOutcome::Updated => ctx.stats.threads_updated += 1,
            UpsertOutcome::Unchanged => ctx.stats.threads_unchanged += 1,
        }
    }
    let stats = ctx.stats;
    let stats_json = serde_json::to_string(&stats)?;
    state.with_store(move |s| s.finish_import_run(run_id, &stats_json, None)).await?;
    Ok(stats)
}

fn progress(state: &State, source: ImportSourceKind, message: &str) {
    state.notify(RpcNotification::new(
        notifications::IMPORT_PROGRESS,
        ImportProgress { source, message: message.to_string() },
    ));
}
