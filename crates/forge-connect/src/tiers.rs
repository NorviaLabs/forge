//! Model tiers derived from models.dev (see `forge_types::ModelTier`).
//!
//! A tier names a provider/model/effort combination without naming a model.
//! Nothing here is configured by hand or by the user. For each provider
//! profile, [`build_profile_tiers`] ranks the models models.dev describes for
//! it into a list per tier, from the metadata Forge caches (price, release
//! date, reasoning support, context window, effort levels).
//! The result is stored (`forge-session`'s tier table) and only rebuilt when
//! its [fingerprint](ProfileTierBuild::fingerprint) changes. At lookup, a tier
//! is the first model in its list that the user's account can run.
//!
//! # The policy
//!
//! 1. **Candidates** are the provider's models that models.dev prices.
//!    Unpriced or free-listed models (cost 0/0) cannot be ranked and are left
//!    out, as are models with a context window below [`MIN_CONTEXT_TOKENS`]
//!    (too small for an agent loop) and premium research tiers billing more
//!    than [`PREMIUM_OUTPUT_CEILING`] per million output tokens (never worth
//!    picking automatically). So are experimental and preview releases,
//!    speech and realtime models, and models.dev `alpha`/`beta` releases.
//! 2. **Current generation first**: within a [model line](model_line) (the
//!    model's name without its version), the newest release is ranked; the
//!    line's older releases follow right behind it as fallbacks for an
//!    account that cannot run the newest. Anything more than
//!    [`STALE_AFTER_DAYS`] older than the provider's newest model is dropped.
//! 3. **Ranking** is by blended price, `(3 × input + output) / 4` per million
//!    tokens — agent turns read far more than they write.
//!    - `strong`: most expensive first, reasoning models before the rest.
//!    - `fast`: cheapest first.
//!    - `default`: the median-priced candidate first (the cheaper of the
//!      two middle ones for an even count), then outward from it.
//!
//!    Ties go to the newer release, then the shorter (alias) id.
//! 4. **Effort** comes from each model's own supported levels
//!    ([`tier_effort`]): `fast` → low, `default` → the model's default,
//!    `strong` → high (never `xhigh`/`max` automatically). Every model gets
//!    an effort per tier, for providers with no ranking at all: there a tier
//!    keeps the session's model and only moves its effort.

use std::collections::BTreeMap;

use forge_types::ModelTier;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::catalog::{ModelCatalogCache, RegistryRow};
use crate::effort::{EffortLevel, EffortSupport};
use crate::profile::ProviderSpec;
use crate::resolve::RouteDefinition;

/// Bumped whenever the ranking rules change, so every stored provider is
/// rebuilt once. (A new Forge version rebuilds too: effort rules live in
/// code.)
pub const TIER_POLICY_VERSION: u32 = 3;

/// Models with a smaller advertised window cannot hold an agent loop.
pub const MIN_CONTEXT_TOKENS: usize = 32_000;
/// A model line this much older than the provider's newest model is a past
/// generation.
pub const STALE_AFTER_DAYS: i64 = 365;
/// USD per million output tokens above which a model is a premium research
/// tier, never chosen automatically.
pub const PREMIUM_OUTPUT_CEILING: f64 = 100.0;

/// The models.dev fields tier derivation reads, per model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TierMetadata {
    /// USD per million input tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cost: Option<f64>,
    /// USD per million output tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_cost: Option<f64>,
    /// `YYYY-MM-DD` (models.dev sometimes publishes only `YYYY-MM`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default)]
    pub reasoning: bool,
    /// models.dev's release status (`alpha`, `beta`, `deprecated`), when
    /// it gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

impl TierMetadata {
    /// Read the tier fields from one models.dev model object.
    pub(crate) fn from_models_dev(model: &serde_json::Value) -> Self {
        let text = |pointer: &str| {
            model
                .pointer(pointer)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        Self {
            input_cost: model
                .pointer("/cost/input")
                .and_then(serde_json::Value::as_f64),
            output_cost: model
                .pointer("/cost/output")
                .and_then(serde_json::Value::as_f64),
            release_date: text("/release_date"),
            family: text("/family"),
            reasoning: model
                .get("reasoning")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            status: text("/status"),
        }
    }

    /// `(3 × input + output) / 4`, or `None` when the model cannot be
    /// ranked by price.
    pub fn blended_cost(&self) -> Option<f64> {
        let input = self
            .input_cost
            .filter(|cost| cost.is_finite() && *cost >= 0.0)?;
        let output = self
            .output_cost
            .filter(|cost| cost.is_finite() && *cost >= 0.0)?;
        let blended = (3.0 * input + output) / 4.0;
        (blended > 0.0).then_some(blended)
    }
}

/// One runnable model with the metadata tiers are derived from.
#[derive(Debug, Clone, PartialEq)]
pub struct TierCandidate {
    /// Forge model id, `<provider>/<model>`.
    pub id: String,
    pub meta: TierMetadata,
    pub context_window: Option<usize>,
}

/// The model each tier resolves to for one provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierLadder {
    pub fast: String,
    pub default: String,
    pub strong: String,
}

impl TierLadder {
    pub fn model(&self, tier: ModelTier) -> &str {
        match tier {
            ModelTier::Fast => &self.fast,
            ModelTier::Default => &self.default,
            ModelTier::Strong => &self.strong,
        }
    }
}

struct Ranked {
    id: String,
    line: String,
    cost: f64,
    day: Option<i64>,
    reasoning: bool,
}

