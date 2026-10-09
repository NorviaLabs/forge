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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TaskFilter {
    #[default]
    Jobs,
    Agents,
    Queue,
}

impl TaskFilter {
    pub fn label(self) -> &'static str {
        match self {
            Self::Jobs => "Jobs",
            Self::Agents => "Agents",
            Self::Queue => "Queue",
        }
    }

    pub fn next(self, backwards: bool) -> Self {
        match (self, backwards) {
            (Self::Jobs, false) | (Self::Queue, true) => Self::Agents,
            (Self::Agents, false) | (Self::Jobs, true) => Self::Queue,
            _ => Self::Jobs,
        }
    }
}

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
    /// Which rank a task's status sorts under.
    pub fn of(status: &BackgroundTaskStatus) -> Self {
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
    pub id: BackgroundTaskId,
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

/// The single filter-and-sort both the drawn strip and the `↑↓ i` selection
/// share: drop TTL-retired `done` rows, rank the rest `blocked → failed →
/// active → queued → done → cancelled`, stable by task id inside a band.
pub fn ordered_live(
    tasks: &[BackgroundTaskSnapshot],
    now: DateTime<Utc>,
) -> Vec<(StripState, BackgroundTaskId, &BackgroundTaskSnapshot)> {
    ordered_tasks(tasks)
        .into_iter()
        .filter(|(_, _, task)| !expired(task, now))
        .collect()
}

/// Inspection retains terminal evidence after a success leaves the dock.
pub fn ordered_tasks(
    tasks: &[BackgroundTaskSnapshot],
) -> Vec<(StripState, BackgroundTaskId, &BackgroundTaskSnapshot)> {
    let mut live: Vec<(StripState, BackgroundTaskId, &BackgroundTaskSnapshot)> = tasks
        .iter()
        .map(|task| {
            let state = StripState::of(&task.status);
            (state, task.id, task)
        })
        .collect();

    // Stable by id inside a state band, so a running row never jumps
    // because a sibling finished.
    live.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1 .0.cmp(&b.1 .0)));
    live
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

pub(crate) fn row_for(
    task: &BackgroundTaskSnapshot,
    now: DateTime<Utc>,
    state: StripState,
) -> StripRow {
    StripRow {
        id: task.id,
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
    if matches!(task.status, BackgroundTaskStatus::WaitingForApproval { .. }) {
        return "waiting".into();
    }
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
            shell: None,
            run_id: uuid::Uuid::new_v4(),
            child: None,
            pending_approval: None,
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

    /// The rows the task view and turn line build: the shared filter-and-sort,
    /// then map to the drawn row shape.
    fn visible_rows(tasks: &[BackgroundTaskSnapshot], now: DateTime<Utc>) -> Vec<StripRow> {
        ordered_live(tasks, now)
            .into_iter()
            .map(|(state, _, task)| row_for(task, now, state))
            .collect()
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

        let rows = visible_rows(&tasks, now);
        let order: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();

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

        let rows = visible_rows(&tasks, now);
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();

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

        let soon = visible_rows(&tasks, finished);
        let later = visible_rows(&tasks, finished + Duration::seconds(20));

        assert_eq!(soon[0].elapsed, later[0].elapsed);
        assert_eq!(soon[0].elapsed, "31s");
    }

    /// A subagent's activity is the one thing its label cannot tell you, so a
    /// running row shows it.
    #[test]
    fn an_active_subagent_shows_what_it_is_doing() {
        let now = Utc::now();
        let started = now - Duration::seconds(5);
        let mut running = task(1, "explore", BackgroundTaskStatus::Running, started, None);
        running.latest_message = Some("reading\nthe   manifest".into());

        let rows = visible_rows(&[running], now);
        assert_eq!(
            rows[0].detail.as_deref(),
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

        let rows = visible_rows(&[waiting], now);
        assert_eq!(rows[0].detail.as_deref(), Some("bash · echo risky"));
    }

    /// Silence is the normal case for the first moments of a run, and a row
    /// must not carry a second line it has nothing to put on.
    #[test]
    fn an_active_row_with_nothing_to_report_has_no_second_line() {
        let now = Utc::now();
        let started = now - Duration::seconds(5);
        let rows = visible_rows(
            &[task(
                1,
                "explore",
                BackgroundTaskStatus::Running,
                started,
                None,
            )],
            now,
        );
        assert!(rows[0].detail.is_none());
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

        let rows = visible_rows(&tasks, now);
        let row = &rows[0];

        assert_eq!(row.marker(), "[|]");
        assert_eq!(row.glyph(), "◆");
        assert_eq!(row.detail.as_deref(), Some("bash · rm -rf target/debug"));
    }
}
