//! The sidebar background-activity strip's view-model.
//!
//! Deliberately pure: no ratatui, and no clock of its own. `now` is injected
//! and the row budget is handed in from the layout, so ordering, truncation
//! and expiry can be tested without a terminal. That matters because the rules
//! here are the ones that are easy to get subtly wrong — an ordering that
//! drops a failure, or a budget that quietly starves the transcript.

use chrono::{DateTime, Utc};
use forge_core::{BackgroundTaskKind, BackgroundTaskStatus};
use forge_session::BackgroundTaskSnapshot;
use forge_transcript::format_elapsed_tenths;
use forge_types::{BackgroundTaskId, HitlPayload};

/// How long a finished row stays in the strip after it stopped, so a
/// completion is still there when the operator looks back — without the strip
/// becoming a log of everything the session ever ran.
pub const DONE_ROW_TTL_SECS: i64 = 60;

/// The most task rows the strip will show; the layout clamps lower when the
/// sidebar is short, and the surplus becomes the header's `+N more`.
pub const STRIP_ROW_CAP: usize = 7;

/// Longest argument summary drawn on a blocked row's detail line.
const DETAIL_CHARS: usize = 28;
/// What an active subagent's activity line keeps. Longer than a request's,
/// because it is prose rather than a command, but still one line.
const ACTIVITY_CHARS: usize = 60;

/// A row's state, and its sort rank in one.
///
/// The ordering is the load-bearing part: `Failed` outranks `Active` because
/// a failure is the only other state that can need an operator, and
/// truncation must never be what hides it. Deriving the rank from the enum's
/// declaration order keeps the two from drifting apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StripState {
    /// Blocked on an operator decision.
    Blocked = 0,
    /// Terminal, and it went wrong.
    Failed = 1,
    /// Running, or waiting on nothing.
    Active = 2,
    /// Spawned but not started.
    Queued = 3,
    /// Terminal and fine. The only state that ages out on its own.
    Done = 4,
    /// Stopped, by the operator or by the session. Kept separate from `Done`
    /// because it must *not* age out: a cancellation the operator did not
    /// perform (a stopped session, an interrupted turn) leaves this row as
    /// the only evidence it happened.
    Cancelled = 5,
}

impl StripState {
    fn of(status: &BackgroundTaskStatus) -> Self {
        match status {
            BackgroundTaskStatus::WaitingForApproval { .. } => Self::Blocked,
            BackgroundTaskStatus::Failed { .. } => Self::Failed,
            BackgroundTaskStatus::Running => Self::Active,
            BackgroundTaskStatus::Queued => Self::Queued,
            BackgroundTaskStatus::Succeeded { .. } => Self::Done,
            BackgroundTaskStatus::Cancelled => Self::Cancelled,
        }
    }

    /// Whether the row retires on the TTL rather than waiting for `x`.
    ///
    /// Only success ages out. A failure, a cancellation or a pending approval
    /// stays until the operator dismisses it — the same instinct as the
    /// ordering rule, and the reason a `done` row cannot be allowed to push
    /// one of them out of the strip.
    fn retires_on_ttl(self) -> bool {
        matches!(self, Self::Done)
    }

    fn marker(self) -> &'static str {
        match self {
            Self::Blocked => "[|]",
            Self::Failed => "[!]",
            Self::Active => "[>]",
            Self::Queued => "[ ]",
            Self::Done => "[✓]",
            Self::Cancelled => "[-]",
        }
    }
}

/// One drawn row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripRow {
    pub state: StripState,
    /// Sub-agent (`◆`) versus shell job (`⟳`) — the same glyph split the
    /// footer's counts-only chip uses, so the two surfaces agree.
    pub subagent: bool,
    pub label: String,
    /// Right-aligned; frozen at `finished_at` once the task is terminal.
    pub elapsed: String,
    /// Blocked rows only: the tool and its already-redacted argument summary.
    pub detail: Option<String>,
}