/// Newer first, then the shorter id (an alias over a dated snapshot).
fn recency_order(a: &Ranked, b: &Ranked) -> std::cmp::Ordering {
    b.day
        .cmp(&a.day)
        .then_with(|| a.id.len().cmp(&b.id.len()))
        .then_with(|| a.id.cmp(&b.id))
}

/// Which release of a line leads it: the newest, then (released together)
/// the reasoning variant, then the alias over a dated snapshot.
fn line_order(a: &Ranked, b: &Ranked) -> std::cmp::Ordering {
    b.day
        .cmp(&a.day)
        .then_with(|| b.reasoning.cmp(&a.reasoning))
        .then_with(|| recency_order(a, b))
}

/// Name tokens of models never picked automatically: experimental and
/// preview releases, and speech or realtime models.
const EXCLUDED_NAME_TOKENS: &[&str] = &[
    "exp",
    "experimental",
    "preview",
    "realtime",
    "audio",
    "tts",
    "transcribe",
    "speech",
];

/// models.dev statuses of releases never picked automatically.
const EXCLUDED_STATUSES: &[&str] = &["alpha", "beta", "deprecated"];

fn excluded(candidate: &TierCandidate) -> bool {
    let bare = candidate
        .id
        .rsplit_once('/')
        .map_or(candidate.id.as_str(), |(_, bare)| bare)
        .to_ascii_lowercase();
    bare.split(['-', '_'])
        .any(|token| EXCLUDED_NAME_TOKENS.contains(&token))
        || candidate
            .meta
            .status
            .as_deref()
            .is_some_and(|status| EXCLUDED_STATUSES.contains(&status))
}

/// Every tier's candidates, best first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TierRanking {
    pub fast: Vec<String>,
    pub default: Vec<String>,
    pub strong: Vec<String>,
}

impl TierRanking {
    pub fn get(&self, tier: ModelTier) -> &[String] {
        match tier {
            ModelTier::Fast => &self.fast,
            ModelTier::Default => &self.default,
            ModelTier::Strong => &self.strong,
        }
    }

    /// The first choice for each tier, when there is any candidate.
    pub fn ladder(&self) -> Option<TierLadder> {
        Some(TierLadder {
            fast: self.fast.first()?.clone(),
            default: self.default.first()?.clone(),
            strong: self.strong.first()?.clone(),
        })
    }
}

/// Rank `candidates` into each tier's list. Empty lists when none
/// qualifies. Pure — see the module documentation for the policy.
pub fn rank_tiers(candidates: &[TierCandidate]) -> TierRanking {
    let eligible: Vec<Ranked> = candidates
        .iter()
        .filter(|candidate| !excluded(candidate))
        .filter(|candidate| {
            candidate
                .context_window
                .is_none_or(|window| window >= MIN_CONTEXT_TOKENS)
        })
        .filter(|candidate| {
            candidate
                .meta
                .output_cost
                .is_none_or(|cost| cost <= PREMIUM_OUTPUT_CEILING)
        })
        .filter_map(|candidate| {
            Some(Ranked {
                id: candidate.id.clone(),
                line: model_line(&candidate.id),
                cost: candidate.meta.blended_cost()?,
                day: candidate.meta.release_date.as_deref().and_then(day_number),
                reasoning: candidate.meta.reasoning,
            })
        })
        .collect();
    // Anything more than a year older than the provider's newest model is a
    // past generation.
    let newest = eligible.iter().filter_map(|ranked| ranked.day).max();
    let current = |ranked: &Ranked| match (newest, ranked.day) {
        (Some(newest), Some(day)) => newest - day <= STALE_AFTER_DAYS,
        // An undated model cannot be shown stale.
        _ => true,
    };

    // Each line's releases, leading one first: the leader is what is
    // ranked; the older releases are fallbacks right behind it, for an
    // account that cannot run the leader.
    let mut lines: BTreeMap<String, Vec<Ranked>> = BTreeMap::new();
    for ranked in eligible.into_iter().filter(|ranked| current(ranked)) {
        lines.entry(ranked.line.clone()).or_default().push(ranked);
    }
    for releases in lines.values_mut() {
        releases.sort_by(line_order);
    }
    let mut pool: Vec<&Ranked> = lines.values().map(|releases| &releases[0]).collect();
    if pool.is_empty() {
        return TierRanking::default();
    }

    pool.sort_by(|a, b| a.cost.total_cmp(&b.cost).then_with(|| recency_order(a, b)));
    let fast: Vec<&Ranked> = pool.clone();

    let mut strong: Vec<&Ranked> = pool.clone();
    strong.sort_by(|a, b| {
        b.reasoning
            .cmp(&a.reasoning)
            .then_with(|| b.cost.total_cmp(&a.cost))
            .then_with(|| recency_order(a, b))
    });

    // The median-priced candidate (the cheaper one of an even count), then
    // outward from it: nearer the median first, the pricier one of two
    // equally near.
    let median = (fast.len() - 1) / 2;
    let mut default: Vec<(usize, &Ranked)> = fast.iter().copied().enumerate().collect();
    default.sort_by(|(a_rank, a), (b_rank, b)| {
        a_rank
            .abs_diff(median)
            .cmp(&b_rank.abs_diff(median))
            .then_with(|| b.cost.total_cmp(&a.cost))
            .then_with(|| recency_order(a, b))
    });
    let default: Vec<&Ranked> = default.into_iter().map(|(_, ranked)| ranked).collect();

    // Each leader, then its line's older releases.
    let ids = |leaders: Vec<&Ranked>| {
        leaders
            .into_iter()
            .flat_map(|leader| lines[&leader.line].iter().map(|ranked| ranked.id.clone()))
            .collect()
    };
    TierRanking {
        fast: ids(fast),
        default: ids(default),
        strong: ids(strong),
    }
}

