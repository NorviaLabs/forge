//! `forge models report`: the model ranking for a workspace's journals, as a
//! table or as JSON.
//!
//! Fixture (all turns ended a day ago):
//!
//! main
//!   P anthropic/claude-opus-4-1 · anthropic · high — 5 turns, all
//!     completed, one with a passed verification; 12000 tokens, 40 s each.
//!   Q openai_codex/gpt-5.6-luna · openai_codex · medium — 5 turns, one
//!     failed and escalated; 20000 tokens, 60 s each.
//!   R xai/grok-4 · xai · (no effort) — 2 turns: not enough data.
//!   Ranks (n = 2, neutral 1.5):
//!     completion P 1, Q 2; escalation P 1, Q 2; verification P 1, Q 1.5;
//!     approval 1.5, 1.5; queued (both 0) 1.5, 1.5; tokens P 1, Q 2;
//!     active P 1, Q 2.
//!   Scores: P 8/7 ≈ 1.14, Q 12.5/7 ≈ 1.79. Order P, Q, R. 12 turns.
//! custom:reviewer
//!   S anthropic/claude-sonnet-4 · anthropic · low — 5 turns: rank 1,
//!     score 1.00.

use std::path::Path;

use assert_cmd::Command;
use chrono::Utc;
use forge_durable::{new_session_id, Journal, TurnRecord};
use forge_test_support::turn_records::TurnRecordBuilder;
use forge_types::TaskKind;
use serde_json::{json, Value};

fn turn(
    kind: &TaskKind,
    model: &str,
    provider: &str,
    effort: Option<&str>,
    tokens: u64,
    active_ms: u64,
) -> TurnRecord {
    let ended_at = Utc::now() - chrono::Duration::days(1);
    TurnRecordBuilder::new(kind.clone(), model)
        .provider(provider)
        .effort(effort)
        .started_at(Some(
            ended_at - chrono::Duration::milliseconds(active_ms as i64),
        ))
        .ended_at(ended_at)
        .tokens(Some(tokens - 500), Some(500))
        .cache_tokens(Some(0), Some(0))
        .duration_ms(Some(active_ms))
        .build()
}

fn fixture() -> Vec<(TaskKind, Vec<TurnRecord>)> {
    let main = TaskKind::Main;
    let reviewer = TaskKind::Custom("reviewer".into());
    let mut main_turns = Vec::new();
    for i in 0..5 {
        let mut p = turn(
            &main,
            "anthropic/claude-opus-4-1",
            "anthropic",
            Some("high"),
            12000,
            40000,
        );
        if i == 0 {
            p.signals = json!([{"kind": "verification_passed", "detail": "cargo test"}]);
        }
        main_turns.push(p);
        let mut q = turn(
            &main,
            "openai_codex/gpt-5.6-luna",
            "openai_codex",
            Some("medium"),
            20000,
            60000,
        );
        if i == 0 {
            q.outcome = "failed".into();
            q.completion_reason = "dangling_tool_call_text".into();
            q.escalated = true;
        }
        main_turns.push(q);
    }
    for _ in 0..2 {
        main_turns.push(turn(&main, "xai/grok-4", "xai", None, 3000, 5000));
    }
    let custom_turns = (0..5)
        .map(|_| {
            turn(
                &reviewer,
                "anthropic/claude-sonnet-4",
                "anthropic",
                Some("low"),
                3000,
                8000,
            )
        })
        .collect();
    vec![(main, main_turns), (reviewer, custom_turns)]
}

/// One journal per kind, written the way sessions write them.
fn write_fixture(journals: &Path) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        for (_, turns) in fixture() {
            let session_id = new_session_id();
            let journal = Journal::open(journals, session_id).await.unwrap();
            journal.append_session_created(session_id).await.unwrap();
            for mut turn in turns {
                turn.session_id = session_id;
                journal.append_turn_record(&turn).await.unwrap();
            }
        }
    });
}

struct Workspace {
    dir: tempfile::TempDir,
}

