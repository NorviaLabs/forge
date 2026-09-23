//! `/goal` — a completion condition the session keeps working toward.
//!
//! Setting a goal starts a turn with the condition as the directive, exactly as
//! if the operator had typed it. After every turn that settles with no queue
//! and no pending approval, a *separate* evaluator model call judges the
//! condition against the transcript; while it is unmet, Forge starts the next
//! turn itself. The goal clears when the evaluator confirms it, judges it
//! impossible, or the operator runs `/goal clear`.
//!
//! The evaluator is a fresh, tool-less model call — it can only read what the
//! assistant already surfaced, so a condition must be something the assistant's
//! own output can demonstrate ("`npm test` exits 0", not "the code is good").

use super::*;
use crate::status_report::StatusRow;

/// Ceiling on automatic continuations, so a condition that can never hold
/// cannot spin the session forever. Re-arming with `/goal` starts a fresh
/// budget.
const MAX_GOAL_TURNS: u32 = 50;

/// What the evaluator decided about the condition.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GoalVerdict {
    /// The condition holds; the goal is done.
    Met(String),
    /// The condition does not hold yet; keep working.
    NotMet(String),
    /// The evaluator judges the condition unsatisfiable.
    Impossible(String),
    /// The evaluator could not be reached or did not answer in the expected
    /// shape. The goal stays set but stops auto-continuing.
    Unknown(String),
}

impl TuiApp {
    /// `/goal <condition>` — record the condition and start working toward it.
    pub(super) async fn set_goal(&mut self, condition: String) -> Result<(), TuiError> {
        self.goal = Some(GoalState {
            condition: condition.clone(),
            turns_evaluated: 0,
            last_reason: None,
            started_at: Instant::now(),
        });
        self.push_toast("goal set");
        self.set_feedback(FeedbackSeverity::Info, format!("◎ goal set · {condition}"));
        self.push_activity(ActivityKind::System, FeedbackSeverity::Info, "goal set");
        // Setting a goal starts a turn immediately, with the condition itself
        // as the directive — the operator does not send a second prompt.
        // Boxed because `dispatch_line` is recursive through this path.
        Box::pin(self.dispatch_line(&condition)).await
    }

    /// `/goal` — report the active goal, or say that none is set.
    pub(super) fn show_goal(&mut self) {
        self.overlay = Some(Overlay::StatusReport {
            title: "Goal".into(),
            rows: self.goal_report_rows(),
        });
    }

    /// `/goal clear` (and its aliases) — drop the active goal.
    pub(super) fn clear_goal_command(&mut self) {
        match self.goal.take() {
            Some(goal) => {
                self.push_toast("goal cleared");
                self.set_feedback(
                    FeedbackSeverity::Info,
                    format!("Goal cleared: {}", goal.condition),
                );
                self.push_activity(ActivityKind::System, FeedbackSeverity::Info, "goal cleared");
            }
            None => self.set_feedback(FeedbackSeverity::Info, "No goal set"),
        }
    }

    /// Rows for the `/goal` status report.
    fn goal_report_rows(&self) -> Vec<StatusRow> {
        let Some(goal) = self.goal.as_ref() else {
            return vec![
                StatusRow::field("Goal", "No goal set"),
                StatusRow::Gap,
                StatusRow::Heading("Set one".into()),
                StatusRow::field("Set", "/goal <condition>"),
                StatusRow::field("Clear", "/goal clear"),
            ];
        };
        let mut rows = vec![
            StatusRow::field("Condition", goal.condition.clone()),
            StatusRow::field("Elapsed", format_goal_elapsed(goal.started_at.elapsed())),
            StatusRow::field("Turns evaluated", goal.turns_evaluated.to_string()),
        ];
        if let Some(reason) = goal.last_reason.as_ref() {
            rows.push(StatusRow::Gap);
            rows.push(StatusRow::field("Last verdict", reason.clone()));
        }
        rows
    }

