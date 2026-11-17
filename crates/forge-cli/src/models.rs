//! `forge models report`: the models this workspace's sessions used, ranked
//! for each kind of task (`forge_session::model_report`), as a table or JSON.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use clap::{Args, Subcommand};
use forge_config::{Config, ConfigOverrides};
use forge_session::model_report::{
    model_report, parse_since, KindReport, ModelReport, RankedModel, ReportOptions,
    UntrustedWorkspace, DEFAULT_MIN_TURNS,
};
use forge_types::TaskKind;

use crate::ExitCode;

#[derive(Debug, Subcommand)]
pub(crate) enum ModelsCommand {
    /// Rank the models for each kind of task (main, subagent, custom:<name>)
    /// by how their turns went: completion, escalation, verification,
    /// approvals, queued messages, tokens and time.
    Report(ReportArgs),
}

#[derive(Args, Debug)]
pub(crate) struct ReportArgs {
    /// The workspace whose sessions to report on.
    #[arg(long, default_value = ".", value_name = "PATH")]
    workspace: PathBuf,

    /// Read the session journals in this directory instead of the
    /// workspace's own.
    #[arg(long, value_name = "PATH")]
    journal: Option<PathBuf>,

    /// Only this kind of task: `main`, `subagent` or `custom:<agent name>`.
    #[arg(long, value_name = "KIND", value_parser = parse_kind)]
    kind: Option<TaskKind>,

    /// Only turns that ended since a date (`2025-06-01`) or within a number
    /// of days or hours (`7d`, `24h`).
    #[arg(long, value_name = "WHEN", value_parser = parse_since_now)]
    since: Option<DateTime<Utc>>,

    /// Groups with fewer turns are listed but not ranked.
    #[arg(long, default_value_t = DEFAULT_MIN_TURNS, value_name = "N")]
    min_turns: usize,

    /// Print the report as JSON.
    #[arg(long)]
    json: bool,
}

fn parse_kind(text: &str) -> Result<TaskKind, String> {
    text.parse()
        .map_err(|error: forge_types::TaskKindError| error.to_string())
}

fn parse_since_now(text: &str) -> Result<DateTime<Utc>, String> {
    parse_since(text, Utc::now())
}

pub(crate) async fn run(command: ModelsCommand) -> anyhow::Result<ExitCode> {
    let ModelsCommand::Report(args) = command;
    // Like any path argument, `--journal` is relative to where the command
    // runs; absolute, it is a directory the user named, read without trust.
    let journal = args
        .journal
        .as_deref()
        .map(std::path::absolute)
        .transpose()?;
    let cfg = Config::load(ConfigOverrides {
        workspace: Some(args.workspace),
        journal_path: journal.as_ref().map(|path| path.display().to_string()),
        ..Default::default()
    })
    .map_err(|e| anyhow::anyhow!(e))?;
    let options = ReportOptions {
        kind: args.kind,
        since: args.since,
        min_turns: args.min_turns,
    };
    let report = match model_report(&cfg, &options).await {
        Ok(report) => report,
        Err(error) => {
            if let Some(untrusted) = error.downcast_ref::<UntrustedWorkspace>() {
                eprintln!(
                    "error: {}\nOpen it in Forge and trust it first, or pass --journal <DIR> \
                     to read a journal directory you choose.",
                    printable(&untrusted.to_string())
                );
                return Ok(ExitCode::Failed);
            }
            return Err(error);
        }
    };
    for skipped in &report.skipped {
        eprintln!(
            "warning: skipped {}: {}",
            printable(&skipped.path.display().to_string()),
            printable(&skipped.error)
        );
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render_table(&report));
    }
    Ok(ExitCode::Success)
}

// ------------------------------------------------------------------ table

const HEADER: [&str; 13] = [
    "RANK",
    "MODEL",
    "PROVIDER",
    "EFFORT",
    "TURNS",
    "SCORE",
    "DONE",
    "ESC",
    "VERIFY",
    "APPROVALS",
    "QUEUED",
    "TOKENS",
    "ACTIVE",
];

/// Columns aligned to the right (numbers); the rest to the left.
const RIGHT_ALIGNED: [bool; 13] = [
    true, false, false, false, true, true, true, true, true, true, true, true, true,
];

const NOT_ENOUGH_DATA: &str = "not enough data";

/// `text` safe to print to a terminal: every control character (C0, DEL,
/// C1) — which a terminal would act on (clear the screen, retitle the
/// window, write the clipboard with OSC 52, …) — shown as its escape
/// (`\u{1b}`, `\r`) instead. Names come from journals and, through them,
/// from provider model catalogs, so none of them is trusted.
pub(crate) fn printable(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

/// A rate as a whole percentage, halves rounded up.
fn percent(rate: f64) -> String {
    format!("{}%", (rate * 100.0).round() as i64)
}

fn cells(model: &RankedModel) -> [String; 13] {
    let dash = || "-".to_string();
    [
        model.rank.map_or_else(dash, |rank| rank.to_string()),
        printable(&model.model),
        printable(&model.provider),
        model
            .effort
            .as_deref()
            .map_or_else(|| "default".into(), printable),
        model.turns.to_string(),
        model.score.map_or_else(dash, |score| format!("{score:.2}")),
        percent(model.completion_rate),
        percent(model.escalation_rate),
        format!(
            "{}/{}",
            model.verification_passed, model.verification_failed
        ),
        format!("{}/{}", model.approvals_granted, model.approvals_denied),
        percent(model.queued_rate),
        model
            .median_tokens
            .map_or_else(dash, |tokens| format!("{}", tokens.round() as u64)),
        model
            .median_active_ms
            .map_or_else(dash, |ms| format!("{:.1}s", ms / 1000.0)),
    ]
}

fn render_kind(kind: &KindReport, out: &mut String) {
    out.push_str(&format!(
        "{} ({} turns)\n",
        printable(&kind.kind.to_string()),
        kind.turns
    ));
    let rows: Vec<[String; 13]> = kind.models.iter().map(cells).collect();
    let mut widths = HEADER.map(str::len);
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let line = |cells: &[String], note: Option<&str>| {
        let mut line = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i > 0 {
                line.push_str("  ");
            }
            let pad = " ".repeat(widths[i].saturating_sub(cell.chars().count()));
            if RIGHT_ALIGNED[i] {
                line.push_str(&pad);
                line.push_str(cell);
            } else {
                line.push_str(cell);
                line.push_str(&pad);
            }
        }
        if let Some(note) = note {
            line.push_str("  ");
            line.push_str(note);
        }
        line.trim_end().to_string() + "\n"
    };
    let header: Vec<String> = HEADER.iter().map(|h| h.to_string()).collect();
    out.push_str(&line(&header, None));
    for (model, row) in kind.models.iter().zip(&rows) {
        let note = model.rank.is_none().then_some(NOT_ENOUGH_DATA);
        out.push_str(&line(row, note));
    }
}

/// The report as one table per kind, or a line saying there is nothing.
fn render_table(report: &ModelReport) -> String {
    if report.kinds.is_empty() {
        return "No turn records found.\n".into();
    }
    let mut out = String::new();
    for (i, kind) in report.kinds.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        render_kind(kind, &mut out);
    }
    out.push_str(&format!(
        "\nGroups with fewer than {} turns are not ranked. Score: the mean of each \
         group's rank on the seven measures (lower is better).\n",
        report.min_turns
    ));
    out
}
