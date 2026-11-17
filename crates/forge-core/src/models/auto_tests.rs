//! Auto for the main session, the parts that are pure: the step-up ladder
//! (effort before model) and when to move along it.

use crate::{
    ladder, AutoDecision, AutoFallback, AutoModel, AutoSource, ChildModel, LadderMove,
    ModelEfforts, TurnEnd,
};

fn model(id: &str, effort: Option<&str>) -> ChildModel {
    ChildModel {
        model: id.into(),
        profile_id: id.split('/').next().unwrap().into(),
        reasoning_effort: effort.map(str::to_string),
        context_window: None,
        output_reserve: None,
        image_input: None,
    }
}

fn efforts(options: &[&str], default: Option<&str>) -> ModelEfforts {
    ModelEfforts {
        options: options.iter().map(|o| o.to_string()).collect(),
        default: default.map(str::to_string),
    }
}

/// `(model, effort)` of each rung.
fn rungs(ladder: &[ChildModel]) -> Vec<(String, Option<String>)> {
    ladder
        .iter()
        .map(|rung| (rung.model.clone(), rung.reasoning_effort.clone()))
        .collect()
}

fn rung(model: &str, effort: Option<&str>) -> (String, Option<String>) {
    (model.to_string(), effort.map(str::to_string))
}

const MID: &str = "test/mid";
const BIG: &str = "test/big";
const SMALL: &str = "test/small";

fn mid_efforts() -> ModelEfforts {
    efforts(&["low", "medium", "high"], Some("medium"))
}

fn big_efforts() -> ModelEfforts {
    efforts(&["medium", "high", "max"], Some("high"))
}

// ---------------------------------------------------------------- ladder

/// Effort first: every higher effort on the same model, then the stronger
/// model at its own effort, then its higher efforts.
#[test]
fn the_ladder_raises_effort_before_changing_model() {
    let strong = model(BIG, Some("high"));
    let ladder = ladder(
        &model(MID, Some("medium")),
        &mid_efforts(),
        Some((&strong, &big_efforts())),
    );
    assert_eq!(
        rungs(&ladder),
        [
            rung(MID, Some("medium")),
            rung(MID, Some("high")),
            rung(BIG, Some("high")),
            rung(BIG, Some("max")),
        ]
    );
}

/// A start with no effort named runs at the model's default; the ladder
/// climbs from there.
#[test]
fn a_start_without_an_effort_climbs_from_the_models_default() {
    let strong = model(BIG, Some("high"));
    let ladder = ladder(
        &model(MID, None),
        &mid_efforts(),
        Some((&strong, &big_efforts())),
    );
    assert_eq!(
        rungs(&ladder),
        [
            rung(MID, None),
            rung(MID, Some("high")),
            rung(BIG, Some("high")),
            rung(BIG, Some("max")),
        ]
    );
}

/// A model with no effort setting has nothing to raise: the next step is
/// the stronger model.
#[test]
fn a_model_without_an_effort_setting_steps_straight_to_the_stronger_model() {
    let strong = model(BIG, Some("high"));
    let ladder = ladder(
        &model(SMALL, None),
        &efforts(&[], None),
        Some((&strong, &big_efforts())),
    );
    assert_eq!(
        rungs(&ladder),
        [
            rung(SMALL, None),
            rung(BIG, Some("high")),
            rung(BIG, Some("max"))
        ]
    );
}

/// Starting on the stronger model itself: only its efforts are left.
#[test]
fn starting_on_the_stronger_model_leaves_only_its_efforts() {
    let strong = model(BIG, Some("high"));
    let ladder = ladder(
        &model(BIG, Some("medium")),
        &big_efforts(),
        Some((&strong, &big_efforts())),
    );
    assert_eq!(
        rungs(&ladder),
        [
            rung(BIG, Some("medium")),
            rung(BIG, Some("high")),
            rung(BIG, Some("max")),
        ]
    );
}

/// No stronger model to move to (no strong tier for the provider): the
/// ladder ends with the start model's highest effort.
#[test]
fn without_a_stronger_model_the_ladder_is_the_efforts_alone() {
    let efforts_only = ladder(&model(MID, Some("medium")), &mid_efforts(), None);
    assert_eq!(
        rungs(&efforts_only),
        [rung(MID, Some("medium")), rung(MID, Some("high"))]
    );
    let top = ladder(&model(BIG, Some("max")), &big_efforts(), None);
    assert_eq!(rungs(&top), [rung(BIG, Some("max"))]);
}

