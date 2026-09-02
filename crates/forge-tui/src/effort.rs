//! Provider-neutral effort selected for the current TUI session.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;
use std::{fmt, str::FromStr};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReasoningEffort {
    #[default]
    Auto,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl ReasoningEffort {
    pub const USAGE: &'static str = "auto|minimal|low|medium|high|xhigh|max";

    pub fn default_for_model(model: &str) -> Self {
        // Temporarily hardcoded to a real effort level for every provider
        // (was `Auto` for openai-codex/openai/fallback) so reasoning content
        // — and therefore the conversation pane's Thinking block — actually
        // gets exercised without the user having to configure it by hand.
        // This costs real reasoning tokens and latency on every new session
        // by default; revisit before shipping.
        let options = Self::options_for_model(model);
        let family = family_default(model);
        if options.contains(&family) {
            family
        } else {
            preferred_default(&options).unwrap_or(family)
        }
    }

    /// Display label for the picker UI. Differs from `Display`/the wire
    /// value only for `XHigh`, which reads "Extra High" on screen while the
    /// internal value and transport string stay `xhigh`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Minimal => "Minimal",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::XHigh => "Extra High",
            Self::Max => "Max",
        }
    }

    /// Display label for chrome that always shows *some* effort text (the
    /// footer control, the Effort column header) — the effort's own label
    /// when the model actually supports adjustable effort, or an explicit
    /// "N/A" otherwise. Never returns a level word for a model that can't
    /// use one.
    pub fn display_label(self, model: &str) -> &'static str {
        if Self::model_supports_effort(model) {
            self.label()
        } else {
            "N/A"
        }
    }

    /// Empty means providers should choose their own default.
    pub fn transport_value(self) -> &'static str {
        match self {
            Self::Auto => "",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// Effort levels the selected model can actually consume.
    /// Prefer models.dev catalog metadata when the cache has a row; fall
    /// back to the last-known built-in family lists only when that row is
    /// missing (offline / first run).
    pub fn options_for_model(model: &str) -> Vec<Self> {
        match catalog_effort_options(model) {
            Some(options) if options.is_empty() => vec![family_default(model)],
            Some(options) => options,
            None => fallback_options_for_model(model),
        }
    }

    /// Step one level forward/back within this model's valid options
    /// (`Alt+,`/`Alt+.`). Clamps at either end rather than wrapping —
    /// reaching an option not currently selected still requires the full
    /// picker (`F4`), this only moves within `options_for_model`.
    pub fn step(self, model: &str, forward: bool) -> Self {
        let options = Self::options_for_model(model);
        let Some(idx) = options.iter().position(|&o| o == self) else {
            return self;
        };
        let next = if forward {
            idx.saturating_add(1).min(options.len() - 1)
        } else {
            idx.saturating_sub(1)
        };
        options[next]
    }

    pub fn model_supports_effort(model: &str) -> bool {
        match catalog_effort_options(model) {
            Some(options) => options.len() > 1,
            None => fallback_model_supports_effort(model),
        }
    }
}

struct EffortCatalogMemo {
    home: Option<String>,
    xdg_config: Option<String>,
    path: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
    by_model: HashMap<String, Option<Vec<ReasoningEffort>>>,
}

fn effort_catalog_memo() -> &'static Mutex<EffortCatalogMemo> {
    static MEMO: OnceLock<Mutex<EffortCatalogMemo>> = OnceLock::new();
    MEMO.get_or_init(|| {
        Mutex::new(EffortCatalogMemo {
            home: None,
            xdg_config: None,
            path: PathBuf::new(),
            modified: None,
            len: 0,
            by_model: HashMap::new(),
        })
    })
}

fn catalog_file_fingerprint(path: &std::path::Path) -> (Option<SystemTime>, u64) {
    match std::fs::metadata(path) {
        Ok(meta) => (meta.modified().ok(), meta.len()),
        Err(_) => (None, 0),
    }
}

