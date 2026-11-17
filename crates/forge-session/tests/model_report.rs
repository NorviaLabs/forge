//! The model report: turn records grouped by task kind and
//! (model, provider, effort), scored, and ranked per kind.
//!
//! The fixtures are small enough to work out by hand; every expected number
//! below is derived in the comment next to it.
//!
//! Scoring, as agreed:
//! - restart-interrupted turns are left out entirely;
//! - seven metrics, ranked per kind among the groups with at least
//!   `min_turns` turns (best = 1, ties share the mean of their positions, a
//!   group without data for a metric gets the neutral rank (n + 1) / 2):
//!   completion rate ↑, escalation rate ↓, verification pass rate ↑,
//!   approval rate ↑, queued-message rate ↓, median tokens ↓, median active
//!   time ↓;
//! - score = mean of the seven ranks, lower is better; ties go to more
//!   turns, then model, provider, effort by name.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use forge_durable::{new_session_id, Journal, TurnRecord};
use forge_session::model_report::{
    build_report, model_report_in, model_report_with_trust_store, parse_since,
    ranked_models_with_trust_store, KindReport, RankStatus, RankedModel, ReportOptions,
    UntrustedWorkspace, DEFAULT_MIN_TURNS,
};
use forge_types::TaskKind;
use serde_json::json;

// ---------------------------------------------------------------- fixtures

const EPS: f64 = 1e-9;

fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < EPS
}

#[track_caller]
fn assert_close(actual: f64, expected: f64) {
    assert!(close(actual, expected), "{actual} != {expected}");
}

#[track_caller]
fn assert_some_close(actual: Option<f64>, expected: f64) {
    let actual = actual.expect("a value");
    assert_close(actual, expected);
}

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2025, 6, 10, 12, 0, 0).unwrap()
}

#[derive(Clone, Copy)]
struct Group {
    model: &'static str,
    provider: &'static str,
    effort: Option<&'static str>,
}

const A: Group = Group {
    model: "anthropic/claude-opus-4-1",
    provider: "anthropic",
    effort: Some("high"),
};
const B: Group = Group {
    model: "openai_codex/gpt-5.6-luna",
    provider: "openai_codex",
    effort: Some("medium"),
};
const C: Group = Group {
    model: "opencode_go/kimi-k2",
    provider: "opencode_go",
    effort: None,
};
const D: Group = Group {
    model: "xai/grok-4",
    provider: "xai",
    effort: Some("high"),
};
const E: Group = Group {
    model: "anthropic/claude-sonnet-4",
    provider: "anthropic",
    effort: Some("low"),
};
const F: Group = Group {
    model: "openai_codex/gpt-5.6-luna",
    provider: "openai_codex",
    effort: Some("low"),
};

/// One turn: completed, `tokens` prompt + completion tokens, `active_ms` of
/// wall time with no waiting, no signals.
#[derive(Clone)]
struct Turn(TurnRecord);

impl Turn {
    fn new(kind: &TaskKind, group: Group, tokens: u64, active_ms: u64) -> Self {
        Self(TurnRecord {
            session_id: new_session_id(),
            parent_session_id: None,
            task_id: 1,
            started_at: Some(t0()),
            ended_at: t0(),
            model: group.model.into(),
            profile_id: group.provider.into(),
            reasoning_effort: group.effort.map(str::to_string),
            tier: None,
            task_kind: kind.clone(),
            outcome: "completed".into(),
            completion_reason: "edit_verified".into(),
            escalated: false,
            escalation_retry: false,
            prompt_tokens: Some(tokens - 100),
            completion_tokens: Some(100),
            cache_read_tokens: Some(50_000),
            cache_write_tokens: Some(0),
            model_calls: Some(1),
            duration_ms: Some(active_ms),
            waiting_ms: Some(0),
            signals: json!([]),
        })
    }

    fn failed(mut self) -> Self {
        self.0.outcome = "failed".into();
        self.0.completion_reason = "dangling_tool_call_text".into();
        self
    }

    fn escalated(mut self) -> Self {
        self.0.escalated = true;
        self
    }

    fn cancelled_by_user(mut self) -> Self {
        self.0.outcome = "cancelled".into();
        self.0.completion_reason = "user_cancelled".into();
        self.0.signals = json!([{"kind": "user_cancelled"}]);
        self
    }

    fn interrupted_by_restart(mut self) -> Self {
        self.0.outcome = "interrupted".into();
        self.0.completion_reason = "interrupted_by_restart".into();
        self
    }

    fn signals(mut self, kinds: &[&str]) -> Self {
        self.0.signals = kinds
            .iter()
            .map(|kind| json!({"kind": kind, "at_ms": 10}))
            .collect();
        self
    }

    /// The turn also waited `ms` on the user: wall time grows, active time
    /// does not.
    fn waited(mut self, ms: u64) -> Self {
        self.0.duration_ms = self.0.duration_ms.map(|d| d + ms);
        self.0.waiting_ms = Some(ms);
        self
    }

    fn stored_as(mut self, provider: &str, model: &str) -> Self {
        self.0.profile_id = provider.into();
        self.0.model = model.into();
        self
    }

    fn ended_at(mut self, at: DateTime<Utc>) -> Self {
        self.0.ended_at = at;
        self
    }
}