// ------------------------------------------------------------ moving on it

fn auto() -> AutoModel {
    AutoModel::new(AutoFallback {
        model: SMALL.into(),
        profile_id: "test".into(),
        reasoning_effort: None,
    })
}

/// Each failed turn moves one step up; at the top, it stays.
#[test]
fn each_failure_moves_one_step_up_and_the_top_holds() {
    let mut auto = auto();
    let top = 3;
    for expected in [1, 2, 3] {
        assert_eq!(auto.after_turn(TurnEnd::Failed, top), Some(LadderMove::Up));
        assert_eq!(auto.position, expected);
    }
    assert_eq!(auto.after_turn(TurnEnd::Failed, top), None);
    assert_eq!(auto.position, 3);
}

/// Back down one step after two successful turns in a row.
#[test]
fn two_successes_in_a_row_step_down_one() {
    let mut auto = auto();
    auto.after_turn(TurnEnd::Failed, 3);
    auto.after_turn(TurnEnd::Failed, 3);
    assert_eq!(auto.position, 2);
    assert_eq!(auto.after_turn(TurnEnd::Succeeded, 3), None);
    assert_eq!(auto.position, 2, "one success is not enough");
    assert_eq!(
        auto.after_turn(TurnEnd::Succeeded, 3),
        Some(LadderMove::Down)
    );
    assert_eq!(auto.position, 1);
    // The count starts again after a move.
    assert_eq!(auto.after_turn(TurnEnd::Succeeded, 3), None);
    assert_eq!(
        auto.after_turn(TurnEnd::Succeeded, 3),
        Some(LadderMove::Down)
    );
    assert_eq!(auto.position, 0);
    // Never below the start.
    auto.after_turn(TurnEnd::Succeeded, 3);
    assert_eq!(auto.after_turn(TurnEnd::Succeeded, 3), None);
    assert_eq!(auto.position, 0);
}

#[test]
fn a_failure_resets_the_run_of_successes() {
    let mut auto = auto();
    auto.after_turn(TurnEnd::Failed, 5);
    auto.after_turn(TurnEnd::Failed, 5);
    auto.after_turn(TurnEnd::Succeeded, 5);
    auto.after_turn(TurnEnd::Failed, 5);
    assert_eq!(auto.position, 3);
    assert_eq!(auto.after_turn(TurnEnd::Succeeded, 5), None);
    assert_eq!(auto.position, 3);
}

/// A turn that says nothing about the model — a provider error, a denied
/// approval, a cancellation, a turn on a tier the user asked for — neither
/// counts nor resets.
#[test]
fn a_neutral_turn_neither_counts_nor_resets() {
    let mut auto = auto();
    auto.after_turn(TurnEnd::Failed, 3);
    auto.after_turn(TurnEnd::Failed, 3);
    auto.after_turn(TurnEnd::Succeeded, 3);
    assert_eq!(auto.after_turn(TurnEnd::Neutral, 3), None);
    assert_eq!(
        auto.after_turn(TurnEnd::Succeeded, 3),
        Some(LadderMove::Down)
    );
}

/// A ladder can get shorter between turns (the ranking picked another
/// start): the position is kept within it.
#[test]
fn a_position_past_a_shorter_ladder_is_kept_at_its_top() {
    let mut auto = auto();
    for _ in 0..4 {
        auto.after_turn(TurnEnd::Failed, 5);
    }
    assert_eq!(auto.position, 4);
    assert_eq!(auto.after_turn(TurnEnd::Failed, 2), None);
    assert_eq!(auto.position, 2);
    assert_eq!(auto.step_on(3), 2);
    assert_eq!(auto.step_on(10), 2);
}

// ---------------------------------------------------------- serialization