impl Workspace {
    fn with_fixture() -> Self {
        let dir = tempfile::tempdir().unwrap();
        write_fixture(&dir.path().join("journals"));
        Self { dir }
    }

    fn empty() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn journals(&self) -> std::path::PathBuf {
        self.dir.path().join("journals")
    }

    fn report(&self, extra: &[&str]) -> Command {
        let mut command = Command::cargo_bin("forge").unwrap();
        command
            .args(["models", "report", "--workspace"])
            .arg(self.dir.path())
            .arg("--journal")
            .arg(self.journals())
            .args(extra);
        command
    }
}

fn stdout(command: &mut Command) -> String {
    let output = command.assert().success().get_output().clone();
    String::from_utf8(output.stdout).unwrap()
}

fn json_report(command: &mut Command) -> Value {
    let out = stdout(command);
    serde_json::from_str(&out).unwrap_or_else(|error| panic!("{error}: {out}"))
}

fn line_with<'a>(out: &'a str, needle: &str) -> (usize, &'a str) {
    out.lines()
        .enumerate()
        .find(|(_, line)| line.contains(needle))
        .unwrap_or_else(|| panic!("no line with `{needle}` in:\n{out}"))
}

// ------------------------------------------------------------------- help

#[test]
fn help_lists_the_models_report_command_and_its_options() {
    let out = stdout(Command::cargo_bin("forge").unwrap().arg("--help"));
    assert!(out.contains("models"), "{out}");
    let out = stdout(
        Command::cargo_bin("forge")
            .unwrap()
            .args(["models", "--help"]),
    );
    assert!(out.contains("report"), "{out}");
    let out = stdout(
        Command::cargo_bin("forge")
            .unwrap()
            .args(["models", "report", "--help"]),
    );
    for option in [
        "--workspace",
        "--journal",
        "--kind",
        "--since",
        "--min-turns",
        "--json",
    ] {
        assert!(out.contains(option), "`{option}` missing from:\n{out}");
    }
}

// ------------------------------------------------------------------ table

#[test]
fn the_table_ranks_each_kind_with_its_turn_counts() {
    let workspace = Workspace::with_fixture();
    let out = stdout(&mut workspace.report(&[]));

    let (main_at, _) = line_with(&out, "main (12 turns)");
    let (custom_at, _) = line_with(&out, "custom:reviewer (5 turns)");
    assert!(main_at < custom_at, "main comes first:\n{out}");

    let (header_at, header) = line_with(&out, "MODEL");
    for column in [
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
    ] {
        assert!(header.contains(column), "`{column}` missing: {header}");
    }
    assert!(header_at > main_at);

    let (p_at, p) = line_with(&out, "anthropic/claude-opus-4-1");
    let (q_at, q) = line_with(&out, "openai_codex/gpt-5.6-luna");
    let (r_at, r) = line_with(&out, "xai/grok-4");
    assert!(main_at < p_at && p_at < q_at && q_at < r_at && r_at < custom_at);

    let cells =
        |line: &str| -> Vec<String> { line.split_whitespace().map(str::to_string).collect() };
    let p = cells(p);
    assert_eq!(
        p,
        [
            "1",
            "anthropic/claude-opus-4-1",
            "anthropic",
            "high",
            "5",
            "1.14",
            "100%",
            "0%",
            "1/0",
            "0/0",
            "0%",
            "12000",
            "40.0s"
        ]
    );
    let q = cells(q);
    assert_eq!(
        q,
        [
            "2",
            "openai_codex/gpt-5.6-luna",
            "openai_codex",
            "medium",
            "5",
            "1.79",
            "80%",
            "20%",
            "0/0",
            "0/0",
            "0%",
            "20000",
            "60.0s"
        ]
    );
    // Unranked: `-` for rank and score, the numbers still shown, and a note.
    assert_eq!(
        cells(r),
        [
            "-",
            "xai/grok-4",
            "xai",
            "default",
            "2",
            "-",
            "100%",
            "0%",
            "0/0",
            "0/0",
            "0%",
            "3000",
            "5.0s",
            "not",
            "enough",
            "data"
        ]
    );

    let (_, s) = line_with(&out, "anthropic/claude-sonnet-4");
    let s = cells(s);
    assert_eq!(
        &s[..6],
        [
            "1",
            "anthropic/claude-sonnet-4",
            "anthropic",
            "low",
            "5",
            "1.00"
        ]
    );
}

