//! Auto for the main session: where each turn starts
//! (`forge_session::auto_models`) — the ranked best model and effort for
//! `main` work on the fallback's provider, or the fallback the user set when
//! the evidence is thin — and the efforts the step-up ladder climbs.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::{
    AutoFallback, AutoModelAdvisor, ChildModel, ChildModelRequest, ChildModelResolver, ModelEfforts,
};
use forge_durable::TurnRecord;
use forge_session::auto_models::{AutoSettings, EffortLookup, RankedAutoModels};
use forge_session::model_report::RankingSnapshot;
use forge_session::subagent_models::{RankingSource, RunnableModels};
use forge_test_support::turn_records::TurnRecordBuilder;
use forge_types::TaskKind;

const OPUS: &str = "anthropic/claude-opus-4-1";
const SONNET: &str = "anthropic/claude-sonnet-4";
const GPT: &str = "openai/gpt-5";

fn turns(
    kind: &TaskKind,
    model: &str,
    effort: Option<&str>,
    n: usize,
    failed: usize,
) -> Vec<TurnRecord> {
    (0..n)
        .map(|i| {
            TurnRecordBuilder::new(kind.clone(), model)
                .task_id(i as u64)
                .effort(effort)
                .completed(i >= failed)
                .build()
        })
        .collect()
}

/// Main-session work: on anthropic, opus @ high (12 turns, all completed)
/// leads sonnet @ low (10 turns, 3 failed); on openai, gpt-5 would lead
/// too — but the fallback is on anthropic.
fn main_history() -> Vec<TurnRecord> {
    let main = TaskKind::Main;
    [
        turns(&main, OPUS, Some("high"), 12, 0),
        turns(&main, SONNET, Some("low"), 10, 3),
        turns(&main, GPT, Some("high"), 12, 0),
    ]
    .concat()
}

struct Fixed {
    records: Vec<TurnRecord>,
    builds: AtomicUsize,
    delay: Duration,
}

impl Fixed {
    fn new(records: Vec<TurnRecord>) -> Arc<Self> {
        Self::slow(records, Duration::ZERO)
    }

    fn slow(records: Vec<TurnRecord>, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            records,
            builds: AtomicUsize::new(0),
            delay,
        })
    }
}

impl RankingSource for Fixed {
    fn build(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<RankingSnapshot>> + Send>> {
        self.builds.fetch_add(1, Ordering::SeqCst);
        let records = self.records.clone();
        let delay = self.delay;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            Ok(RankingSnapshot::from_records(records, None))
        })
    }
}

struct RunsAll;

impl RunnableModels for RunsAll {
    fn can_run(&self, _provider: &str, _model: &str) -> Option<bool> {
        Some(true)
    }
}

struct Shape;

impl ChildModelResolver for Shape {
    fn resolve(&self, request: &ChildModelRequest<'_>) -> Result<ChildModel, String> {
        forge_core::resolve_child_model_shape(request)
    }
}

/// Efforts by model: opus has low … max, everything else none.
struct Efforts;

impl EffortLookup for Efforts {
    fn efforts(&self, model: &str, _profile_id: &str) -> ModelEfforts {
        match model {
            OPUS => ModelEfforts {
                options: ["low", "medium", "high", "max"].map(String::from).to_vec(),
                default: Some("low".into()),
            },
            _ => ModelEfforts::default(),
        }
    }
}

fn fallback() -> AutoFallback {
    AutoFallback {
        model: SONNET.into(),
        profile_id: "anthropic".into(),
        reasoning_effort: Some("medium".into()),
    }
}

fn settings(trusted: bool) -> AutoSettings {
    AutoSettings {
        trusted,
        stale_after: Duration::from_secs(3600),
    }
}

async fn ready(records: Vec<TurnRecord>) -> Arc<RankedAutoModels> {
    let advisor = RankedAutoModels::new(
        Fixed::new(records),
        Arc::new(RunsAll),
        Arc::new(Shape),
        Arc::new(Efforts),
        settings(true),
    );
    advisor.refresh().await.unwrap();
    advisor
}

#[tokio::test]
async fn a_turn_starts_on_the_ranked_best_on_the_fallbacks_provider() {
    let advisor = ready(main_history()).await;
    let start = advisor.start(&fallback());
    assert!(start.ranked);
    assert_eq!(
        start.model.model, OPUS,
        "not openai's gpt-5: another provider"
    );
    assert_eq!(start.model.reasoning_effort.as_deref(), Some("high"));
    assert!(start.reason.contains(OPUS), "{}", start.reason);
    let evidence = start.evidence.expect("evidence");
    assert_eq!(evidence.turns, 22);
    assert_eq!(evidence.ranked_groups, 2);
}

#[tokio::test]
async fn thin_evidence_starts_on_the_fallback_and_says_why() {
    let main = TaskKind::Main;
    let history = [
        turns(&main, OPUS, Some("high"), 6, 0),
        turns(&main, SONNET, Some("low"), 6, 2),
    ]
    .concat();
    let advisor = ready(history).await;
    let start = advisor.start(&fallback());
    assert!(!start.ranked);
    assert_eq!(start.model.model, SONNET);
    assert_eq!(start.model.reasoning_effort.as_deref(), Some("medium"));
    assert!(
        start.reason.contains("12") && start.reason.contains("20"),
        "{}",
        start.reason
    );
}

/// Subagents' work is another kind: it does not choose the main session's
/// model.
#[tokio::test]
async fn only_main_session_work_counts() {
    let sub = TaskKind::Subagent;
    let history = [
        turns(&sub, OPUS, Some("high"), 30, 0),
        turns(&sub, SONNET, Some("low"), 30, 10),
    ]
    .concat();
    let advisor = ready(history).await;
    let start = advisor.start(&fallback());
    assert!(!start.ranked);
    assert_eq!(start.model.model, SONNET);
}

#[tokio::test]
async fn an_untrusted_workspace_starts_on_the_fallback_without_reading_journals() {
    let source = Fixed::new(main_history());
    let advisor = RankedAutoModels::new(
        source.clone(),
        Arc::new(RunsAll),
        Arc::new(Shape),
        Arc::new(Efforts),
        settings(false),
    );
    advisor.refresh().await.unwrap();
    let start = advisor.start(&fallback());
    assert!(!start.ranked);
    assert!(start.reason.contains("not trusted"), "{}", start.reason);
    assert_eq!(source.builds.load(Ordering::SeqCst), 0);
}

/// A turn never waits for the ranking: the fallback at once, the ranking
/// built in the background and used by a later turn.
#[tokio::test(flavor = "multi_thread")]
async fn a_turn_never_waits_for_the_ranking() {
    let advisor = RankedAutoModels::new(
        Fixed::slow(main_history(), Duration::from_millis(300)),
        Arc::new(RunsAll),
        Arc::new(Shape),
        Arc::new(Efforts),
        settings(true),
    );
    let started = Instant::now();
    let start = advisor.start(&fallback());
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(!start.ranked);
    assert!(start.reason.contains("not built"), "{}", start.reason);
    for _ in 0..100 {
        if advisor.start(&fallback()).ranked {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the background build was never used");
}

#[tokio::test]
async fn the_ladders_efforts_come_from_the_lookup() {
    let advisor = ready(main_history()).await;
    assert_eq!(
        advisor.efforts(OPUS, "anthropic"),
        Efforts.efforts(OPUS, "anthropic")
    );
    assert_eq!(
        advisor.efforts(SONNET, "anthropic"),
        ModelEfforts::default()
    );
}