fn parse_catalog_effort_values(values: Vec<String>) -> Vec<ReasoningEffort> {
    values
        .into_iter()
        .filter_map(|value| value.parse::<ReasoningEffort>().ok())
        .filter(|effort| *effort != ReasoningEffort::Auto)
        .collect()
}

/// Catalog lookups sit on the footer render path (`display_label` every
/// frame). Re-reading and parsing `model-catalog.toml` there blows the
/// allocation budget, so memoize by process env + file fingerprint.
fn catalog_effort_options(model: &str) -> Option<Vec<ReasoningEffort>> {
    let home = std::env::var("HOME").ok();
    let xdg_config = std::env::var("XDG_CONFIG_HOME").ok();
    let mut memo = effort_catalog_memo()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let env_changed = memo.home != home || memo.xdg_config != xdg_config;
    if !env_changed {
        let (modified, len) = catalog_file_fingerprint(&memo.path);
        if modified == memo.modified && len == memo.len {
            if let Some(hit) = memo.by_model.get(model) {
                return hit.clone();
            }
        } else {
            memo.modified = modified;
            memo.len = len;
            memo.by_model.clear();
        }
    }

    let cache = forge_connect::ModelCatalogCache::user_default();
    if env_changed || memo.path != cache.path() {
        let (modified, len) = catalog_file_fingerprint(cache.path());
        memo.home = home;
        memo.xdg_config = xdg_config;
        memo.path = cache.path().to_path_buf();
        memo.modified = modified;
        memo.len = len;
        memo.by_model.clear();
    }

    let value = cache
        .model_effort_options(model)
        .map(parse_catalog_effort_values);
    memo.by_model.insert(model.to_string(), value.clone());
    value
}

fn preferred_default(options: &[ReasoningEffort]) -> Option<ReasoningEffort> {
    for candidate in [
        ReasoningEffort::Medium,
        ReasoningEffort::Low,
        ReasoningEffort::High,
    ] {
        if options.contains(&candidate) {
            return Some(candidate);
        }
    }
    options.first().copied()
}

fn family_default(model: &str) -> ReasoningEffort {
    if model.to_ascii_lowercase().starts_with("anthropic/") {
        ReasoningEffort::Low
    } else {
        ReasoningEffort::Medium
    }
}

fn fallback_options_for_model(model: &str) -> Vec<ReasoningEffort> {
    use ReasoningEffort::*;
    if !fallback_model_supports_effort(model) {
        return vec![family_default(model)];
    }
    let model = model.to_ascii_lowercase();
    if model.starts_with("openai-codex/") {
        return vec![Minimal, Low, Medium, High, XHigh, Max];
    }
    if model.starts_with("anthropic/") {
        let model_id = model.trim_start_matches("anthropic/");
        let mut options = vec![Low, Medium, High];
        if !(model_id.contains("4-6") || model_id.contains("opus-4-5")) {
            options.push(XHigh);
        }
        return options;
    }
    vec![Minimal, Low, Medium, High, XHigh]
}

fn fallback_model_supports_effort(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    let model_id = model
        .split_once('/')
        .map(|(_, id)| id)
        .unwrap_or(model.as_str());

    if model.starts_with("openai-codex/") || model.starts_with("opencode-") {
        return true;
    }
    if model.starts_with("openai/")
        && ["gpt-5", "o1", "o3", "o4"]
            .iter()
            .any(|prefix| model_id.starts_with(prefix))
    {
        return true;
    }
    if (model.starts_with("xai/") || model.starts_with("grok/"))
        && ["grok-4.3", "grok-4.5", "grok-4.20", "grok-4.6"]
            .iter()
            .any(|marker| model_id.contains(marker))
    {
        return true;
    }
    if model.starts_with("anthropic/")
        && [
            "sonnet-5",
            "opus-4-8",
            "opus-4-7",
            "opus-4-6",
            "sonnet-4-6",
            "opus-4-5",
        ]
        .iter()
        .any(|marker| model_id.contains(marker))
    {
        return true;
    }
    false
}
impl fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        })
    }
}