/// Main kind, four groups.
///
/// A — 10 turns (+1 restart-interrupted, left out):
///   completed 9/10 = 0.9 (turn 0 failed), escalated 1/10 = 0.1 (turn 0);
///   verification: turns 1–4 passed, turn 5 passed then failed → failed;
///     4 passed, 1 failed, rate 4/5 = 0.8;
///   approvals (signals, not turns): 2 + 1 granted, 1 denied → 3/4 = 0.75;
///   queued: turn 9 (two messages, one turn) → 1/10 = 0.1;
///   tokens 8000 … 30000 → median (11500 + 12500) / 2 = 12000;
///   active 20000 … 60000 (turn 5 waited 8000 more) → (38000 + 42000) / 2
///   = 40000.
/// B — 8 turns: completed 8/8 = 1.0, escalated 0;
///   verification: only `verification_unfinished` → no data;
///   approvals 2 granted, 0 denied → 1.0; queued turns 3, 4 → 2/8 = 0.25;
///   tokens → (19500 + 20500) / 2 = 20000; active → (59000 + 61000) / 2 =
///   60000.
/// C — 6 turns (two stored under the old `opencode-go` names):
///   completed 5/6 (turn 5 cancelled by the user), escalated 0;
///   verification 2 passed, 0 failed → 1.0; approvals: no data; queued 0;
///   tokens → (14500 + 15500) / 2 = 15000; active → (49000 + 51000) / 2 =
///   50000.
/// D — 3 turns: below 5, not enough data.
///
/// Ranks among A, B, C (n = 3, neutral = 2):
///   completion   B 1,   A 2,   C 3
///   escalation   B 1.5, C 1.5, A 3
///   verification C 1,   A 2,   B 2 (neutral)
///   approval     B 1,   A 2,   C 2 (neutral)
///   queued       C 1,   A 2,   B 3
///   tokens       A 1,   C 2,   B 3
///   active       A 1,   C 2,   B 3
/// Scores: A 13/7 ≈ 1.857, B 14.5/7 ≈ 2.071, C 12.5/7 ≈ 1.786.
/// Order: C, A, B, then D (unranked). Kind turns: 10 + 8 + 6 + 3 = 27.
fn main_turns() -> Vec<Turn> {
    let main = TaskKind::Main;
    let mut turns = Vec::new();

    let a_tokens = [
        8000, 9000, 10000, 11000, 11500, 12500, 13000, 14000, 15000, 30000,
    ];
    let a_active = [
        20000, 25000, 30000, 35000, 38000, 42000, 45000, 50000, 55000, 60000,
    ];
    for i in 0..10 {
        let turn = Turn::new(&main, A, a_tokens[i], a_active[i]);
        turns.push(match i {
            0 => turn.failed().escalated(),
            1..=4 => turn.signals(&["verification_passed"]),
            5 => turn
                .signals(&["verification_passed", "verification_failed"])
                .waited(8000),
            6 => turn.signals(&[
                "approval_requested",
                "approval_granted",
                "approval_requested",
                "approval_granted",
            ]),
            7 => turn.signals(&["approval_requested", "approval_granted"]),
            8 => turn.signals(&["approval_requested", "approval_denied"]),
            _ => turn.signals(&["user_message_queued", "user_message_queued"]),
        });
    }
    // Left out: Forge restarted under it. Its numbers would change every
    // one of A's metrics if it counted.
    turns.push(
        Turn::new(&main, A, 1_000_000, 1_000_000)
            .interrupted_by_restart()
            .escalated()
            .signals(&["verification_failed", "user_message_queued"]),
    );

    let b_tokens = [16000, 18000, 19000, 19500, 20500, 21000, 22000, 24000];
    let b_active = [50000, 55000, 58000, 59000, 61000, 62000, 65000, 70000];
    for i in 0..8 {
        let turn = Turn::new(&main, B, b_tokens[i], b_active[i]);
        turns.push(match i {
            0 => turn.signals(&["verification_unfinished"]),
            1 | 2 => turn.signals(&["approval_requested", "approval_granted"]),
            3 | 4 => turn.signals(&["user_message_queued"]),
            _ => turn,
        });
    }

    let c_tokens = [10000, 14000, 14500, 15500, 16000, 40000];
    let c_active = [40000, 45000, 49000, 51000, 60000, 70000];
    for i in 0..6 {
        let turn = Turn::new(&main, C, c_tokens[i], c_active[i]);
        turns.push(match i {
            0 | 1 => turn.signals(&["verification_passed"]),
            2 | 3 => turn.stored_as("opencode-go", "opencode-go/kimi-k2"),
            5 => turn.cancelled_by_user(),
            _ => turn,
        });
    }

    for _ in 0..3 {
        turns.push(Turn::new(&main, D, 5000, 10000));
    }
    turns
}