#[test]
fn a_kind_filter_prints_that_kind_only() {
    let workspace = Workspace::with_fixture();
    let out = stdout(&mut workspace.report(&["--kind", "custom:reviewer"]));
    assert!(out.contains("custom:reviewer (5 turns)"), "{out}");
    assert!(!out.contains("main ("), "{out}");
    assert!(!out.contains("xai/grok-4"), "{out}");
}

#[test]
fn no_matching_turns_says_so() {
    let workspace = Workspace::with_fixture();
    for args in [&["--kind", "subagent"][..], &["--since", "1h"][..]] {
        let out = stdout(&mut workspace.report(args));
        assert!(out.contains("No turn records found."), "{args:?}: {out}");
    }
}

/// A user of its own for the command: an empty config directory (so no
/// real trust file or config is read) whose trust file lists `trusted`.
struct IsolatedUser {
    dir: tempfile::TempDir,
}

impl IsolatedUser {
    fn trusting(trusted: &[&Path]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let user = Self { dir };
        let store = user.config_dir().join("forge").join("trust.toml");
        for workspace in trusted {
            forge_config::grant_trust_at(&store, workspace).unwrap();
        }
        user
    }

    fn home(&self) -> std::path::PathBuf {
        self.dir.path().join("home")
    }

    /// Where `dirs::config_dir()` points with this user's environment.
    fn config_dir(&self) -> std::path::PathBuf {
        if cfg!(target_os = "macos") {
            self.home().join("Library").join("Application Support")
        } else {
            self.dir.path().join("config")
        }
    }

    fn apply(&self, command: &mut Command) {
        command
            .env("HOME", self.home())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"));
    }
}

#[test]
fn an_empty_trusted_workspace_has_no_turn_records_and_is_left_untouched() {
    let workspace = Workspace::empty();
    let user = IsolatedUser::trusting(&[workspace.dir.path()]);
    let mut command = Command::cargo_bin("forge").unwrap();
    command
        .args(["models", "report", "--workspace"])
        .arg(workspace.dir.path());
    user.apply(&mut command);
    let out = stdout(&mut command);
    assert!(out.contains("No turn records found."), "{out}");
    assert_eq!(
        std::fs::read_dir(workspace.dir.path()).unwrap().count(),
        0,
        "the report created files in the workspace"
    );
}

/// Without trust there is nothing of the user's to report: the command says
/// why and how to read a journal directory anyway, and touches nothing.
#[test]
fn an_untrusted_workspace_is_refused_with_a_way_forward() {
    let workspace = Workspace::empty();
    let user = IsolatedUser::trusting(&[]);
    let mut command = Command::cargo_bin("forge").unwrap();
    command
        .args(["models", "report", "--workspace"])
        .arg(workspace.dir.path());
    user.apply(&mut command);
    let output = command.assert().failure().get_output().clone();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not trusted"), "{stderr}");
    assert!(stderr.contains("--journal"), "{stderr}");
    assert!(output.stdout.is_empty());
    assert_eq!(std::fs::read_dir(workspace.dir.path()).unwrap().count(), 0);
}

/// A relative `--journal` is relative to where the command runs, like any
/// path argument, and is the user's own choice: no trust needed.
#[test]
fn a_relative_journal_directory_is_read_from_the_current_directory() {
    let here = Workspace::with_fixture();
    let elsewhere = Workspace::empty();
    let user = IsolatedUser::trusting(&[]);
    let mut command = Command::cargo_bin("forge").unwrap();
    command
        .current_dir(here.dir.path())
        .args(["models", "report", "--journal", "journals", "--json"])
        .arg("--workspace")
        .arg(elsewhere.dir.path());
    user.apply(&mut command);
    let report = json_report(&mut command);
    assert_eq!(report["kinds"][0]["turns"], 12);
}