impl FromStr for ReasoningEffort {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" | "default" => Ok(Self::Auto),
            "minimal" | "min" => Ok(Self::Minimal),
            "low" | "light" => Ok(Self::Low),
            "medium" | "med" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" | "extra-high" | "extra_high" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::helpers::isolated_home_guard;

    fn isolated_catalog_home() -> (
        tempfile::TempDir,
        crate::app::tests::helpers::ScopedEnvGuard,
    ) {
        isolated_home_guard()
    }

    #[test]
    fn isolated_catalog_home_restores_home_on_drop() {
        let isolated;
        {
            let home = isolated_catalog_home();
            isolated = home.0.path().to_path_buf();
            assert_eq!(std::env::var("HOME").ok().as_deref(), isolated.to_str());
        }
        assert_ne!(std::env::var("HOME").ok().as_deref(), isolated.to_str());
    }

    fn write_user_catalog_effort(model: &str, values: &[&str]) {
        let cache = forge_connect::ModelCatalogCache::user_default();
        if let Some(parent) = cache.path().parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let listed = values
            .iter()
            .map(|value| format!("\"{value}\""))
            .collect::<Vec<_>>()
            .join(", ");
        std::fs::write(
            cache.path(),
            format!("registry_effort_ready = true\n[registry_effort]\n\"{model}\" = [{listed}]\n"),
        )
        .unwrap();
    }

    #[test]
    fn catalog_effort_memo_invalidates_when_the_cache_file_changes() {
        let _home = isolated_catalog_home();
        let model = "xai/grok-4.6";
        write_user_catalog_effort(model, &["low"]);
        assert_eq!(
            ReasoningEffort::options_for_model(model),
            vec![ReasoningEffort::Low]
        );

        write_user_catalog_effort(model, &["low", "medium", "high", "xhigh"]);
        assert_eq!(
            ReasoningEffort::options_for_model(model),
            vec![
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::XHigh
            ]
        );
    }

    #[test]
    fn aliases_parse_to_canonical_values() {
        assert_eq!("light".parse(), Ok(ReasoningEffort::Low));
        assert_eq!("extra-high".parse(), Ok(ReasoningEffort::XHigh));
        assert_eq!(ReasoningEffort::XHigh.to_string(), "xhigh");
        assert_eq!(ReasoningEffort::Auto.transport_value(), "");
    }

    #[test]
    fn display_label_is_na_for_a_model_without_adjustable_effort() {
        let _home = isolated_catalog_home();
        assert!(!ReasoningEffort::model_supports_effort("mock"));
        assert_eq!(ReasoningEffort::Low.display_label("mock"), "N/A");
        assert_eq!(ReasoningEffort::Auto.display_label("mock"), "N/A");
    }

    #[test]
    fn display_label_matches_label_for_a_model_that_supports_effort() {
        let _home = isolated_catalog_home();
        let model = "anthropic/claude-sonnet-4-6";
        assert!(ReasoningEffort::model_supports_effort(model));
        assert_eq!(
            ReasoningEffort::High.display_label(model),
            ReasoningEffort::High.label()
        );
    }

    #[test]
    fn xhigh_label_reads_extra_high_while_wire_value_is_unchanged() {
        assert_eq!(ReasoningEffort::XHigh.label(), "Extra High");
        assert_eq!(ReasoningEffort::XHigh.to_string(), "xhigh");
        assert_eq!(ReasoningEffort::XHigh.transport_value(), "xhigh");
        assert_eq!(ReasoningEffort::High.label(), "High");
    }