/// Subagent kind: the same two models as A and B, the other way round.
///
/// A′ — 5 turns, turns 0 and 1 failed and escalated: completion 3/5 = 0.6,
///   escalation 2/5 = 0.4; tokens 7000 each; active 10000 each.
/// B′ — 5 turns, all completed: completion 1.0, escalation 0; tokens 6000;
///   active 12000.
/// No verification, approval or queue data: verification and approval are
/// neutral (1.5) for both; queued is 0 for both, a tie → 1.5 each.
///   completion B′ 1, A′ 2; escalation B′ 1, A′ 2; tokens B′ 1, A′ 2;
///   active A′ 1, B′ 2.
/// Scores: A′ (2+2+1.5+1.5+1.5+2+1)/7 = 11.5/7 ≈ 1.643,
///         B′ (1+1+1.5+1.5+1.5+1+2)/7 = 9.5/7 ≈ 1.357.
/// Order: B′, A′. Kind turns: 10.
fn subagent_turns() -> Vec<Turn> {
    let kind = TaskKind::Subagent;
    let mut turns = Vec::new();
    for i in 0..5 {
        let turn = Turn::new(&kind, A, 7000, 10000);
        turns.push(if i < 2 {
            turn.failed().escalated()
        } else {
            turn
        });
    }
    for _ in 0..5 {
        turns.push(Turn::new(&kind, B, 6000, 12000));
    }
    turns
}

/// `custom:reviewer`: E has exactly 5 turns — 3 stored as `anthropic`, 2
/// under the old `anthropic-api` route id — so it is ranked only if the two
/// names are one provider. F has 4: one short of the minimum.
/// E alone is ranked: every rank is 1 (the neutral rank for n = 1 is 1 too),
/// so its score is 1.0. Kind turns: 9.
fn custom_turns() -> Vec<Turn> {
    let kind = TaskKind::Custom("reviewer".into());
    let mut turns = Vec::new();
    for i in 0..5 {
        let turn = Turn::new(&kind, E, 3000, 8000);
        turns.push(if i < 2 {
            turn.stored_as("anthropic-api", "anthropic/claude-sonnet-4")
        } else {
            turn
        });
    }
    for _ in 0..4 {
        turns.push(Turn::new(&kind, F, 3000, 8000));
    }
    turns
}

fn all_turns() -> Vec<TurnRecord> {
    // Interleaved on purpose: input order must not matter.
    let mut turns: Vec<TurnRecord> = custom_turns()
        .into_iter()
        .chain(main_turns())
        .chain(subagent_turns())
        .map(|turn| turn.0)
        .collect();
    turns.reverse();
    turns
}

fn options() -> ReportOptions {
    ReportOptions::default()
}

fn kind<'a>(report: &'a [KindReport], kind: &TaskKind) -> &'a KindReport {
    report
        .iter()
        .find(|k| &k.kind == kind)
        .unwrap_or_else(|| panic!("no {kind} in the report"))
}

fn group(kind: &KindReport, g: Group) -> &RankedModel {
    kind.models
        .iter()
        .find(|m| m.model == g.model && m.provider == g.provider && m.effort.as_deref() == g.effort)
        .unwrap_or_else(|| panic!("no {} in {}", g.model, kind.kind))
}

fn order(kind: &KindReport) -> Vec<(String, Option<String>)> {
    kind.models
        .iter()
        .map(|m| (m.model.clone(), m.effort.clone()))
        .collect()
}

fn key(g: Group) -> (String, Option<String>) {
    (g.model.to_string(), g.effort.map(str::to_string))
}

// ------------------------------------------------------------ pure report

#[test]
fn the_default_minimum_is_five_turns() {
    assert_eq!(DEFAULT_MIN_TURNS, 5);
    let options = ReportOptions::default();
    assert_eq!(options.min_turns, 5);
    assert_eq!(options.kind, None);
    assert_eq!(options.since, None);
}

#[test]
fn kinds_come_main_then_subagent_then_custom_agents_with_their_turn_counts() {
    let report = build_report(all_turns(), &options());
    let kinds: Vec<(String, usize)> = report
        .iter()
        .map(|k| (k.kind.to_string(), k.turns))
        .collect();
    assert_eq!(
        kinds,
        [
            ("main".to_string(), 27),
            ("subagent".to_string(), 10),
            ("custom:reviewer".to_string(), 9),
        ]
    );
}

#[test]
fn main_turns_rank_c_then_a_then_b_with_d_unranked() {
    let report = build_report(all_turns(), &options());
    let main = kind(&report, &TaskKind::Main);
    assert_eq!(order(main), [key(C), key(A), key(B), key(D)]);

    let ranks: Vec<Option<usize>> = main.models.iter().map(|m| m.rank).collect();
    assert_eq!(ranks, [Some(1), Some(2), Some(3), None]);
    let statuses: Vec<RankStatus> = main.models.iter().map(|m| m.status).collect();
    assert_eq!(
        statuses,
        [
            RankStatus::Ranked,
            RankStatus::Ranked,
            RankStatus::Ranked,
            RankStatus::InsufficientData
        ]
    );
    assert_some_close(group(main, C).score, 12.5 / 7.0);
    assert_some_close(group(main, A).score, 13.0 / 7.0);
    assert_some_close(group(main, B).score, 14.5 / 7.0);
    assert_eq!(group(main, D).score, None);
}