/// The effort each tier would run `model_id` at on `profile`, by the same
/// rules the picker uses: models.dev's levels when it lists the model,
/// otherwise Forge's own per-provider rules. `None` when the model has no
/// adjustable effort. For models the tier table has no row for (a local or
/// newly released model); reads the cache, so call it off the request path.
pub fn model_tier_efforts(
    profile: &ProviderSpec,
    model_id: &str,
    cache: &ModelCatalogCache,
) -> Option<TierEffortRow> {
    let definition = RouteDefinition::from_spec(profile);
    let bare = model_id
        .strip_prefix(&definition.model_provider_prefix)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(model_id);
    let support = crate::effort::effort(&definition, bare, cache);
    let effort = |tier| tier_effort(tier, &support).map(|level| level.as_str().to_string());
    let row = TierEffortRow {
        model_id: model_id.to_string(),
        fast: effort(ModelTier::Fast),
        default: effort(ModelTier::Default),
        strong: effort(ModelTier::Strong),
    };
    (row.fast.is_some() || row.default.is_some() || row.strong.is_some()).then_some(row)
}

/// The first choice per tier; `None` when no candidate qualifies.
pub fn derive_ladder(candidates: &[TierCandidate]) -> Option<TierLadder> {
    rank_tiers(candidates).ladder()
}

/// One ranked model for a tier, with what a session needs to run it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TierRow {
    pub tier: ModelTier,
    /// 0 is the first choice.
    pub rank: u32,
    /// Forge model id, `<provider>/<model>`.
    pub model_id: String,
    /// `None` when the model has no adjustable effort.
    pub reasoning_effort: Option<String>,
    pub context_window: Option<usize>,
    pub output_reserve: Option<usize>,
    pub image_input: bool,
    /// USD per million tokens, `(3 × input + output) / 4`.
    pub blended_cost: f64,
    pub release_date: Option<String>,
}

/// The effort each tier would run one model at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TierEffortRow {
    pub model_id: String,
    pub fast: Option<String>,
    pub default: Option<String>,
    pub strong: Option<String>,
}

impl TierEffortRow {
    pub fn get(&self, tier: ModelTier) -> Option<&str> {
        match tier {
            ModelTier::Fast => self.fast.as_deref(),
            ModelTier::Default => self.default.as_deref(),
            ModelTier::Strong => self.strong.as_deref(),
        }
    }
}

/// Everything stored for one provider profile.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileTierBuild {
    pub profile_id: String,
    /// SHA-256 over everything the rows depend on: the policy and Forge
    /// versions, the parts of the provider definition that affect effort,
    /// and the profile's models.dev rows. Equal fingerprints mean equal rows,
    /// so a stored profile whose fingerprint matches is left alone.
    pub fingerprint: String,
    pub rows: Vec<TierRow>,
    pub efforts: Vec<TierEffortRow>,
}

/// The fingerprint of `profile` over `rows`; see
/// [`ProfileTierBuild::fingerprint`]. Cheap: no ranking.
pub fn profile_fingerprint(profile: &ProviderSpec, rows: &[RegistryRow]) -> String {
    let definition = RouteDefinition::from_spec(profile);
    #[derive(Serialize)]
    struct Inputs<'a> {
        policy: u32,
        forge: &'a str,
        profile_id: &'a str,
        prefix: &'a str,
        models_dev_providers: &'a [String],
        /// Everything `effort::effort` reads from the definition.
        effort_inputs: String,
        rows: Vec<&'a RegistryRow>,
    }
    let mut sorted: Vec<&RegistryRow> = rows.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    let inputs = Inputs {
        policy: TIER_POLICY_VERSION,
        forge: env!("CARGO_PKG_VERSION"),
        profile_id: &profile.id,
        prefix: &definition.model_provider_prefix,
        models_dev_providers: &profile.models_dev_providers,
        effort_inputs: format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}",
            definition.wire,
            definition.wire_overrides,
            definition.oauth_flavor,
            definition.session_header,
            definition.effort_family
        ),
        rows: sorted,
    };
    let json = serde_json::to_vec(&inputs).expect("tier inputs serialize");
    let digest = Sha256::digest(json);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Rank `profile`'s models.dev rows from `cache` into its tier rows.