impl StripRow {
    pub fn marker(&self) -> &'static str {
        self.state.marker()
    }

    pub fn glyph(&self) -> &'static str {
        if self.subagent {
            "◆"
        } else {
            "⟳"
        }
    }
}

/// What the strip should draw this frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackgroundStrip {
    pub rows: Vec<StripRow>,
    /// Tasks that did not fit, drawn as `+N more` on the header.
    pub hidden: usize,
    /// Tasks that survived expiry, before the row cap.
    pub total: usize,
}

impl BackgroundStrip {
    /// Nothing survived expiry, so the strip should not be drawn at all and
    /// the sidebar's vertical budget goes to the transcript.
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Lines the strip needs: the header, plus one for each row, plus one more
    /// for every row that carries a second line.
    ///
    /// Rows are not all one line tall, and the caller cannot work that out from
    /// a count. Getting this wrong does not clip gracefully — a row's second
    /// line is drawn where the next row's first line goes, so the budget has to
    /// agree with the widget's own row advance.
    pub fn height(&self) -> u16 {
        let rows: u16 = self
            .rows
            .iter()
            .map(|row| 1 + u16::from(row.detail.is_some()))
            .sum();
        1 + rows
    }

    /// Build the strip from the selected session's background tasks.
    ///
    /// `visible_rows` is what the layout says actually fits after the
    /// transcript's floor — the model never decides that itself, so there is
    /// one place that owns the budget.
    pub fn build(
        tasks: &[BackgroundTaskSnapshot],
        now: DateTime<Utc>,
        visible_rows: usize,
    ) -> Self {
        let mut live: Vec<(StripState, BackgroundTaskId, StripRow)> = tasks
            .iter()
            .filter(|task| !expired(task, now))
            .map(|task| {
                let state = StripState::of(&task.status);
                (state, task.id, row_for(task, now, state))
            })
            .collect();

        // Stable by id inside a state band, so a running row never jumps
        // because a sibling finished.
        live.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1 .0.cmp(&b.1 .0)));

        let total = live.len();
        let shown = visible_rows.min(total);
        let rows: Vec<StripRow> = live.into_iter().take(shown).map(|(_, _, r)| r).collect();

        // Truncation takes from the tail, and the sort puts blocked and failed
        // first, so the rows that get cut are always the ones that are still
        // progressing or already finished. That guarantee lives in the ranking
        // and is pinned by `truncation_never_hides_an_attention_state` rather
        // than re-checked here — a second implementation of the same rule
        // would be one more thing to keep in sync.
        Self {
            hidden: total - rows.len(),
            total,
            rows,
        }
    }
}

/// A `done` row leaves `DONE_ROW_TTL_SECS` after it finished. Everything else
/// stays put; a task with no `finished_at` is still running and never expires.
fn expired(task: &BackgroundTaskSnapshot, now: DateTime<Utc>) -> bool {
    let state = StripState::of(&task.status);
    if !state.retires_on_ttl() {
        return false;
    }
    match task.finished_at {
        Some(finished_at) => (now - finished_at).num_seconds() >= DONE_ROW_TTL_SECS,
        // A terminal status with no timestamp should not be resurrected
        // forever; treat it as freshly finished rather than expired.
        None => false,
    }
}

fn row_for(task: &BackgroundTaskSnapshot, now: DateTime<Utc>, state: StripState) -> StripRow {
    StripRow {
        state,
        subagent: matches!(task.kind, BackgroundTaskKind::Subagent { .. }),
        label: task.label.clone(),
        elapsed: elapsed_for(task, now),
        detail: match &task.status {
            // The pending request is what the operator has to answer, so it
            // outranks the activity line — but the two states are mutually
            // exclusive, so this is ordering of precedence rather than of
            // competition.
            BackgroundTaskStatus::WaitingForApproval { payload } => Some(detail_for(payload)),
            _ if state == StripState::Active => activity_for(task),
            _ => None,
        },
    }
}