#[test]
fn group_a_counts_rates_and_medians() {
    let report = build_report(all_turns(), &options());
    let a = group(kind(&report, &TaskKind::Main), A);
    // The restart-interrupted turn is not among them.
    assert_eq!(a.turns, 10);
    assert_eq!(a.completed, 9);
    assert_close(a.completion_rate, 0.9);
    assert_eq!(a.escalated, 1);
    assert_close(a.escalation_rate, 0.1);
    assert_eq!(a.verification_passed, 4);
    assert_eq!(a.verification_failed, 1);
    assert_some_close(a.verification_pass_rate, 0.8);
    assert_eq!(a.approvals_granted, 3);
    assert_eq!(a.approvals_denied, 1);
    assert_some_close(a.approval_rate, 0.75);
    assert_eq!(a.queued_turns, 1);
    assert_close(a.queued_rate, 0.1);
    assert_some_close(a.median_tokens, 12000.0);
    assert_some_close(a.median_active_ms, 40000.0);
}

#[test]
fn every_metric_rank_is_the_hand_worked_one() {
    let report = build_report(all_turns(), &options());
    let main = kind(&report, &TaskKind::Main);
    // (completion, escalation, verification, approval, queued, tokens, active)
    let expected = [
        (A, [2.0, 3.0, 2.0, 2.0, 2.0, 1.0, 1.0]),
        (B, [1.0, 1.5, 2.0, 1.0, 3.0, 3.0, 3.0]),
        (C, [3.0, 1.5, 1.0, 2.0, 1.0, 2.0, 2.0]),
    ];
    for (g, ranks) in expected {
        let r = group(main, g).ranks.clone().expect("ranked");
        let actual = [
            r.completion,
            r.escalation,
            r.verification,
            r.approval,
            r.queued,
            r.tokens,
            r.active_time,
        ];
        for (a, e) in actual.iter().zip(ranks) {
            assert_close(*a, e);
        }
    }
    assert!(group(main, D).ranks.is_none());
}

#[test]
fn group_b_has_no_verification_data_and_c_no_approval_data() {
    let report = build_report(all_turns(), &options());
    let main = kind(&report, &TaskKind::Main);
    let b = group(main, B);
    assert_eq!(b.turns, 8);
    assert_eq!((b.verification_passed, b.verification_failed), (0, 0));
    assert_eq!(b.verification_pass_rate, None, "unfinished is not a result");
    assert_eq!((b.approvals_granted, b.approvals_denied), (2, 0));
    assert_some_close(b.approval_rate, 1.0);
    assert_eq!(b.queued_turns, 2);
    assert_close(b.queued_rate, 0.25);
    assert_some_close(b.median_tokens, 20000.0);
    assert_some_close(b.median_active_ms, 60000.0);

    let c = group(main, C);
    assert_eq!(c.turns, 6, "old `opencode-go` names are the same group");
    assert_eq!(c.completed, 5, "a turn the user cancelled is not completed");
    assert_eq!(c.effort, None);
    assert_eq!(c.approval_rate, None);
    assert_some_close(c.verification_pass_rate, 1.0);
    assert_some_close(c.median_tokens, 15000.0);
    assert_some_close(c.median_active_ms, 50000.0);
}

#[test]
fn a_group_below_the_minimum_is_listed_with_its_numbers_but_not_ranked() {
    let report = build_report(all_turns(), &options());
    let d = group(kind(&report, &TaskKind::Main), D);
    assert_eq!(d.turns, 3);
    assert_eq!(d.status, RankStatus::InsufficientData);
    assert_eq!(d.rank, None);
    assert_eq!(d.completed, 3);
    assert_close(d.completion_rate, 1.0);
    assert_some_close(d.median_tokens, 5000.0);
}

#[test]
fn subagent_turns_rank_b_then_a() {
    let report = build_report(all_turns(), &options());
    let sub = kind(&report, &TaskKind::Subagent);
    assert_eq!(order(sub), [key(B), key(A)]);
    assert_some_close(group(sub, B).score, 9.5 / 7.0);
    assert_some_close(group(sub, A).score, 11.5 / 7.0);
    let a = group(sub, A);
    assert_eq!((a.turns, a.completed, a.escalated), (5, 3, 2));
    assert_close(a.escalation_rate, 0.4);
}

#[test]
fn the_minimum_is_inclusive_and_old_provider_names_merge() {
    let report = build_report(all_turns(), &options());
    let custom = kind(&report, &TaskKind::Custom("reviewer".into()));
    assert_eq!(custom.models.len(), 2, "{:#?}", custom.models);
    let e = group(custom, E);
    assert_eq!(e.turns, 5, "`anthropic-api` is `anthropic`");
    assert_eq!(e.rank, Some(1));
    assert_some_close(e.score, 1.0);
    let f = group(custom, F);
    assert_eq!(f.turns, 4);
    assert_eq!(f.status, RankStatus::InsufficientData);
    assert_eq!(order(custom), [key(E), key(F)]);
}

#[test]
fn a_lower_minimum_ranks_smaller_groups() {
    let options = ReportOptions {
        min_turns: 3,
        ..ReportOptions::default()
    };
    let report = build_report(all_turns(), &options);
    let main = kind(&report, &TaskKind::Main);
    assert!(main.models.iter().all(|m| m.rank.is_some()));
    let custom = kind(&report, &TaskKind::Custom("reviewer".into()));
    assert!(custom.models.iter().all(|m| m.status == RankStatus::Ranked));
}