#[test]
fn auto_and_its_decisions_read_back_as_written() {
    let mut auto = auto();
    auto.after_turn(TurnEnd::Failed, 3);
    let value = serde_json::to_value(&auto).unwrap();
    assert_eq!(serde_json::from_value::<AutoModel>(value).unwrap(), auto);

    let decision = AutoDecision {
        task_id: 7,
        user_message: 2,
        model: MID.into(),
        profile_id: "test".into(),
        reasoning_effort: Some("high".into()),
        source: AutoSource::SteppedUp,
        step: 1,
        steps: 4,
        reason: "step 1 of 3 above the start, after a failed turn".into(),
    };
    let value = serde_json::to_value(&decision).unwrap();
    assert_eq!(value["source"], "stepped_up");
    assert_eq!(
        serde_json::from_value::<AutoDecision>(value).unwrap(),
        decision
    );
    assert_eq!(AutoSource::SteppedDown.to_string(), "stepped_down");
    assert_eq!(AutoSource::Ranked.to_string(), "ranked");
}

// ------------------------------------------------------------ controller

use std::sync::Arc;

use crate::models::auto::{AutoController, AutoStart, AutoTurnStart};
use crate::AutoModelAdvisor;

/// Starts every turn on `start`; knows `test/mid`'s and `test/big`'s
/// efforts.
struct Advisor {
    start: ChildModel,
    ranked: bool,
}

impl AutoModelAdvisor for Advisor {
    fn start(&self, _fallback: &AutoFallback) -> AutoStart {
        AutoStart {
            model: self.start.clone(),
            ranked: self.ranked,
            reason: format!("{} ranks first", self.start.model),
            evidence: None,
        }
    }

    fn efforts(&self, model: &str, _profile_id: &str) -> ModelEfforts {
        match model {
            MID => mid_efforts(),
            BIG => big_efforts(),
            _ => ModelEfforts::default(),
        }
    }
}

fn small_fallback() -> AutoFallback {
    AutoFallback {
        model: SMALL.into(),
        profile_id: "test".into(),
        reasoning_effort: Some("low".into()),
    }
}

/// Auto on, starting on mid @ medium (ranked), with big @ high as the
/// strong tier: a ladder of 4 steps.
fn controller() -> AutoController {
    let mut auto = AutoController::default();
    auto.advisor = Some(Arc::new(Advisor {
        start: model(MID, Some("medium")),
        ranked: true,
    }));
    auto.set_state(Some(AutoModel::new(small_fallback())));
    auto
}

fn big_high(_start: &crate::ModelExecutionSelection) -> Option<ChildModel> {
    Some(model(BIG, Some("high")))
}

fn turn(task_id: u64) -> AutoTurnStart<'static> {
    AutoTurnStart {
        task_id,
        user_message: task_id as usize,
        thinking_enabled: true,
        strong: &big_high,
    }
}

/// Start a turn, accept its decision, and say how it went: the decision.
fn run(auto: &mut AutoController, task_id: u64, end: TurnEnd) -> AutoDecision {
    let started = auto.on_turn_start(turn(task_id)).expect("Auto is on");
    auto.decided(started.decision.clone());
    auto.on_turn_end(end);
    started.decision
}

#[test]
fn with_auto_off_turns_are_left_alone() {
    let mut auto = AutoController::default();
    assert!(auto.on_turn_start(turn(1)).is_none());
    assert_eq!(auto.on_turn_end(TurnEnd::Failed), None);
    assert!(auto.decisions().is_empty());
}

/// A turn starts on the ranked start, the decision says so, and the model
/// to run on is the ladder's first step.
#[test]
fn a_turn_starts_on_the_ranked_start() {
    let mut auto = controller();
    let started = auto.on_turn_start(turn(7)).unwrap();
    assert_eq!(
        (
            started.rung.model.as_str(),
            started.rung.reasoning_effort.as_deref()
        ),
        (MID, Some("medium"))
    );
    let decision = &started.decision;
    assert_eq!(decision.source, AutoSource::Ranked);
    assert_eq!((decision.step, decision.steps), (0, 4));
    assert_eq!((decision.task_id, decision.user_message), (7, 7));
    assert_eq!(decision.reason, "test/mid ranks first");
    // Not a decision until the session has recorded it.
    assert!(auto.decisions().is_empty());
    auto.decided(started.decision.clone());
    assert_eq!(auto.decisions(), [started.decision]);
}

