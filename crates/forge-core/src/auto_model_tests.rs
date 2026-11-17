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