/// How the table rounds, and what it shows for a value it does not know.
///
/// G1 test/rounding · test · high — 6 turns: turn 0 failed and escalated,
///   turn 1 had a queued message; tokens 1000 ×3 and 1001 ×3, 12340 ms each.
///   DONE 5/6 = 83.3% → 83%; ESC 1/6 = 16.7% → 17%; QUEUED 17%;
///   TOKENS median (1000 + 1001) / 2 = 1000.5 → 1001; ACTIVE 12.34 s → 12.3s.
/// G2 test/unknown · test · (no effort) — 5 turns, all completed, tokens
///   and duration unknown: TOKENS `-`, ACTIVE `-`.
/// Ranks (n = 2, neutral 1.5):
///   completion G2 1, G1 2; escalation G2 1, G1 2; verification 1.5, 1.5;
///   approval 1.5, 1.5; queued G2 1, G1 2; tokens G1 1, G2 1.5 (neutral);
///   active G1 1, G2 1.5 (neutral).
/// Scores: G1 11/7 ≈ 1.571 → 1.57; G2 9/7 ≈ 1.286 → 1.29. Order G2, G1.
#[test]
fn the_table_rounds_and_shows_unknown_values_as_a_dash() {
    let dir = tempfile::tempdir().unwrap();
    let journals = dir.path().join("journals");
    let main = TaskKind::Main;
    let mut turns = Vec::new();
    for i in 0..6u64 {
        let tokens = if i < 3 { 1000 } else { 1001 };
        let mut t = turn(&main, "test/rounding", "test", Some("high"), tokens, 12340);
        if i == 0 {
            t.outcome = "failed".into();
            t.completion_reason = "dangling_tool_call_text".into();
            t.escalated = true;
        }
        if i == 1 {
            t.signals = json!([{"kind": "user_message_queued"}]);
        }
        turns.push(t);
    }
    for _ in 0..5 {
        let mut t = turn(&main, "test/unknown", "test", None, 1000, 1000);
        t.prompt_tokens = None;
        t.duration_ms = None;
        t.waiting_ms = None;
        turns.push(t);
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let session_id = new_session_id();
        let journal = Journal::open(&journals, session_id).await.unwrap();
        for mut t in turns {
            t.session_id = session_id;
            journal.append_turn_record(&t).await.unwrap();
        }
    });

    let mut command = Command::cargo_bin("forge").unwrap();
    command
        .args(["models", "report", "--workspace"])
        .arg(dir.path())
        .arg("--journal")
        .arg(&journals);
    let out = stdout(&mut command);
    let cells =
        |line: &str| -> Vec<String> { line.split_whitespace().map(str::to_string).collect() };
    let (g2_at, g2) = line_with(&out, "test/unknown");
    let (g1_at, g1) = line_with(&out, "test/rounding");
    assert!(g2_at < g1_at, "{out}");
    assert_eq!(
        cells(g1),
        [
            "2",
            "test/rounding",
            "test",
            "high",
            "6",
            "1.57",
            "83%",
            "17%",
            "0/0",
            "0/0",
            "17%",
            "1001",
            "12.3s"
        ]
    );
    assert_eq!(
        cells(g2),
        [
            "1",
            "test/unknown",
            "test",
            "default",
            "5",
            "1.29",
            "100%",
            "0%",
            "0/0",
            "0/0",
            "0%",
            "-",
            "-"
        ]
    );
}

// ------------------------------------------------------------------- json