/// Three groups with identical turns: every metric ties (rank 2 each, score
/// 2.0). More turns wins; equal turns go by model name.
#[test]
fn ties_go_to_more_turns_then_to_the_name() {
    let kind_main = TaskKind::Main;
    let g = |model: &'static str| Group {
        model,
        provider: "test",
        effort: Some("high"),
    };
    let mut turns = Vec::new();
    for _ in 0..5 {
        turns.push(Turn::new(&kind_main, g("test/b"), 1000, 1000).0);
        turns.push(Turn::new(&kind_main, g("test/a"), 1000, 1000).0);
    }
    for _ in 0..6 {
        turns.push(Turn::new(&kind_main, g("test/c"), 1000, 1000).0);
    }
    let report = build_report(turns, &options());
    let main = kind(&report, &kind_main);
    let models: Vec<&str> = main.models.iter().map(|m| m.model.as_str()).collect();
    assert_eq!(models, ["test/c", "test/a", "test/b"]);
    for m in &main.models {
        assert_some_close(m.score, 2.0);
    }
    let ranks: Vec<Option<usize>> = main.models.iter().map(|m| m.rank).collect();
    assert_eq!(ranks, [Some(1), Some(2), Some(3)]);
}

#[test]
fn since_keeps_turns_that_ended_at_or_after_it() {
    let main = TaskKind::Main;
    let since = t0();
    let turns = vec![
        Turn::new(&main, A, 1000, 1000)
            .ended_at(since - chrono::Duration::seconds(1))
            .0,
        Turn::new(&main, A, 1000, 1000).ended_at(since).0,
        Turn::new(&main, A, 1000, 1000)
            .ended_at(since + chrono::Duration::days(1))
            .0,
    ];
    let options = ReportOptions {
        since: Some(since),
        min_turns: 1,
        ..ReportOptions::default()
    };
    let report = build_report(turns, &options);
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].turns, 2);
}

#[test]
fn a_kind_filter_reports_that_kind_only() {
    let options = ReportOptions {
        kind: Some(TaskKind::Subagent),
        ..ReportOptions::default()
    };
    let report = build_report(all_turns(), &options);
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].kind, TaskKind::Subagent);
    let none = build_report(
        all_turns(),
        &ReportOptions {
            kind: Some(TaskKind::Custom("nobody".into())),
            ..ReportOptions::default()
        },
    );
    assert!(none.is_empty());
}

/// Medians over the turns whose value is known: the middle one for an odd
/// count, the mean of the middle two for an even count. Waiting is taken
/// off the wall time; a turn with no known waiting waited none.
#[test]
fn medians_skip_unknown_values() {
    let main = TaskKind::Main;
    let mut turns = vec![
        Turn::new(&main, A, 1000, 1000),
        Turn::new(&main, A, 3000, 9000),
        Turn::new(&main, A, 2000, 4000).waited(1000),
    ];
    // Unknown tokens and duration: counted as a turn, not in the medians.
    let mut unknown = Turn::new(&main, A, 9_999_999, 1);
    unknown.0.prompt_tokens = None;
    unknown.0.duration_ms = None;
    unknown.0.waiting_ms = None;
    turns.push(unknown);
    // Known duration, unknown waiting: all of it active.
    let mut no_waiting = Turn::new(&main, A, 2000, 5000);
    no_waiting.0.completion_tokens = None;
    no_waiting.0.waiting_ms = None;
    turns.push(no_waiting);

    let options = ReportOptions {
        min_turns: 1,
        ..ReportOptions::default()
    };
    let report = build_report(turns.iter().map(|t| t.0.clone()), &options);
    let a = group(&report[0], A);
    assert_eq!(a.turns, 5);
    // Tokens known for 1000, 3000, 2000 → median 2000.
    assert_some_close(a.median_tokens, 2000.0);
    // Active: 1000, 9000, 4000, 5000 → (4000 + 5000) / 2 = 4500.
    assert_some_close(a.median_active_ms, 4500.0);

    let mut blank = Turn::new(&main, A, 1000, 1000);
    blank.0.prompt_tokens = None;
    blank.0.duration_ms = None;
    let report = build_report([blank.0], &options);
    let a = group(&report[0], A);
    assert_eq!(a.median_tokens, None);
    assert_eq!(a.median_active_ms, None);
}

#[test]
fn a_group_of_only_restart_interrupted_turns_is_not_reported() {
    let main = TaskKind::Main;
    let turns = (0..6).map(|_| Turn::new(&main, A, 1000, 1000).interrupted_by_restart().0);
    assert!(build_report(turns, &options()).is_empty());
}

#[test]
fn the_report_serializes_with_kind_text_and_rank_status() {
    let report = build_report(all_turns(), &options());
    let main = serde_json::to_value(kind(&report, &TaskKind::Main)).unwrap();
    assert_eq!(main["kind"], "main");
    assert_eq!(main["turns"], 27);
    let first = &main["models"][0];
    assert_eq!(first["rank"], 1);
    assert_eq!(first["status"], "ranked");
    assert_eq!(first["model"], C.model);
    assert_eq!(first["provider"], "opencode_go");
    assert_eq!(first["effort"], serde_json::Value::Null);
    // The seven per-metric ranks behind the score, by name.
    let ranks = first["ranks"].as_object().expect("ranks of a ranked model");
    let mut names: Vec<&str> = ranks.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "active_time",
            "approval",
            "completion",
            "escalation",
            "queued",
            "tokens",
            "verification"
        ]
    );
    // C: completion 3, escalation 1.5, verification 1, approval 2 (neutral),
    // queued 1, tokens 2, active 2.
    assert_eq!(
        first["ranks"],
        json!({
            "completion": 3.0,
            "escalation": 1.5,
            "verification": 1.0,
            "approval": 2.0,
            "queued": 1.0,
            "tokens": 2.0,
            "active_time": 2.0
        })
    );
    let last = &main["models"][3];
    assert_eq!(last["rank"], serde_json::Value::Null);
    assert_eq!(last["status"], "insufficient_data");
    assert_eq!(last["turns"], 3);
    assert_eq!(last["score"], serde_json::Value::Null);
    assert_eq!(last["ranks"], serde_json::Value::Null);
    let custom = serde_json::to_value(kind(&report, &TaskKind::Custom("reviewer".into()))).unwrap();
    assert_eq!(custom["kind"], "custom:reviewer");
}

