//! Reading the journals a listing allows, several at a time, into totals.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use forge_durable::TurnRecordFilter;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use super::aggregate::{Aggregator, TurnFacts};
use super::{ModelReport, ReportOptions, SkippedJournal};
use crate::workspace_journals::{JournalListing, WorkspaceJournals};

/// The report for every session journal in `journal_dir`. A missing
/// directory is an empty report (and is not created); a journal that cannot
/// be read is listed in `skipped`, and so is one that is a symbolic link:
/// Forge writes journals as files, and following a link could pull in
/// another project's journal, or any file, as this one's.
pub async fn model_report_in(
    journal_dir: &Path,
    options: &ReportOptions,
) -> anyhow::Result<ModelReport> {
    report_of(&WorkspaceJournals::at(journal_dir.to_path_buf()), options).await
}

/// The report for `journals`, refused as their listing is.
pub(super) async fn report_of(
    journals: &WorkspaceJournals,
    options: &ReportOptions,
) -> anyhow::Result<ModelReport> {
    let (totals, skipped) = totals_in(journals, options).await?;
    Ok(ModelReport {
        journal_dir: journals.dir().to_path_buf(),
        since: options.since,
        min_turns: options.min_turns,
        kinds: totals.finish(options.min_turns),
        skipped,
    })
}

/// The running totals of every session journal `journals` may read, and
/// the journals skipped (see [`WorkspaceJournals::list`]) or unreadable.
pub(super) async fn totals_in(
    journals: &WorkspaceJournals,
    options: &ReportOptions,
) -> anyhow::Result<(Aggregator, Vec<SkippedJournal>)> {
    let JournalListing {
        journals,
        mut skipped,
    } = journals.list().await?;
    let mut totals = Aggregator::default();
    for (path, read) in read_journals(journals, options).await? {
        match read {
            Ok(journal) => totals.merge(journal),
            Err(error) => skipped.push(SkippedJournal { path, error }),
        }
    }
    skipped.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((totals, skipped))
}

/// Each of `journals`' totals, read [`CONCURRENT_JOURNALS`] at a time; an
/// error says why a journal could not be read.
pub(crate) async fn read_journals(
    journals: Vec<PathBuf>,
    options: &ReportOptions,
) -> anyhow::Result<Vec<(PathBuf, Result<Aggregator, String>)>> {
    let permits = Arc::new(Semaphore::new(CONCURRENT_JOURNALS));
    let mut reads = JoinSet::new();
    for path in journals {
        let permit = permits.clone().acquire_owned().await?;
        let options = options.clone();
        reads.spawn(async move {
            let totals = read_journal(&path, &options).await;
            drop(permit);
            (path, totals)
        });
    }
    let mut results = Vec::new();
    while let Some(read) = reads.join_next().await {
        results.push(read?);
    }
    Ok(results)
}

/// Journals read at once. Not tied to the number of CPUs: a journal's read
/// is mostly waiting — sqlx runs each connection on a thread of its own and
/// every query is a hand-off to it and back, slow across cores — so more
/// reads in flight than cores still helps. Measured on 10,000 journals,
/// 8 at a time was fastest on 1, 2 and 4 CPUs (3.5 s, 4.5 s, 4.4 s against
/// 4.1 s, 8.2 s, 8.4 s one at a time), for the same CPU time on 1–2 CPUs
/// and about 1.7 times as much on 4.
const CONCURRENT_JOURNALS: usize = 8;

/// One journal's totals. Streamed: only the running totals are held. A
/// journal with a row whose values the report uses do not decode counts for
/// nothing — its totals are dropped, not merged — and the error says why.
/// Columns the report does not read (task id, start time, tier, cache
/// tokens, model calls) are not checked: a bad value there cannot change a
/// number in the report, and does not cost the session its other turns.
/// [`forge_durable::read_turn_records`] checks every column.
async fn read_journal(path: &Path, options: &ReportOptions) -> Result<Aggregator, String> {
    let filter = TurnRecordFilter {
        ended_since: options.since,
        task_kind: options.kind.clone(),
    };
    let mut totals = Aggregator::default();
    let mut bad_row: Option<String> = None;
    let scanned = forge_durable::scan_turn_records(path, &filter, |view| {
        if bad_row.is_some() {
            return;
        }
        match TurnFacts::of_view(&view) {
            Ok(turn) => totals.add(&turn, options),
            Err(error) => bad_row = Some(format!("signals are not valid: {error}")),
        }
    })
    .await;
    match (scanned, bad_row) {
        (Err(error), _) => Err(error.to_string()),
        (Ok(()), Some(error)) => Err(error),
        (Ok(()), None) => Ok(totals),
    }
}