    #[test]
    fn default_matches_provider_family() {
        let _home = isolated_catalog_home();
        // Temporarily hardcoded to a real effort level (was `Auto`) for
        // openai/openai-codex/fallback so reasoning content actually gets
        // requested by default — see the comment on `default_for_model`.
        assert_eq!(
            ReasoningEffort::default_for_model("openai/gpt-4.1-mini"),
            ReasoningEffort::Medium
        );
        assert_eq!(
            ReasoningEffort::default_for_model("anthropic/claude-sonnet-4"),
            ReasoningEffort::Low
        );
        assert_eq!(
            ReasoningEffort::default_for_model("opencode-go/deepseek-v4-flash"),
            ReasoningEffort::Medium
        );
    }

    #[test]
    fn options_match_transport_support() {
        let _home = isolated_catalog_home();
        assert_eq!(
            ReasoningEffort::options_for_model("openai/gpt-5.2"),
            vec![
                ReasoningEffort::Minimal,
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::XHigh
            ]
        );
        assert!(
            ReasoningEffort::options_for_model("openai/gpt-5.2").contains(&ReasoningEffort::High)
        );
        assert!(
            ReasoningEffort::options_for_model("opencode-go/claude-sonnet-4")
                .contains(&ReasoningEffort::Medium)
        );
        assert!(
            ReasoningEffort::options_for_model("openai-codex/gpt-5.6-sol")
                .contains(&ReasoningEffort::Max)
        );
        assert!(
            !ReasoningEffort::options_for_model("openai-codex/gpt-5.6-sol")
                .contains(&ReasoningEffort::Auto)
        );
        assert!(
            ReasoningEffort::options_for_model("anthropic/claude-sonnet-4-6")
                .contains(&ReasoningEffort::High)
        );
        assert!(
            !ReasoningEffort::options_for_model("anthropic/claude-sonnet-4-6")
                .contains(&ReasoningEffort::XHigh)
        );
    }

    #[test]
    fn step_moves_within_model_options_and_clamps() {
        let _home = isolated_catalog_home();
        let model = "openai/gpt-5.2"; // [Minimal, Low, Medium, High, XHigh]
        assert_eq!(
            ReasoningEffort::Medium.step(model, true),
            ReasoningEffort::High
        );
        assert_eq!(
            ReasoningEffort::Medium.step(model, false),
            ReasoningEffort::Low
        );
        assert_eq!(
            ReasoningEffort::XHigh.step(model, true),
            ReasoningEffort::XHigh,
            "clamps at the top rather than wrapping"
        );
        assert_eq!(
            ReasoningEffort::Minimal.step(model, false),
            ReasoningEffort::Minimal,
            "clamps at the bottom rather than wrapping"
        );
    }

    #[test]
    fn step_is_a_noop_when_current_value_is_not_a_valid_option() {
        let _home = isolated_catalog_home();
        // A model with no adjustable effort only ever offers its single
        // default value from `options_for_model` — stepping must not panic
        // or invent a level the model doesn't actually support.
        assert_eq!(
            ReasoningEffort::High.step("mock", true),
            ReasoningEffort::High
        );
    }

    #[test]
    fn parse_invalid_returns_err() {
        assert!("bogus".parse::<ReasoningEffort>().is_err());
    }

    #[test]
    fn default_is_auto() {
        assert_eq!(ReasoningEffort::default(), ReasoningEffort::Auto);
    }

    #[test]
    fn max_options_are_available_for_some_models() {
        let _home = isolated_catalog_home();
        let opts = ReasoningEffort::options_for_model("openai-codex/gpt-5.8");
        assert!(opts.contains(&ReasoningEffort::Max));
    }

    #[test]
    fn unknown_model_returns_default() {
        let _home = isolated_catalog_home();
        let opts = ReasoningEffort::options_for_model("mocked/model");
        assert_eq!(opts, vec![ReasoningEffort::Medium]);
    }

    #[test]
    fn grok_46_is_adjustable_even_without_a_warm_catalog() {
        let _home = isolated_catalog_home();
        assert!(ReasoningEffort::model_supports_effort("xai/grok-4.6"));
        assert!(
            ReasoningEffort::options_for_model("xai/grok-4.6").contains(&ReasoningEffort::XHigh)
        );
    }
}