// ----------------------------------------------------------------- --since

#[test]
fn since_accepts_a_date_or_a_number_of_days_or_hours() {
    let now = Utc.with_ymd_and_hms(2025, 6, 10, 15, 30, 0).unwrap();
    assert_eq!(
        parse_since("2025-06-01", now).unwrap(),
        Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap()
    );
    assert_eq!(
        parse_since("7d", now).unwrap(),
        now - chrono::Duration::days(7)
    );
    assert_eq!(
        parse_since("24h", now).unwrap(),
        now - chrono::Duration::hours(24)
    );
    assert_eq!(parse_since("0d", now).unwrap(), now);
    for bad in [
        "",
        "7",
        "d",
        "-3d",
        "7w",
        "3.5d",
        "2025-13-01",
        "2025-06-1x",
        "yesterday",
        " 7d",
    ] {
        assert!(parse_since(bad, now).is_err(), "`{bad}` must be refused");
    }
}

// ------------------------------------------------------------ the journals

/// Wait until SQLite has closed every journal written here (their `-wal`
/// and `-shm` files gone), so the directory only changes if the report
/// changes it.
async fn settle(dir: &Path) {
    for _ in 0..500 {
        let busy = std::fs::read_dir(dir).unwrap().any(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            name.ends_with("-wal") || name.ends_with("-shm")
        });
        if !busy {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("journals in {} never closed", dir.display());
}

fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.insert(path.clone(), std::fs::read(&path).unwrap());
            }
        }
    }
    files
}

/// Write each session's turns into its own journal, as Forge does.
async fn write_journals(dir: &Path, turns: Vec<TurnRecord>) {
    let mut by_session: BTreeMap<String, Vec<TurnRecord>> = BTreeMap::new();
    for (i, mut turn) in turns.into_iter().enumerate() {
        // A handful of sessions per kind, not one per turn.
        let session = format!("{}-{}", turn.task_kind, i % 3);
        by_session.entry(session).or_default().push({
            turn.task_id = i as u64;
            turn
        });
    }
    for turns in by_session.into_values() {
        let session_id = new_session_id();
        let journal = Journal::open(dir, session_id).await.unwrap();
        journal.append_session_created(session_id).await.unwrap();
        for mut turn in turns {
            turn.session_id = session_id;
            journal.append_turn_record(&turn).await.unwrap();
        }
    }
}

#[tokio::test]
async fn the_report_reads_every_session_journal_in_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let journals = dir.path().join("journals");
    write_journals(&journals, all_turns()).await;
    std::fs::write(journals.join("notes.txt"), "not a journal").unwrap();
    std::fs::write(journals.join("not-a-session.db"), "not a session").unwrap();
    let corrupt = journals.join(format!("{}.db", new_session_id()));
    std::fs::write(&corrupt, "this is not a SQLite database, not even close").unwrap();
    settle(&journals).await;
    let before = snapshot(dir.path());

    let report = model_report_in(&journals, &options()).await.unwrap();
    assert_eq!(report.journal_dir, journals);
    assert_eq!(report.min_turns, 5);
    // Same numbers as from the records themselves.
    assert_eq!(report.kinds, build_report(all_turns(), &options()));
    assert_eq!(report.skipped.len(), 1, "{:?}", report.skipped);
    assert_eq!(report.skipped[0].path, corrupt);
    assert!(!report.skipped[0].error.is_empty());

    assert_eq!(
        snapshot(dir.path()),
        before,
        "a report must not change any journal"
    );
}