/// What an active subagent is doing, when it has told us.
///
/// Assistant text arrives as it streams, so it carries the newlines of prose;
/// a row's second line is one line, so runs of whitespace collapse.
fn activity_for(task: &BackgroundTaskSnapshot) -> Option<String> {
    let text = task.latest_message.as_deref()?;
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(truncate(&collapsed, ACTIVITY_CHARS))
}

/// Counts up while the task runs, then freezes: a finished row's age is its
/// duration, not how long ago it finished.
fn elapsed_for(task: &BackgroundTaskSnapshot, now: DateTime<Utc>) -> String {
    let end = task.finished_at.unwrap_or(now);
    let secs = (end - task.started_at).num_milliseconds() as f64 / 1000.0;
    format_elapsed_tenths(secs.max(0.0))
}

/// The pending request, from the redacted payload — never the raw arguments.
fn detail_for(payload: &HitlPayload) -> String {
    let args = payload
        .args_redacted
        .get("command")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| payload.args_redacted.to_string());
    format!("{} · {}", payload.tool, truncate(&args, DETAIL_CHARS))
}

/// Truncate on character boundaries with an ellipsis that costs one cell.
fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    let kept: String = text.chars().take(width - 1).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn blocked(tool: &str, command: &str) -> BackgroundTaskStatus {
        BackgroundTaskStatus::WaitingForApproval {
            payload: HitlPayload {
                call_id: "call_1".into(),
                tool: tool.into(),
                args_redacted: serde_json::json!({ "command": command }),
                reason: "sandbox denied".into(),
                failure: None,
                sandbox_escalation: false,
                denied_host: None,
            },
        }
    }

    fn task(
        id: u64,
        label: &str,
        status: BackgroundTaskStatus,
        started_at: DateTime<Utc>,
        finished_at: Option<DateTime<Utc>>,
    ) -> BackgroundTaskSnapshot {
        BackgroundTaskSnapshot {
            id: BackgroundTaskId(id),
            label: label.into(),
            kind: BackgroundTaskKind::Subagent {
                role: label.into(),
                prompt: "do the thing".into(),
            },
            status,
            child_session_id: None,
            latest_message: None,
            worktree_path: None,
            worktree_branch: None,
            started_at,
            finished_at,
        }
    }

    fn succeeded() -> BackgroundTaskStatus {
        BackgroundTaskStatus::Succeeded {
            summary: "ok".into(),
        }
    }

    /// The whole reason the ranking is a `#[repr]`-style enum: a failure must
    /// be visible without the operator hunting for it.
    #[test]
    fn ordering_puts_attention_first_and_is_stable_within_a_band() {
        let now = Utc::now();
        let started = now - Duration::seconds(10);
        let tasks = vec![
            task(5, "done", succeeded(), started, Some(now)),
            task(1, "running-a", BackgroundTaskStatus::Running, started, None),
            task(
                3,
                "failed",
                BackgroundTaskStatus::Failed {
                    error: "boom".into(),
                },
                started,
                Some(now),
            ),
            task(2, "running-b", BackgroundTaskStatus::Running, started, None),
            task(
                4,
                "blocked",
                blocked("bash", "rm -rf target/debug"),
                started,
                None,
            ),
            task(6, "queued", BackgroundTaskStatus::Queued, started, None),
        ];

        let strip = BackgroundStrip::build(&tasks, now, STRIP_ROW_CAP);
        let order: Vec<&str> = strip.rows.iter().map(|r| r.label.as_str()).collect();

        assert_eq!(
            order,
            vec![
                "blocked",
                "failed",
                "running-a",
                "running-b",
                "queued",
                "done"
            ],
            "blocked then failed then active, stable by id inside each band"
        );
    }

    /// Truncation takes from the tail, so the states that need an operator
    /// have to be at the front at every budget — not just at the cap.
    #[test]
    fn truncation_never_hides_an_attention_state() {
        let now = Utc::now();
        let started = now - Duration::seconds(10);
        let mut tasks: Vec<BackgroundTaskSnapshot> = (0..5)
            .map(|i| {
                task(
                    100 + i,
                    "running",
                    BackgroundTaskStatus::Running,
                    started,
                    None,
                )
            })
            .collect();
        tasks.push(task(
            1,
            "blocked",
            blocked("bash", "rm -rf target/debug"),
            started,
            None,
        ));
        tasks.push(task(
            2,
            "failed",
            BackgroundTaskStatus::Failed {
                error: "boom".into(),
            },
            started,
            Some(now),
        ));
        tasks.push(task(3, "done", succeeded(), started, Some(now)));

        for budget in 0..=tasks.len() {
            let strip = BackgroundStrip::build(&tasks, now, budget);
            let labels: Vec<&str> = strip.rows.iter().map(|r| r.label.as_str()).collect();
            let hidden = strip.hidden;

            if budget >= 1 {
                assert!(
                    labels.contains(&"blocked"),
                    "budget {budget} hid the blocked row: {labels:?}"
                );
            }
            if budget >= 2 {
                assert!(
                    labels.contains(&"failed"),
                    "budget {budget} hid the failed row: {labels:?}"
                );
            }
            assert_eq!(
                strip.rows.len() + hidden,
                tasks.len(),
                "every task is either drawn or counted as hidden"
            );
        }
    }

    /// Only success ages out, and it ages from when it finished.
    #[test]
    fn a_done_row_retires_after_its_ttl_and_nothing_else_does() {
        let now = Utc::now();
        let started = now - Duration::seconds(600);
        let recent = now - Duration::seconds(DONE_ROW_TTL_SECS - 1);
        let stale = now - Duration::seconds(DONE_ROW_TTL_SECS);

        let tasks = vec![
            task(1, "just-finished", succeeded(), started, Some(recent)),
            task(2, "long-finished", succeeded(), started, Some(stale)),
            // Terminal but not successful: these stay until dismissed.
            task(
                3,
                "failed-long-ago",
                BackgroundTaskStatus::Failed {
                    error: "boom".into(),
                },
                started,
                Some(stale),
            ),
            task(
                4,
                "cancelled-long-ago",
                BackgroundTaskStatus::Cancelled,
                started,
                Some(stale),
            ),
            // Still running: nothing to age from.
            task(5, "running", BackgroundTaskStatus::Running, started, None),
        ];

        let strip = BackgroundStrip::build(&tasks, now, STRIP_ROW_CAP);
        let labels: Vec<&str> = strip.rows.iter().map(|r| r.label.as_str()).collect();

        assert!(
            !labels.contains(&"long-finished"),
            "a stale done row must retire: {labels:?}"
        );
        assert!(
            labels.contains(&"just-finished"),
            "a fresh done row must stay: {labels:?}"
        );
        assert!(
            labels.contains(&"failed-long-ago"),
            "a failure must not age out: {labels:?}"
        );
        assert!(
            labels.contains(&"cancelled-long-ago"),
            "a cancellation must not age out: {labels:?}"
        );
        assert!(
            labels.contains(&"running"),
            "a running task must not age out: {labels:?}"
        );
    }

    /// A finished row shows how long it *took*, not how long ago it stopped,
    /// so the column does not creep upward while the operator reads it.
    #[test]
    fn a_finished_row_freezes_its_elapsed() {
        let started = Utc::now() - Duration::seconds(300);
        let finished = started + Duration::seconds(31);
        let tasks = vec![task(1, "job", succeeded(), started, Some(finished))];

        let soon = BackgroundStrip::build(&tasks, finished, STRIP_ROW_CAP);
        let later = BackgroundStrip::build(&tasks, finished + Duration::seconds(20), STRIP_ROW_CAP);

        assert_eq!(soon.rows[0].elapsed, later.rows[0].elapsed);
        assert_eq!(soon.rows[0].elapsed, "31s");
    }

    /// A subagent's activity is the one thing its label cannot tell you, so a
    /// running row shows it.
    #[test]
    fn an_active_subagent_shows_what_it_is_doing() {
        let now = Utc::now();
        let started = now - Duration::seconds(5);
        let mut running = task(1, "explore", BackgroundTaskStatus::Running, started, None);
        running.latest_message = Some("reading\nthe   manifest".into());

        let strip = BackgroundStrip::build(&[running], now, STRIP_ROW_CAP);
        assert_eq!(
            strip.rows[0].detail.as_deref(),
            Some("reading the manifest"),
            "streamed prose collapses onto the one line the row has"
        );
    }

    /// The request is what the operator has to answer, so it wins the second
    /// line over whatever the agent happened to say last.
    #[test]
    fn a_blocked_row_shows_the_request_not_the_activity() {
        let now = Utc::now();
        let started = now - Duration::seconds(5);
        let mut waiting = task(1, "explore", blocked("bash", "echo risky"), started, None);
        waiting.latest_message = Some("I am about to run a command".into());

        let strip = BackgroundStrip::build(&[waiting], now, STRIP_ROW_CAP);
        assert_eq!(strip.rows[0].detail.as_deref(), Some("bash · echo risky"));
    }

    /// Silence is the normal case for the first moments of a run, and a row
    /// must not reserve a line it has nothing to put on.
    #[test]
    fn an_active_row_with_nothing_to_report_has_no_second_line() {
        let now = Utc::now();
        let started = now - Duration::seconds(5);
        let strip = BackgroundStrip::build(
            &[task(
                1,
                "explore",
                BackgroundTaskStatus::Running,
                started,
                None,
            )],
            now,
            STRIP_ROW_CAP,
        );
        assert!(strip.rows[0].detail.is_none());
        assert_eq!(strip.height(), 2, "header plus one row");
    }

    /// The strip's height has to agree with the widget's row advance: a row
    /// with a second line is two lines tall, and a budget that assumes one
    /// draws the next row over that line.
    #[test]
    fn height_counts_second_lines() {
        let now = Utc::now();
        let started = now - Duration::seconds(5);
        let rows = vec![
            task(1, "explore", blocked("bash", "echo risky"), started, None),
            task(2, "verify", BackgroundTaskStatus::Running, started, None),
        ];
        let strip = BackgroundStrip::build(&rows, now, STRIP_ROW_CAP);
        assert_eq!(strip.rows.len(), 2);
        assert_eq!(strip.height(), 4, "header + two rows + one second line");
    }

    /// The detail line is what tells the operator *what* a subagent wants, and
    /// it reads the redacted payload — never the raw arguments.
    #[test]
    fn a_blocked_row_names_the_tool_and_its_redacted_command() {
        let now = Utc::now();
        let started = now - Duration::seconds(5);
        let tasks = vec![task(
            1,
            "explore",
            blocked("bash", "rm -rf target/debug"),
            started,
            None,
        )];

        let strip = BackgroundStrip::build(&tasks, now, STRIP_ROW_CAP);
        let row = &strip.rows[0];

        assert_eq!(row.marker(), "[|]");
        assert_eq!(row.glyph(), "◆");
        assert_eq!(row.detail.as_deref(), Some("bash · rm -rf target/debug"));
    }

    /// A long command is truncated to a cell budget rather than wrapped, so a
    /// row's height never depends on what it is running.
    #[test]
    fn a_long_blocked_command_is_truncated_not_wrapped() {
        let now = Utc::now();
        let started = now - Duration::seconds(5);
        let tasks = vec![task(
            1,
            "explore",
            blocked(
                "bash",
                "cargo test --workspace --all-features -- --nocapture",
            ),
            started,
            None,
        )];

        let strip = BackgroundStrip::build(&tasks, now, STRIP_ROW_CAP);
        let detail = strip.rows[0].detail.as_deref().unwrap();

        assert!(detail.ends_with('…'), "expected an ellipsis: {detail}");
        assert_eq!(
            detail.chars().count(),
            "bash · ".chars().count() + DETAIL_CHARS
        );
    }

    #[test]
    fn an_empty_registry_produces_an_empty_strip() {
        let strip = BackgroundStrip::build(&[], Utc::now(), STRIP_ROW_CAP);
        assert!(strip.is_empty());
        assert_eq!(strip.total, 0);
        assert_eq!(strip.hidden, 0);
    }
}