/// Without an advisor the fallback is the start, and the ladder is only as
/// long as what is known of it.
#[test]
fn without_an_advisor_a_turn_starts_on_the_fallback() {
    let mut auto = AutoController::default();
    auto.set_state(Some(AutoModel::new(small_fallback())));
    let started = auto.on_turn_start(turn(1)).unwrap();
    assert_eq!(started.rung.model, SMALL);
    assert_eq!(started.decision.source, AutoSource::Fallback);
    assert_eq!(
        started.decision.reason,
        "no ranking of models is available to this session"
    );
    // Without an advisor no model's efforts are known: the start, then the
    // strong tier.
    assert_eq!(started.decision.steps, 2);
}

/// A failed turn moves the next one up a step; two successes bring it back
/// down. The new state is returned to be journaled; an unchanged one is
/// not.
#[test]
fn failures_step_up_and_successes_step_down() {
    let mut auto = controller();
    let first = auto.on_turn_start(turn(1)).unwrap();
    auto.decided(first.decision);
    let moved = auto.on_turn_end(TurnEnd::Failed).expect("moved up");
    assert_eq!(moved.position, 1);
    assert_eq!(auto.state().unwrap().position, 1);

    let second = run(&mut auto, 2, TurnEnd::Succeeded);
    assert_eq!(second.source, AutoSource::SteppedUp);
    assert_eq!(
        (second.model.as_str(), second.reasoning_effort.as_deref()),
        (MID, Some("high"))
    );
    assert_eq!(
        auto.state().unwrap().position,
        1,
        "one success is not enough"
    );

    let third = run(&mut auto, 3, TurnEnd::Succeeded);
    assert_eq!(third.source, AutoSource::SteppedUp);
    assert_eq!(auto.state().unwrap().position, 0, "two successes step down");

    let fourth = auto.on_turn_start(turn(4)).unwrap();
    assert_eq!(fourth.decision.source, AutoSource::Ranked);
    auto.decided(fourth.decision);
    assert_eq!(auto.on_turn_end(TurnEnd::Neutral), None, "nothing moved");
    assert_eq!(auto.decisions().len(), 4);
}

/// Only a turn Auto started moves the ladder: ending another turn (Auto
/// turned on mid-turn) changes nothing.
#[test]
fn only_a_turn_auto_started_moves_the_ladder() {
    let mut auto = controller();
    assert_eq!(auto.on_turn_end(TurnEnd::Failed), None);
    assert_eq!(auto.state().unwrap().position, 0);

    // And a turn's end counts once.
    let started = auto.on_turn_start(turn(1)).unwrap();
    auto.decided(started.decision);
    assert!(auto.on_turn_end(TurnEnd::Failed).is_some());
    assert_eq!(auto.on_turn_end(TurnEnd::Failed), None);
    assert_eq!(auto.state().unwrap().position, 1);
}

/// What a finished turn says about the model that ran it.
#[test]
fn a_turns_end_is_read_from_how_it_ended() {
    use forge_types::TaskLifecycle::{Cancelled, Completed, Failed, Interrupted};
    assert_eq!(TurnEnd::of(Completed, false, false), TurnEnd::Succeeded);
    assert_eq!(TurnEnd::of(Failed, false, true), TurnEnd::Failed);
    // A provider error or the user's denials: the work did not fail.
    assert_eq!(TurnEnd::of(Failed, false, false), TurnEnd::Neutral);
    assert_eq!(TurnEnd::of(Cancelled, false, false), TurnEnd::Neutral);
    assert_eq!(TurnEnd::of(Interrupted, false, false), TurnEnd::Neutral);
    // A turn on a tier the user asked for says nothing about Auto's choice.
    assert_eq!(TurnEnd::of(Completed, true, false), TurnEnd::Neutral);
    assert_eq!(TurnEnd::of(Failed, true, true), TurnEnd::Neutral);
}

/// Turning Auto off forgets the turn in progress; the decisions stay.
#[test]
fn turning_auto_off_leaves_the_turn_alone() {
    let mut auto = controller();
    let started = auto.on_turn_start(turn(1)).unwrap();
    auto.decided(started.decision);
    auto.set_state(None);
    assert_eq!(auto.on_turn_end(TurnEnd::Failed), None);
    assert!(auto.state().is_none());
    assert_eq!(auto.decisions().len(), 1);
}