    /// Judge the active goal after a turn settles, and start the next turn while
    /// it is unmet. A no-op when no goal is set, another turn is already queued,
    /// or the session is waiting on the operator (approval or a question).
    pub(super) async fn advance_goal_after_turn(&mut self) -> Result<(), TuiError> {
        if self.goal.is_none() || self.selected_is_supervised() {
            return Ok(());
        }
        // A queued turn or a pending prompt runs first; the goal is judged once
        // that turn settles and the session is idle again.
        if !self.session_runtime.queue().is_empty() || self.pending_turn.has_prompt() {
            return Ok(());
        }
        // Paused, not abandoned: an unanswered approval or question is the
        // operator's turn, so evaluating now would judge a half-finished step.
        if self.session_runtime.pending_hitl().is_some()
            || self.session_runtime.pending_question().is_some()
        {
            return Ok(());
        }
        let condition = self
            .goal
            .as_ref()
            .map(|goal| goal.condition.clone())
            .unwrap_or_default();

        match self.evaluate_goal(&condition).await {
            GoalVerdict::Met(reason) => {
                self.goal = None;
                self.push_toast("goal met");
                self.set_feedback(FeedbackSeverity::Ok, format!("◎ goal met · {reason}"));
                self.push_activity(ActivityKind::System, FeedbackSeverity::Ok, "goal met");
            }
            GoalVerdict::Impossible(reason) => {
                self.goal = None;
                self.push_toast("goal impossible");
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    format!("goal judged impossible · {reason}"),
                );
                self.push_activity(
                    ActivityKind::System,
                    FeedbackSeverity::Warn,
                    "goal impossible",
                );
            }
            GoalVerdict::NotMet(reason) => {
                let turns = match self.goal.as_mut() {
                    Some(goal) => {
                        goal.turns_evaluated += 1;
                        goal.last_reason = Some(reason.clone());
                        goal.turns_evaluated
                    }
                    None => return Ok(()),
                };
                if turns >= MAX_GOAL_TURNS {
                    self.goal = None;
                    self.push_toast("goal paused");
                    self.set_feedback(
                        FeedbackSeverity::Warn,
                        format!("goal paused after {turns} turns · /goal to re-arm"),
                    );
                    return Ok(());
                }
                self.set_feedback(
                    FeedbackSeverity::Info,
                    format!("◎ goal active · turn {turns} · {reason}"),
                );
                self.push_activity(
                    ActivityKind::System,
                    FeedbackSeverity::Info,
                    format!("goal turn {turns}"),
                );
                let prompt = goal_continuation_prompt(&condition, &reason);
                Box::pin(self.dispatch_line(&prompt)).await?;
            }
            GoalVerdict::Unknown(reason) => {
                // Keep the goal set but stop auto-continuing: a broken or
                // unreachable evaluator must not spin the session.
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    format!("goal paused · evaluator: {reason}"),
                );
                self.push_activity(
                    ActivityKind::System,
                    FeedbackSeverity::Warn,
                    "goal evaluator unavailable",
                );
            }
        }
        Ok(())
    }

    /// Ask a separate, tool-less model call whether the condition holds. The
    /// request reuses the live transcript, so the evaluator sees exactly what
    /// the working model surfaced — nothing more.
    async fn evaluate_goal(&mut self, condition: &str) -> GoalVerdict {
        if let Some(profile_id) = self.connect.profile.clone() {
            self.apply_connect_credentials(&profile_id);
        }
        let mut request = self.session_runtime.build_model_request();
        request.tools.clear();
        request.messages.push(Message::new(
            MessageRole::User,
            goal_evaluator_instruction(condition),
        ));
        match self.session_runtime.model_client().complete(request).await {
            Ok(response) => parse_goal_verdict(&response.text),
            Err(error) => GoalVerdict::Unknown(error.to_string()),
        }
    }
}

/// The directive that turns the transcript into an evaluation turn.
fn goal_evaluator_instruction(condition: &str) -> String {
    format!(
        "You are the goal evaluator. Decide whether the completion condition holds, \
         using only the conversation above. You cannot run commands or read files, so \
         judge only what the assistant has already surfaced.\n\n\
         Completion condition:\n{condition}\n\n\
         Reply with exactly one line, one of:\n\
         MET: <one-sentence reason>\n\
         NOT_MET: <one-sentence reason>\n\
         IMPOSSIBLE: <one-sentence reason>"
    )
}

/// The directive that starts the next turn while the goal is unmet.
fn goal_continuation_prompt(condition: &str, reason: &str) -> String {
    format!(
        "Continue working toward the goal. Do not restate it — take the next \
         concrete step.\n\nGoal: {condition}\nEvaluator: {reason}"
    )
}

/// Parse the evaluator's verdict from its reply. Unrecognised replies are
/// [`GoalVerdict::Unknown`], which pauses rather than continuing.
fn parse_goal_verdict(text: &str) -> GoalVerdict {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty());
    let Some(line) = line else {
        return GoalVerdict::Unknown("empty evaluator reply".into());
    };
    let reason = line
        .split_once(':')
        .map(|(_, reason)| reason.trim())
        .filter(|reason| !reason.is_empty())
        .unwrap_or(line)
        .to_string();
    let upper = line.to_ascii_uppercase();
    // Order matters: `NOT_MET` must be tested before `MET`.
    if upper.starts_with("IMPOSSIBLE") {
        GoalVerdict::Impossible(reason)
    } else if upper.starts_with("NOT_MET") || upper.starts_with("NOT MET") {
        GoalVerdict::NotMet(reason)
    } else if upper.starts_with("MET") {
        GoalVerdict::Met(reason)
    } else {
        GoalVerdict::Unknown(reason)
    }
}

/// Compact elapsed time for the `/goal` status line.
fn format_goal_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3_600, (secs % 3_600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_parsing_reads_the_leading_keyword() {
        assert_eq!(
            parse_goal_verdict("MET: tests pass and lint is clean"),
            GoalVerdict::Met("tests pass and lint is clean".into())
        );
        assert_eq!(
            parse_goal_verdict("not_met: two call sites still fail to compile"),
            GoalVerdict::NotMet("two call sites still fail to compile".into())
        );
        assert_eq!(
            parse_goal_verdict("NOT MET - the queue is not empty"),
            GoalVerdict::NotMet("NOT MET - the queue is not empty".into())
        );
        assert_eq!(
            parse_goal_verdict("IMPOSSIBLE: the API this targets was removed"),
            GoalVerdict::Impossible("the API this targets was removed".into())
        );
    }

    #[test]
    fn verdict_parsing_skips_leading_blank_lines_and_pauses_on_noise() {
        assert_eq!(
            parse_goal_verdict("\n\n  MET: done  \n"),
            GoalVerdict::Met("done".into())
        );
        // A reply that names no verdict pauses rather than continuing.
        assert!(matches!(
            parse_goal_verdict("Sure, here is my analysis of the work so far."),
            GoalVerdict::Unknown(_)
        ));
        assert!(matches!(parse_goal_verdict("   "), GoalVerdict::Unknown(_)));
    }

    #[test]
    fn elapsed_formatting_is_readable_at_each_scale() {
        assert_eq!(format_goal_elapsed(Duration::from_secs(5)), "5s");
        assert_eq!(format_goal_elapsed(Duration::from_secs(90)), "1m 30s");
        assert_eq!(format_goal_elapsed(Duration::from_secs(3_900)), "1h 5m");
    }
}