#[test]
fn json_has_the_same_ranking_with_every_number() {
    let workspace = Workspace::with_fixture();
    let report = json_report(&mut workspace.report(&["--json"]));

    assert_eq!(report["min_turns"], 5);
    assert_eq!(report["since"], Value::Null);
    assert_eq!(
        report["journal_dir"].as_str().map(std::path::PathBuf::from),
        Some(workspace.journals())
    );
    assert_eq!(report["skipped"], json!([]));

    let kinds = report["kinds"].as_array().unwrap();
    assert_eq!(kinds.len(), 2);
    assert_eq!(kinds[0]["kind"], "main");
    assert_eq!(kinds[0]["turns"], 12);
    assert_eq!(kinds[1]["kind"], "custom:reviewer");
    assert_eq!(kinds[1]["turns"], 5);

    let models = kinds[0]["models"].as_array().unwrap();
    let names: Vec<&str> = models
        .iter()
        .map(|m| m["model"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "anthropic/claude-opus-4-1",
            "openai_codex/gpt-5.6-luna",
            "xai/grok-4"
        ]
    );
    let p = &models[0];
    assert_eq!(p["rank"], 1);
    assert_eq!(p["status"], "ranked");
    assert!((p["score"].as_f64().unwrap() - 8.0 / 7.0).abs() < 1e-9);
    assert_eq!(p["provider"], "anthropic");
    assert_eq!(p["effort"], "high");
    assert_eq!(p["turns"], 5);
    assert_eq!(p["completed"], 5);
    assert_eq!(p["completion_rate"], 1.0);
    assert_eq!(p["verification_passed"], 1);
    assert_eq!(p["verification_pass_rate"], 1.0);
    assert_eq!(p["approval_rate"], Value::Null);
    assert_eq!(p["median_tokens"], 12000.0);
    assert_eq!(p["median_active_ms"], 40000.0);
    assert_eq!(p["ranks"]["verification"], 1.0);

    let q = &models[1];
    assert_eq!(q["rank"], 2);
    assert!((q["score"].as_f64().unwrap() - 12.5 / 7.0).abs() < 1e-9);
    assert_eq!(q["escalated"], 1);
    assert_eq!(q["escalation_rate"], 0.2);
    assert_eq!(q["ranks"]["verification"], 1.5);

    let r = &models[2];
    assert_eq!(r["rank"], Value::Null);
    assert_eq!(r["status"], "insufficient_data");
    assert_eq!(r["score"], Value::Null);
    assert_eq!(r["effort"], Value::Null);
    assert_eq!(r["turns"], 2);
    assert_eq!(r["ranks"], Value::Null);
}

/// With `--min-turns 2`, R (2 turns) is ranked too (n = 3, neutral 2):
///   completion   P 1.5, R 1.5, Q 3   (P and R both 1.0)
///   escalation   P 1.5, R 1.5, Q 3
///   verification P 1,   Q 2,   R 2   (only P ran one)
///   approval     2, 2, 2              (none asked)
///   queued       2, 2, 2              (all 0: a three-way tie)
///   tokens       R 1 (3000), P 2, Q 3
///   active       R 1 (5 s),  P 2, Q 3
/// Scores: R 11/7 ≈ 1.571, P 12/7 ≈ 1.714, Q 18/7 ≈ 2.571. Order R, P, Q.
#[test]
fn json_options_are_applied_and_echoed() {
    let workspace = Workspace::with_fixture();
    let report = json_report(&mut workspace.report(&[
        "--json",
        "--min-turns",
        "2",
        "--since",
        "7d",
        "--kind",
        "main",
    ]));
    assert_eq!(report["min_turns"], 2);
    assert!(report["since"].is_string(), "{}", report["since"]);
    let kinds = report["kinds"].as_array().unwrap();
    assert_eq!(kinds.len(), 1);
    let models = kinds[0]["models"].as_array().unwrap();
    let ranked: Vec<(&str, i64, f64)> = models
        .iter()
        .map(|m| {
            (
                m["model"].as_str().unwrap(),
                m["rank"].as_i64().expect("two turns are enough now"),
                m["score"].as_f64().unwrap(),
            )
        })
        .collect();
    let expected = [
        ("xai/grok-4", 1, 11.0 / 7.0),
        ("anthropic/claude-opus-4-1", 2, 12.0 / 7.0),
        ("openai_codex/gpt-5.6-luna", 3, 18.0 / 7.0),
    ];
    assert_eq!(ranked.len(), expected.len());
    for ((model, rank, score), (e_model, e_rank, e_score)) in ranked.iter().zip(expected) {
        assert_eq!((*model, *rank), (e_model, e_rank));
        assert!(
            (score - e_score).abs() < 1e-9,
            "{model}: {score} != {e_score}"
        );
    }
}