/// Reads the cache only; no network, no account data.
///
/// See also [`model_tier_efforts`], for a model models.dev does not list.
pub fn build_profile_tiers(profile: &ProviderSpec, cache: &ModelCatalogCache) -> ProfileTierBuild {
    let rows = cache.registry_rows(&profile.id);
    let fingerprint = profile_fingerprint(profile, &rows);
    let efforts: std::collections::HashMap<&str, TierEffortRow> = rows
        .iter()
        .filter_map(|row| {
            Some((
                row.id.as_str(),
                model_tier_efforts(profile, &row.id, cache)?,
            ))
        })
        .collect();
    let effort_for = |tier, id: &str| {
        efforts
            .get(id)
            .and_then(|row| row.get(tier))
            .map(str::to_string)
    };

    let candidates: Vec<TierCandidate> = rows
        .iter()
        .filter_map(|row| {
            Some(TierCandidate {
                id: row.id.clone(),
                meta: row.tier.clone()?,
                context_window: row.limits.map(|limits| limits.context),
            })
        })
        .collect();
    let ranking = rank_tiers(&candidates);
    let by_id: std::collections::HashMap<&str, &RegistryRow> =
        rows.iter().map(|row| (row.id.as_str(), row)).collect();
    let mut tier_rows = Vec::new();
    for tier in ModelTier::ALL {
        for (rank, id) in ranking.get(tier).iter().enumerate() {
            let row = by_id[id.as_str()];
            let meta = row.tier.as_ref();
            tier_rows.push(TierRow {
                tier,
                rank: rank as u32,
                model_id: id.clone(),
                reasoning_effort: effort_for(tier, id),
                context_window: row.limits.map(|limits| limits.context).filter(|c| *c > 0),
                output_reserve: row.limits.map(|limits| limits.output).filter(|o| *o > 0),
                image_input: row.image_input,
                blended_cost: meta.and_then(TierMetadata::blended_cost).unwrap_or(0.0),
                release_date: meta.and_then(|meta| meta.release_date.clone()),
            });
        }
    }
    let mut efforts: Vec<TierEffortRow> = efforts.into_values().collect();
    efforts.sort_by(|a, b| a.model_id.cmp(&b.model_id));
    ProfileTierBuild {
        profile_id: profile.id.clone(),
        fingerprint,
        rows: tier_rows,
        efforts,
    }
}

/// The reasoning effort a tier runs its model at, from the levels that model
/// takes. `None` when the model has no adjustable effort.
pub fn tier_effort(tier: ModelTier, support: &EffortSupport) -> Option<EffortLevel> {
    if !support.supported() {
        return None;
    }
    let options = &support.options;
    let pick = |preferred: EffortLevel| options.contains(&preferred).then_some(preferred);
    Some(match tier {
        ModelTier::Fast => pick(EffortLevel::Low)
            .or_else(|| options.iter().copied().min())
            .unwrap_or(support.default),
        ModelTier::Default => support.default,
        ModelTier::Strong => pick(EffortLevel::High)
            .or_else(|| {
                options
                    .iter()
                    .copied()
                    .filter(|level| *level <= EffortLevel::High)
                    .max()
            })
            .or_else(|| options.iter().copied().min())
            .unwrap_or(support.default),
    })
}

/// The model line an id belongs to: its name without version, snapshot-date
/// and `latest` tokens, so every generation of a line shares it.
/// `gpt-5.4-mini` and `gpt-4o-mini` are `gpt-mini`; `claude-sonnet-4-5` and
/// `claude-sonnet-4-5-20250929` are `claude-sonnet`; `o3` and `o1` are `o`;
/// `deepseek-v4-pro` is `deepseek-pro`; `grok-4.20-0309-reasoning` is `grok`.
/// Unlike models.dev's `family`, which
/// can put a provider's whole flagship history in one group, this keeps each
/// line's newest generation and drops only the generations it replaced.
pub fn model_line(id: &str) -> String {
    // `-latest` names an alias; `-reasoning` / `-non-reasoning` name two
    // modes of one model.
    const NON_LINE_TOKENS: &[&str] = &["latest", "reasoning", "non"];
    let bare = id.rsplit_once('/').map_or(id, |(_, bare)| bare);
    let tokens: Vec<String> = bare
        .to_ascii_lowercase()
        .split(['-', '_'])
        .filter(|token| !token.is_empty() && !NON_LINE_TOKENS.contains(token))
        .filter_map(|token| {
            // A version or date (`5.4`, `4o`, `20250929`) names a generation.
            if token.starts_with(|c: char| c.is_ascii_digit()) {
                return None;
            }
            // A name then a version (`o3`, `qwen3`, `k2`, `v4`): keep the
            // name, drop the version; a bare `v4` is all version.
            let name = token.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
            if name.len() < token.len() {
                return (name != "v").then(|| name.to_string());
            }
            Some(token.to_string())
        })
        .collect();
    if tokens.is_empty() {
        bare.to_ascii_lowercase()
    } else {
        tokens.join("-")
    }
}