#[tokio::test]
async fn a_missing_journal_folder_is_an_empty_report_and_is_not_created() {
    let dir = tempfile::tempdir().unwrap();
    let journals = dir.path().join("never-written");
    let report = model_report_in(&journals, &options()).await.unwrap();
    assert!(report.kinds.is_empty());
    assert!(report.skipped.is_empty());
    assert!(!journals.exists());
}

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "--initial-branch=main"]);
    git(dir, &["config", "user.email", "forge@example.com"]);
    git(dir, &["config", "user.name", "Forge Test"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

fn repo_config(repo: &Path) -> forge_config::Config {
    forge_config::Config {
        resolved_workspace: repo.to_path_buf(),
        workspace_root: Some(repo.display().to_string()),
        ..Default::default()
    }
}

/// A trust store of its own for each test: empty (nothing trusted), or
/// trusting `workspaces`.
fn trust_store(workspaces: &[&Path]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("trust.toml");
    for workspace in workspaces {
        forge_config::grant_trust_at(&store, workspace).unwrap();
    }
    (dir, store)
}

/// Forge runs no session in a workspace the user has not trusted, so journals
/// found there came from someone else (committed to the repository, say):
/// the report refuses, and reads and creates nothing.
#[tokio::test]
async fn an_untrusted_workspace_is_refused_and_left_untouched() {
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (_trust, store) = trust_store(&[]);
    let before = snapshot(repo.path());
    let error = model_report_with_trust_store(&repo_config(repo.path()), &options(), Some(store))
        .await
        .expect_err("an untrusted workspace is refused");
    let untrusted = error
        .downcast_ref::<UntrustedWorkspace>()
        .expect("the refusal names the reason");
    assert_eq!(untrusted.workspace, repo.path());
    assert_eq!(snapshot(repo.path()), before);
}

/// Reporting on a trusted repository Forge has never run in creates nothing:
/// no `.forge` directory, no change to Git's exclude file.
#[tokio::test]
async fn reporting_on_a_fresh_repository_changes_nothing_in_it() {
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let (_trust, store) = trust_store(&[repo.path()]);
    let before = snapshot(repo.path());
    let report = model_report_with_trust_store(&repo_config(repo.path()), &options(), Some(store))
        .await
        .unwrap();
    assert!(report.kinds.is_empty());
    assert!(!repo.path().join(".forge").exists());
    assert_eq!(snapshot(repo.path()), before);
}

/// Journals committed to a repository are not the user's history; in an
/// untrusted clone they are never opened.
#[tokio::test]
async fn journals_committed_to_an_untrusted_repository_are_not_read() {
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let sessions = repo.path().join(".forge/local/sessions");
    write_journals(&sessions, all_turns()).await;
    settle(&sessions).await;
    git(repo.path(), &["add", "-f", ".forge"]);
    git(repo.path(), &["commit", "-q", "-m", "add journals"]);
    let cfg = repo_config(repo.path());
    let (_trust, store) = trust_store(&[]);

    let error = model_report_with_trust_store(&cfg, &options(), Some(store.clone()))
        .await
        .expect_err("refused");
    assert!(
        error.downcast_ref::<UntrustedWorkspace>().is_some(),
        "{error:#}"
    );
    let error = ranked_models_with_trust_store(&cfg, &TaskKind::Main, &options(), Some(store))
        .await
        .expect_err("refused");
    assert!(
        error.downcast_ref::<UntrustedWorkspace>().is_some(),
        "{error:#}"
    );
}

/// A journal directory the user names is theirs to read: no trust needed.
#[tokio::test]
async fn an_explicit_journal_directory_needs_no_trust() {
    let workspace = tempfile::tempdir().unwrap();
    let journals = tempfile::tempdir().unwrap();
    write_journals(journals.path(), all_turns()).await;
    settle(journals.path()).await;
    let mut cfg = repo_config(workspace.path());
    cfg.journal.path = journals.path().display().to_string();
    let (_trust, store) = trust_store(&[]);
    let report = model_report_with_trust_store(&cfg, &options(), Some(store))
        .await
        .unwrap();
    assert_eq!(report.journal_dir, journals.path());
    assert_eq!(report.kinds, build_report(all_turns(), &options()));
}

/// The report finds the journals where Forge's sessions write them.
#[tokio::test]
async fn ranked_models_reads_the_repositorys_own_journals() {
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let cfg = repo_config(repo.path());
    let (_trust, store) = trust_store(&[repo.path()]);
    let (journal_dir, _) = forge_session::resolve_journal_dir(&cfg);
    write_journals(&journal_dir, all_turns()).await;
    settle(&journal_dir).await;

    let ranked = |kind: TaskKind| {
        let (cfg, store) = (cfg.clone(), store.clone());
        async move {
            ranked_models_with_trust_store(&cfg, &kind, &options(), Some(store))
                .await
                .unwrap()
        }
    };
    let main = ranked(TaskKind::Main).await;
    let order: Vec<(String, Option<usize>, usize)> = main
        .iter()
        .map(|m| (m.model.clone(), m.rank, m.turns))
        .collect();
    assert_eq!(
        order,
        [
            (C.model.to_string(), Some(1), 6),
            (A.model.to_string(), Some(2), 10),
            (B.model.to_string(), Some(3), 8),
            (D.model.to_string(), None, 3),
        ]
    );
    let reviewer = ranked(TaskKind::Custom("reviewer".into())).await;
    assert_eq!(reviewer.len(), 2);
    assert!(ranked(TaskKind::Custom("nobody".into())).await.is_empty());
}

/// Puts a directory's permissions back when dropped, even if the test fails,
/// so the temporary directory can be cleaned up.
#[cfg(unix)]
struct RestorePermissions(PathBuf, u32);

#[cfg(unix)]
impl Drop for RestorePermissions {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(self.1));
    }
}