#[test]
fn json_for_an_empty_workspace_has_no_kinds() {
    let workspace = Workspace::empty();
    let report = json_report(&mut workspace.report(&["--json"]));
    assert_eq!(report["kinds"], json!([]));
    assert_eq!(report["skipped"], json!([]));
}

// ----------------------------------------------------------------- errors

#[test]
fn an_unknown_kind_or_since_is_refused() {
    let workspace = Workspace::with_fixture();
    for args in [
        &["--kind", "unknown"][..],
        &["--kind", "read_only"][..],
        &["--kind", "custom:"][..],
        &["--since", "7w"][..],
        &["--since", "yesterday"][..],
        &["--min-turns", "-1"][..],
    ] {
        let output = workspace
            .report(args)
            .assert()
            .failure()
            .get_output()
            .clone();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.is_empty(), "{args:?} failed without a message");
        assert!(output.stdout.is_empty(), "{args:?} printed a report");
    }
}

// --------------------------------------------------------------- security

/// Every character of `text` a terminal would act on rather than print.
fn control_characters(text: &str) -> Vec<char> {
    text.chars()
        .filter(|c| c.is_control() && *c != '\n')
        .collect()
}

/// Names come from journals and provider catalogs; the table must not pass
/// terminal control sequences through (clear the screen, retitle the window,
/// write the clipboard with OSC 52, …). They are shown escaped instead.
#[test]
fn control_characters_in_names_are_shown_escaped() {
    let dir = tempfile::tempdir().unwrap();
    let journals = dir.path().join("journals");
    let main = TaskKind::Main;
    let model = "evil/\u{1b}[2J\u{1b}]52;c;cHduZWQ=\u{7}model\u{7f}\u{9b}31m";
    let turns: Vec<TurnRecord> = (0..5)
        .map(|_| turn(&main, model, "prov\rider", Some("hi\u{1b}gh"), 1000, 1000))
        .collect();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let session_id = new_session_id();
        let journal = Journal::open(&journals, session_id).await.unwrap();
        for mut t in turns {
            t.session_id = session_id;
            journal.append_turn_record(&t).await.unwrap();
        }
    });

    let mut command = Command::cargo_bin("forge").unwrap();
    command
        .args(["models", "report", "--workspace"])
        .arg(dir.path())
        .arg("--journal")
        .arg(&journals);
    let out = stdout(&mut command);
    let raw = control_characters(&out);
    assert!(
        raw.is_empty(),
        "raw control characters {raw:?} in:\n{out:?}"
    );
    assert!(
        out.contains(r"evil/\u{1b}[2J\u{1b}]52;c;cHduZWQ=\u{7}model\u{7f}\u{9b}31m"),
        "{out}"
    );
    assert!(out.contains(r"prov\rider"), "{out}");
    assert!(out.contains(r"hi\u{1b}gh"), "{out}");
}

/// The same for the warnings about journals that could not be read.
#[test]
fn control_characters_in_warnings_are_shown_escaped() {
    let dir = tempfile::tempdir().unwrap();
    let journals = dir.path().join("jour\u{1b}]0;x\u{7}nals");
    std::fs::create_dir_all(&journals).unwrap();
    std::fs::write(
        journals.join(format!("{}.db", new_session_id())),
        "not a database",
    )
    .unwrap();
    let mut command = Command::cargo_bin("forge").unwrap();
    command
        .args(["models", "report", "--workspace"])
        .arg(dir.path())
        .arg("--journal")
        .arg(&journals);
    let output = command.assert().success().get_output().clone();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains("warning: skipped"), "{stderr}");
    let raw = control_characters(&stderr);
    assert!(
        raw.is_empty(),
        "raw control characters {raw:?} in:\n{stderr:?}"
    );
}