/// Days since the civil epoch for `YYYY-MM-DD` or `YYYY-MM` (as the 1st).
fn day_number(date: &str) -> Option<i64> {
    let mut parts = date.trim().splitn(3, '-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = match parts.next() {
        Some(day) => day.get(..2).unwrap_or(day).parse().ok()?,
        None => 1,
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, input: f64, output: f64, date: &str, family: &str) -> TierCandidate {
        TierCandidate {
            id: id.into(),
            meta: TierMetadata {
                input_cost: Some(input),
                output_cost: Some(output),
                release_date: Some(date.into()),
                family: Some(family.into()),
                reasoning: true,
                status: None,
            },
            context_window: Some(200_000),
        }
    }

    /// The shape of models.dev's Anthropic rows: three families, each with
    /// older generations still listed.
    fn anthropic() -> Vec<TierCandidate> {
        vec![
            model(
                "anthropic/claude-3-5-haiku-latest",
                0.8,
                4.0,
                "2024-10-22",
                "claude-haiku",
            ),
            model(
                "anthropic/claude-haiku-4-5",
                1.0,
                5.0,
                "2025-10-15",
                "claude-haiku",
            ),
            model(
                "anthropic/claude-sonnet-4-0",
                3.0,
                15.0,
                "2025-05-22",
                "claude-sonnet",
            ),
            model(
                "anthropic/claude-sonnet-4-5",
                3.0,
                15.0,
                "2025-09-29",
                "claude-sonnet",
            ),
            model(
                "anthropic/claude-sonnet-4-5-20250929",
                3.0,
                15.0,
                "2025-09-29",
                "claude-sonnet",
            ),
            model(
                "anthropic/claude-opus-4-1",
                15.0,
                75.0,
                "2025-08-05",
                "claude-opus",
            ),
        ]
    }

    #[test]
    fn a_three_family_provider_maps_cheap_middle_and_flagship() {
        let ladder = derive_ladder(&anthropic()).unwrap();
        assert_eq!(ladder.fast, "anthropic/claude-haiku-4-5");
        assert_eq!(ladder.default, "anthropic/claude-sonnet-4-5");
        assert_eq!(ladder.strong, "anthropic/claude-opus-4-1");
    }

    #[test]
    fn older_generations_and_stale_families_are_not_candidates() {
        let mut models = vec![
            model("openai/gpt-4o", 2.5, 10.0, "2024-05-13", "gpt-4o"),
            model("openai/gpt-4o-mini", 0.15, 0.6, "2024-07-18", "gpt-4o-mini"),
            model("openai/gpt-5", 1.25, 10.0, "2025-08-07", "gpt-5"),
            model("openai/gpt-5-mini", 0.25, 2.0, "2025-08-07", "gpt-5-mini"),
            model("openai/gpt-5-nano", 0.05, 0.4, "2025-08-07", "gpt-5-nano"),
        ];
        // A premium research tier is never chosen automatically.
        models.push(model(
            "openai/gpt-5-pro",
            15.0,
            120.0,
            "2025-10-06",
            "gpt-5-pro",
        ));
        let ladder = derive_ladder(&models).unwrap();
        assert_eq!(ladder.strong, "openai/gpt-5");
        assert_eq!(ladder.fast, "openai/gpt-5-nano");
        assert_eq!(ladder.default, "openai/gpt-5-mini");
        // The gpt-4o families are more than a year older than the newest
        // model: a past generation, whatever their price.
        assert!(!ladder_mentions(&ladder, "gpt-4o"));
    }

    fn ladder_mentions(ladder: &TierLadder, needle: &str) -> bool {
        [&ladder.fast, &ladder.default, &ladder.strong]
            .iter()
            .any(|id| id.contains(needle))
    }

    #[test]
    fn strong_prefers_a_reasoning_model_over_a_pricier_plain_one() {
        let mut plain = model("x/plain-big", 10.0, 40.0, "2025-06-01", "plain");
        plain.meta.reasoning = false;
        let thinker = model("x/thinker", 3.0, 15.0, "2025-06-01", "thinker");
        let small = model("x/small", 0.2, 0.8, "2025-06-01", "small");
        let ladder = derive_ladder(&[plain, thinker, small]).unwrap();
        assert_eq!(ladder.strong, "x/thinker");
        assert_eq!(ladder.fast, "x/small");
    }

    #[test]
    fn unpriced_tiny_or_missing_models_leave_no_ladder() {
        assert_eq!(derive_ladder(&[]), None);
        let mut free = model("ollama/llama", 0.0, 0.0, "2025-01-01", "llama");
        free.meta.reasoning = false;
        let mut tiny = model("x/tiny", 0.1, 0.1, "2025-01-01", "tiny");
        tiny.context_window = Some(8_192);
        let unpriced = TierCandidate {
            id: "x/unknown".into(),
            meta: TierMetadata::default(),
            context_window: None,
        };
        assert_eq!(derive_ladder(&[free, tiny, unpriced]), None);
    }

    #[test]
    fn a_single_candidate_fills_every_tier() {
        let only = model("x/only", 1.0, 4.0, "2025-03-01", "only");
        let ladder = derive_ladder(&[only]).unwrap();
        assert_eq!(ladder.fast, "x/only");
        assert_eq!(ladder.default, "x/only");
        assert_eq!(ladder.strong, "x/only");
    }

    #[test]
    fn ties_go_to_the_newer_release_then_the_alias() {
        let old = model("x/a-old", 1.0, 4.0, "2025-01-01", "a-old");
        let new = model("x/b-new", 1.0, 4.0, "2025-02-01", "b-new");
        let ladder = derive_ladder(&[old, new]).unwrap();
        assert_eq!(ladder.fast, "x/b-new");
        assert_eq!(ladder.strong, "x/b-new");
        // Same family and date: the alias, not the dated snapshot.
        let ladder = derive_ladder(&anthropic()).unwrap();
        assert_eq!(ladder.default, "anthropic/claude-sonnet-4-5");
    }

    #[test]
    fn every_candidate_is_ranked_for_every_tier_best_first() {
        let ranking = rank_tiers(&anthropic());
        // Each line's newest release, then its older ones behind it.
        assert_eq!(
            ranking.fast,
            [
                "anthropic/claude-haiku-4-5",
                "anthropic/claude-3-5-haiku-latest",
                "anthropic/claude-sonnet-4-5",
                "anthropic/claude-sonnet-4-5-20250929",
                "anthropic/claude-sonnet-4-0",
                "anthropic/claude-opus-4-1",
            ]
        );
        assert_eq!(
            ranking.strong,
            [
                "anthropic/claude-opus-4-1",
                "anthropic/claude-sonnet-4-5",
                "anthropic/claude-sonnet-4-5-20250929",
                "anthropic/claude-sonnet-4-0",
                "anthropic/claude-haiku-4-5",
                "anthropic/claude-3-5-haiku-latest",
            ]
        );
        assert_eq!(ranking.default[0], "anthropic/claude-sonnet-4-5");
        assert_eq!(ranking.default.len(), 6);
        assert_eq!(rank_tiers(&[]), TierRanking::default());
        assert_eq!(rank_tiers(&[]).ladder(), None);
    }

    /// A catalog file as a models.dev refresh leaves it, for two providers.
    fn write_catalog(dir: &std::path::Path, opus_output_cost: f64) -> ModelCatalogCache {
        let path = dir.join("catalog.toml");
        let mut text = String::from(
            "version = 2\n\
             registry_image_input = [\"anthropic/claude-opus-4-8\"]\n\
             registry_image_input_ready = true\n\
             registry_effort_ready = true\n\
             registry_tiers_ready = true\n\n\
             [registry_models]\n\
             anthropic = [\"anthropic/claude-haiku-4-5\", \"anthropic/claude-sonnet-4-6\", \"anthropic/claude-opus-4-8\"]\n\
             openai = [\"openai/gpt-5\", \"openai/gpt-5-mini\"]\n\n\
             [registry_effort]\n\
             \"anthropic/claude-haiku-4-5\" = []\n\
             \"openai/gpt-5\" = [\"minimal\", \"low\", \"medium\", \"high\"]\n\
             \"openai/gpt-5-mini\" = [\"low\", \"medium\", \"high\"]\n\n\
             [registry_limits.\"anthropic/claude-opus-4-8\"]\n\
             context = 1000000\n\
             output = 64000\n",
        );
        for (id, input, output, date, family) in [
            (
                "anthropic/claude-haiku-4-5",
                1.0,
                5.0,
                "2025-10-15",
                "claude-haiku",
            ),
            (
                "anthropic/claude-sonnet-4-6",
                3.0,
                15.0,
                "2026-02-01",
                "claude-sonnet",
            ),
            (
                "anthropic/claude-opus-4-8",
                5.0,
                opus_output_cost,
                "2026-05-01",
                "claude-opus",
            ),
            ("openai/gpt-5", 1.25, 10.0, "2025-08-07", "gpt-5"),
            ("openai/gpt-5-mini", 0.25, 2.0, "2025-08-07", "gpt-5-mini"),
        ] {
            text.push_str(&format!(
                "\n[registry_tiers.\"{id}\"]\ninput_cost = {input:?}\noutput_cost = {output:?}\n\
                 release_date = \"{date}\"\nfamily = \"{family}\"\nreasoning = true\n"
            ));
        }
        std::fs::write(&path, text).unwrap();
        ModelCatalogCache::new(path)
    }

    #[test]
    fn a_profile_build_ranks_its_models_with_effort_and_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let cache = write_catalog(dir.path(), 25.0);
        let build = build_profile_tiers(&crate::builtin::builtin_profile("anthropic"), &cache);
        assert_eq!(build.profile_id, "anthropic");
        assert_eq!(build.rows.len(), 9, "three models ranked for each tier");
        let first = |tier| {
            build
                .rows
                .iter()
                .find(|row| row.tier == tier && row.rank == 0)
                .unwrap()
        };
        let strong = first(ModelTier::Strong);
        assert_eq!(strong.model_id, "anthropic/claude-opus-4-8");
        assert_eq!(strong.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(strong.context_window, Some(1_000_000));
        assert_eq!(strong.output_reserve, Some(64_000));
        assert!(strong.image_input);
        assert_eq!(strong.blended_cost, 10.0);
        let fast = first(ModelTier::Fast);
        assert_eq!(fast.model_id, "anthropic/claude-haiku-4-5");
        assert_eq!(
            fast.reasoning_effort, None,
            "models.dev lists no effort for it"
        );
        assert_eq!(
            first(ModelTier::Default).model_id,
            "anthropic/claude-sonnet-4-6"
        );

        // Effort rows for the models that take effort, for providers or
        // sessions with no ranking to use.
        let sonnet = build
            .efforts
            .iter()
            .find(|row| row.model_id == "anthropic/claude-sonnet-4-6")
            .unwrap();
        assert_eq!(sonnet.get(ModelTier::Fast), Some("low"));
        assert_eq!(sonnet.get(ModelTier::Strong), Some("high"));
        assert!(build
            .efforts
            .iter()
            .all(|row| row.model_id != "anthropic/claude-haiku-4-5"));

        // A provider models.dev says nothing about has nothing to rank.
        let ollama = build_profile_tiers(&crate::builtin::builtin_profile("ollama"), &cache);
        assert!(ollama.rows.is_empty() && ollama.efforts.is_empty());
    }

    #[test]
    fn the_fingerprint_moves_only_for_the_provider_whose_inputs_changed() {
        let dir = tempfile::tempdir().unwrap();
        let anthropic_spec = crate::builtin::builtin_profile("anthropic");
        let openai_spec = crate::builtin::builtin_profile("openai");
        let before = write_catalog(dir.path(), 25.0);
        let anthropic = build_profile_tiers(&anthropic_spec, &before);
        let openai = build_profile_tiers(&openai_spec, &before);
        assert_eq!(
            anthropic.fingerprint,
            build_profile_tiers(&anthropic_spec, &before).fingerprint,
            "stable for the same inputs"
        );

        // models.dev changes one Anthropic price.
        let after = write_catalog(dir.path(), 30.0);
        assert_ne!(
            build_profile_tiers(&anthropic_spec, &after).fingerprint,
            anthropic.fingerprint
        );
        assert_eq!(
            build_profile_tiers(&openai_spec, &after).fingerprint,
            openai.fingerprint,
            "another provider's rows did not change"
        );

        // The provider definition changes what effort its models take.
        let mut custom = openai_spec.clone();
        custom.effort_family = crate::profile::EffortFamily::None;
        assert_ne!(
            build_profile_tiers(&custom, &after).fingerprint,
            openai.fingerprint
        );
    }

    #[test]
    fn model_lines_drop_versions_dates_and_aliases() {
        for (id, line) in [
            ("openai/gpt-5.4-mini", "gpt-mini"),
            ("openai/gpt-4o-mini", "gpt-mini"),
            ("openai/gpt-5.5", "gpt"),
            ("openai/gpt-4o-2024-08-06", "gpt"),
            ("openai/gpt-5.5-pro", "gpt-pro"),
            ("openai/gpt-daybreak-red-latest", "gpt-daybreak-red"),
            ("openai/gpt-6-luna", "gpt-luna"),
            ("openai/o3", "o"),
            ("openai/o4-mini", "o-mini"),
            ("anthropic/claude-sonnet-4-5-20250929", "claude-sonnet"),
            ("anthropic/claude-3-5-haiku-latest", "claude-haiku"),
            ("xai/grok-4.20-0309-reasoning", "grok"),
            ("xai/grok-4.20-0309-non-reasoning", "grok"),
            ("xai/grok-build-0.1", "grok-build"),
            ("opencode_go/deepseek-v4-pro", "deepseek-pro"),
            ("opencode_go/kimi-k3", "kimi-k"),
            ("opencode_zen/qwen3-coder", "qwen-coder"),
            ("x/42", "42"),
        ] {
            assert_eq!(model_line(id), line, "{id}");
        }
    }

    /// models.dev (2026-09-30) puts a new, pricier Claude family above
    /// Opus. The price midpoint then lands on Opus; the median stays on
    /// Sonnet.
    #[test]
    fn default_is_the_median_priced_candidate() {
        let candidates = vec![
            model(
                "anthropic/claude-haiku-4-5",
                1.0,
                5.0,
                "2025-10-15",
                "claude-haiku",
            ),
            model(
                "anthropic/claude-sonnet-5-5",
                2.0,
                10.0,
                "2026-09-28",
                "claude-sonnet",
            ),
            model(
                "anthropic/claude-opus-5-5",
                4.0,
                20.0,
                "2026-09-22",
                "claude-opus",
            ),
            model(
                "anthropic/claude-fable-5-1",
                10.0,
                50.0,
                "2026-09-01",
                "claude-fable",
            ),
        ];
        let ranking = rank_tiers(&candidates);
        assert_eq!(
            ranking.default,
            [
                "anthropic/claude-sonnet-5-5",
                // One step away each side: the pricier first.
                "anthropic/claude-opus-5-5",
                "anthropic/claude-haiku-4-5",
                "anthropic/claude-fable-5-1",
            ]
        );
        assert_eq!(ranking.fast[0], "anthropic/claude-haiku-4-5");
        assert_eq!(ranking.strong[0], "anthropic/claude-fable-5-1");
    }

    /// models.dev (2026-09-30) files gpt-4o through gpt-5.6 and
    /// `gpt-daybreak-red-latest` under one `gpt` family. Grouped by family,
    /// only the newest of them survived and gpt-5.6 was dropped; grouped by
    /// model line, gpt-5.6 replaces only the older `gpt` generations.
    #[test]
    fn generations_are_grouped_by_model_line_not_by_family() {
        let candidates = vec![
            model("openai/gpt-4o", 2.5, 10.0, "2024-05-13", "gpt"),
            model("openai/gpt-5", 1.25, 10.0, "2025-08-07", "gpt"),
            model("openai/gpt-5.5", 5.0, 30.0, "2026-04-23", "gpt"),
            model("openai/gpt-5.6", 4.0, 20.0, "2026-07-09", "gpt-sol"),
            model(
                "openai/gpt-daybreak-red-latest",
                12.5,
                75.0,
                "2026-08-07",
                "gpt",
            ),
            model("openai/gpt-5.4-mini", 0.75, 4.5, "2026-03-17", "gpt-mini"),
            model("openai/gpt-6-luna", 0.1, 0.5, "2026-09-22", "gpt-luna"),
        ];
        let ranking = rank_tiers(&candidates);
        // One leader per line; gpt-5.5 follows gpt-5.6 as its line's
        // fallback; gpt-5 and gpt-4o are more than a year old.
        assert_eq!(
            ranking.fast,
            [
                "openai/gpt-6-luna",
                "openai/gpt-5.4-mini",
                "openai/gpt-5.6",
                "openai/gpt-5.5",
                "openai/gpt-daybreak-red-latest",
            ]
        );
        assert_eq!(ranking.strong[0], "openai/gpt-daybreak-red-latest");
        // Second choice for an account without the first.
        assert_eq!(ranking.strong[1], "openai/gpt-5.6");
        assert_eq!(ranking.fast[0], "openai/gpt-6-luna");
        assert_eq!(ranking.default[0], "openai/gpt-5.4-mini");
    }

    /// An account without a line's newest release falls back to that line's
    /// older release before any other line.
    #[test]
    fn a_lines_older_releases_follow_its_newest() {
        let candidates = vec![
            model(
                "anthropic/claude-sonnet-5-5",
                2.0,
                10.0,
                "2026-09-28",
                "claude-sonnet",
            ),
            model(
                "anthropic/claude-opus-5-5",
                4.0,
                20.0,
                "2026-09-22",
                "claude-opus",
            ),
            model(
                "anthropic/claude-opus-5",
                5.0,
                25.0,
                "2026-07-24",
                "claude-opus",
            ),
            model(
                "anthropic/claude-opus-4-8",
                5.0,
                25.0,
                "2026-05-28",
                "claude-opus",
            ),
        ];
        let ranking = rank_tiers(&candidates);
        assert_eq!(
            ranking.strong,
            [
                "anthropic/claude-opus-5-5",
                "anthropic/claude-opus-5",
                "anthropic/claude-opus-4-8",
                "anthropic/claude-sonnet-5-5",
            ],
            "the older Opus releases, though pricier, rank with their line"
        );
    }

    #[test]
    fn experimental_preview_speech_and_beta_models_are_never_candidates() {
        let mut beta = model("x/solid-2", 3.0, 12.0, "2026-09-01", "solid");
        beta.meta.status = Some("beta".into());
        let candidates = vec![
            model(
                "opencode/deepseek-v4-flash-vision-exp",
                0.1,
                0.2,
                "2026-09-01",
                "ds",
            ),
            model(
                "opencode/longcat-2.5-preview-free",
                0.1,
                0.3,
                "2026-09-01",
                "lc",
            ),
            model("openai/gpt-realtime-2.1", 4.0, 24.0, "2026-07-06", "gpt"),
            model("openai/gpt-audio-mini", 0.6, 2.4, "2026-07-06", "gpt"),
            beta,
            model("x/steady-1", 1.0, 4.0, "2026-08-01", "steady"),
        ];
        let ranking = rank_tiers(&candidates);
        assert_eq!(ranking.fast, ["x/steady-1"]);
        assert_eq!(ranking.strong, ["x/steady-1"]);
    }

    /// Released together, the reasoning variant leads its line.
    #[test]
    fn a_reasoning_variant_leads_its_non_reasoning_twin() {
        let mut plain = model(
            "xai/grok-4.20-0309-non-reasoning",
            1.25,
            2.5,
            "2026-03-09",
            "grok",
        );
        plain.meta.reasoning = false;
        let thinking = model(
            "xai/grok-4.20-0309-reasoning",
            1.25,
            2.5,
            "2026-03-09",
            "grok",
        );
        let newer = model("xai/grok-4.7", 2.0, 6.0, "2026-09-21", "grok");
        let ranking = rank_tiers(&[plain, thinking, newer]);
        assert_eq!(
            ranking.fast,
            [
                "xai/grok-4.7",
                "xai/grok-4.20-0309-reasoning",
                "xai/grok-4.20-0309-non-reasoning"
            ],
            "one line: the newest leads, the older modes follow"
        );
    }

    #[test]
    fn models_dev_status_is_read() {
        let meta = TierMetadata::from_models_dev(&serde_json::json!({"status": "beta"}));
        assert_eq!(meta.status.as_deref(), Some("beta"));
    }

    /// A model models.dev does not list still gets tier efforts from
    /// Forge's own rules; one with no adjustable effort gets none.
    #[test]
    fn unlisted_models_take_tier_efforts_from_forges_rules() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCatalogCache::new(dir.path().join("empty.toml"));
        let xai = crate::builtin::builtin_profile("xai");
        let row = model_tier_efforts(&xai, "xai/grok-4.6-fast", &cache).unwrap();
        assert_eq!(row.get(ModelTier::Fast), Some("low"));
        assert_eq!(row.get(ModelTier::Default), Some("medium"));
        assert_eq!(row.get(ModelTier::Strong), Some("high"));
        let ollama = crate::builtin::builtin_profile("ollama");
        assert_eq!(model_tier_efforts(&ollama, "ollama/llama3.3", &cache), None);
    }

    #[test]
    fn effort_follows_the_tier_within_what_the_model_takes() {
        use EffortLevel::*;
        let full = EffortSupport {
            options: vec![Minimal, Low, Medium, High, XHigh, Max],
            default: Medium,
        };
        assert_eq!(tier_effort(ModelTier::Fast, &full), Some(Low));
        assert_eq!(tier_effort(ModelTier::Default, &full), Some(Medium));
        assert_eq!(tier_effort(ModelTier::Strong, &full), Some(High));

        let narrow = EffortSupport {
            options: vec![Medium, XHigh],
            default: Medium,
        };
        assert_eq!(tier_effort(ModelTier::Fast, &narrow), Some(Medium));
        assert_eq!(tier_effort(ModelTier::Strong, &narrow), Some(Medium));

        let none = EffortSupport {
            options: Vec::new(),
            default: Medium,
        };
        for tier in ModelTier::ALL {
            assert_eq!(tier_effort(tier, &none), None);
        }
    }

    #[test]
    fn models_dev_rows_read_into_tier_metadata() {
        let meta = TierMetadata::from_models_dev(&serde_json::json!({
            "id": "claude-opus-4-1",
            "family": "claude-opus",
            "reasoning": true,
            "release_date": "2025-08-05",
            "cost": {"input": 15, "output": 75, "cache_read": 1.5}
        }));
        assert_eq!(meta.input_cost, Some(15.0));
        assert_eq!(meta.output_cost, Some(75.0));
        assert_eq!(meta.family.as_deref(), Some("claude-opus"));
        assert!(meta.reasoning);
        assert_eq!(meta.blended_cost(), Some(30.0));
        assert_eq!(
            TierMetadata::from_models_dev(&serde_json::json!({})),
            TierMetadata::default()
        );
    }

    #[test]
    fn release_dates_parse_to_day_numbers() {
        assert_eq!(day_number("1970-01-01"), Some(0));
        assert_eq!(
            day_number("2025-08-07").unwrap() - day_number("2024-08-07").unwrap(),
            365
        );
        assert_eq!(day_number("2025-08"), day_number("2025-08-01"));
        assert_eq!(day_number("2025-13-01"), None);
        assert_eq!(day_number("soon"), None);
    }
}