/// A repository Forge cannot write to keeps its sessions' journals in the
/// application-data directory instead; the report finds them there.
#[cfg(unix)]
#[tokio::test]
async fn the_report_finds_journals_kept_in_the_application_data_fallback() {
    use std::os::unix::fs::PermissionsExt;

    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let cfg = repo_config(repo.path());
    let (_trust, store) = trust_store(&[repo.path()]);
    // No `.forge` can be created in the repository.
    std::fs::set_permissions(repo.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let _restore = RestorePermissions(repo.path().to_path_buf(), 0o755);
    if std::fs::create_dir(repo.path().join(".probe")).is_ok() {
        // Running as root: permissions do not apply, nothing to test.
        return;
    }

    // Where a session of this repository writes its journal.
    let (journal_dir, notices) = forge_session::resolve_journal_dir(&cfg);
    assert!(
        !journal_dir.starts_with(repo.path()),
        "sessions fell back to the application-data directory: {}",
        journal_dir.display()
    );
    assert!(!notices.is_empty(), "the fallback is announced");
    write_journals(&journal_dir, all_turns()).await;
    settle(&journal_dir).await;

    let report = model_report_with_trust_store(&cfg, &options(), Some(store.clone()))
        .await
        .unwrap();
    assert_eq!(report.journal_dir, journal_dir);
    assert_eq!(report.kinds, build_report(all_turns(), &options()));
    let main = ranked_models_with_trust_store(&cfg, &TaskKind::Main, &options(), Some(store))
        .await
        .unwrap();
    let models: Vec<&str> = main.iter().map(|m| m.model.as_str()).collect();
    assert_eq!(models, [C.model, A.model, B.model, D.model]);
    assert!(!repo.path().join(".forge").exists());

    // The fallback directory is keyed by this temporary repository; remove it.
    if let Some(identity_root) = journal_dir.parent() {
        let _ = std::fs::remove_dir_all(identity_root);
    }
}

/// A journal holding a row whose values the report reads cannot be decoded
/// is skipped and listed; the other journals are still reported. (The report
/// reads only the columns it uses; a bad value elsewhere in a row cannot
/// change its numbers. `read_turn_records` rejects any bad value.)
#[tokio::test]
async fn a_malformed_journal_is_skipped_and_the_rest_reported() {
    use sqlx::{ConnectOptions, Connection};

    let dir = tempfile::tempdir().unwrap();
    let journals = dir.path().join("journals");
    write_journals(&journals, all_turns()).await;
    let bad_session = new_session_id();
    drop(Journal::open(&journals, bad_session).await.unwrap());
    let bad = journals.join(format!("{bad_session}.db"));
    let mut conn = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&bad)
        .connect()
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO turn_records (session_id, task_id, ended_at, model, profile_id, \
         outcome, completion_reason, escalated, escalation_retry, prompt_tokens, signals, \
         task_kind) \
         VALUES (?, 1, '2025-06-01T00:00:00+00:00', 'a/b', 'a', \
         'completed', 'x', 0, 0, 'not a number', '[]', 'main')",
    )
    .bind(bad_session.to_string())
    .execute(&mut conn)
    .await
    .unwrap();
    conn.close().await.unwrap();
    settle(&journals).await;

    let report = model_report_in(&journals, &options()).await.unwrap();
    assert_eq!(report.kinds, build_report(all_turns(), &options()));
    assert_eq!(report.skipped.len(), 1, "{:?}", report.skipped);
    assert_eq!(report.skipped[0].path, bad);
}

/// A journal that is a symbolic link is not followed — it could pull in
/// another project's journal, or any file — but listed as skipped.
#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_journal_is_skipped_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let journals = dir.path().join("journals");
    write_journals(&journals, all_turns()).await;
    let other_project = dir.path().join("other-project");
    write_journals(
        &other_project,
        subagent_turns().into_iter().map(|t| t.0).collect(),
    )
    .await;
    settle(&journals).await;
    settle(&other_project).await;
    let target = std::fs::read_dir(&other_project)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "db"))
        .unwrap();
    let link = journals.join(format!("{}.db", new_session_id()));
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let report = model_report_in(&journals, &options()).await.unwrap();
    assert_eq!(
        report.kinds,
        build_report(all_turns(), &options()),
        "the other project's turns are not counted"
    );
    assert_eq!(report.skipped.len(), 1, "{:?}", report.skipped);
    assert_eq!(report.skipped[0].path, link);
    assert!(
        report.skipped[0].error.contains("symbolic link"),
        "{}",
        report.skipped[0].error
    );
}

/// Journals are read concurrently; the report does not depend on which
/// finishes first, and the journals skipped are listed in path order.
#[tokio::test]
async fn many_journals_give_the_same_report_every_time() {
    let dir = tempfile::tempdir().unwrap();
    let journals = dir.path().join("journals");
    // One journal per turn: 46 journals.
    for (i, turn) in all_turns().into_iter().enumerate() {
        let session_id = new_session_id();
        let journal = Journal::open(&journals, session_id).await.unwrap();
        let mut turn = turn;
        turn.session_id = session_id;
        turn.task_id = i as u64;
        journal.append_turn_record(&turn).await.unwrap();
    }
    let mut corrupt = Vec::new();
    for _ in 0..5 {
        let path = journals.join(format!("{}.db", new_session_id()));
        std::fs::write(&path, "not a database").unwrap();
        corrupt.push(path);
    }
    corrupt.sort();
    settle(&journals).await;

    let expected = build_report(all_turns(), &options());
    for _ in 0..5 {
        let report = model_report_in(&journals, &options()).await.unwrap();
        assert_eq!(report.kinds, expected);
        let skipped: Vec<PathBuf> = report.skipped.iter().map(|s| s.path.clone()).collect();
        assert_eq!(skipped, corrupt);
    }
}
