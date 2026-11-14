//! The tier table: incremental builds, persistence, and resolution from
//! memory. Hermetic: every test has its own catalog, table and provider
//! file.

use std::collections::{HashMap, HashSet};

use forge_connect::ProviderSpec;
use forge_core::{ModelExecutionSelection, ModelTier, TierBasis};

use super::*;

/// What models.dev says in a test, per model:
/// `(id, input $/M, output $/M, release date, family)`.
type ModelsDevRow = (&'static str, f64, f64, &'static str, &'static str);

const ANTHROPIC: &[ModelsDevRow] = &[
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
        25.0,
        "2026-05-01",
        "claude-opus",
    ),
];

const OPENAI: &[ModelsDevRow] = &[
    ("openai/gpt-5", 1.25, 10.0, "2025-08-07", "gpt-5"),
    ("openai/gpt-5-mini", 0.25, 2.0, "2025-08-07", "gpt-5-mini"),
];

const ACME: &[ModelsDevRow] = &[
    ("acme/small", 0.5, 1.0, "2026-01-01", "small"),
    ("acme/large", 4.0, 16.0, "2026-01-01", "large"),
];

struct Fixture {
    _dir: tempfile::TempDir,
    paths: TierPaths,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = TierPaths {
            catalog: Some(dir.path().join("model-catalog.toml")),
            table: Some(dir.path().join(TIER_TABLE_FILE)),
            // No such file: the built-in providers only.
            providers: Some(dir.path().join("providers.toml")),
        };
        Self { _dir: dir, paths }
    }

    fn table(&self) -> Arc<TierTable> {
        TierTable::for_paths(&self.paths).unwrap()
    }

    /// Write the catalog a models.dev refresh would leave, for `providers`
    /// (profile id and its rows).
    fn write_catalog(&self, providers: &[(&str, &[ModelsDevRow])]) {
        let mut text = String::from(
            "version = 2\n\
             registry_image_input = [\"anthropic/claude-opus-4-8\"]\n\
             registry_image_input_ready = true\n\
             registry_effort_ready = true\n\
             registry_tiers_ready = true\n\n\
             [registry_models]\n",
        );
        for (profile, rows) in providers {
            let ids: Vec<String> = rows.iter().map(|row| format!("\"{}\"", row.0)).collect();
            text.push_str(&format!("{profile} = [{}]\n", ids.join(", ")));
        }
        text.push_str(
            "\n[registry_effort]\n\
             \"anthropic/claude-haiku-4-5\" = []\n\
             \"openai/gpt-5\" = [\"minimal\", \"low\", \"medium\", \"high\"]\n\
             \"openai/gpt-5-mini\" = [\"low\", \"medium\", \"high\"]\n",
        );
        text.push_str(
            "\n[registry_limits.\"anthropic/claude-opus-4-8\"]\ncontext = 1000000\noutput = 64000\n",
        );
        for (_, rows) in providers {
            for (id, input, output, date, family) in rows.iter() {
                text.push_str(&format!(
                    "\n[registry_tiers.\"{id}\"]\ninput_cost = {input:?}\noutput_cost = {output:?}\n\
                     release_date = \"{date}\"\nfamily = \"{family}\"\nreasoning = true\n"
                ));
            }
        }
        std::fs::write(self.paths.catalog.as_ref().unwrap(), text).unwrap();
    }
}

fn builtin(ids: &[&str]) -> Vec<ProviderSpec> {
    ids.iter()
        .map(|id| forge_connect::builtin::builtin_profile(id))
        .collect()
}

/// A user-defined provider that models.dev describes as `acme`.
fn acme() -> ProviderSpec {
    let mut spec = forge_connect::builtin::builtin_profile("openai");
    spec.id = "acme".into();
    spec.title = "Acme".into();
    spec.model_provider_prefix = "acme".into();
    spec.models_dev_providers = vec!["acme".into()];
    spec
}

fn on(model: &str, effort: Option<&str>) -> ModelExecutionSelection {
    ModelExecutionSelection {
        model: model.into(),
        profile_id: model.split_once('/').unwrap().0.into(),
        reasoning_effort: effort.map(str::to_string),
        thinking_enabled: true,
    }
}

fn runnable(entries: &[(&str, &[&str])]) -> HashMap<String, HashSet<String>> {
    entries
        .iter()
        .map(|(profile, models)| {
            (
                profile.to_string(),
                models.iter().map(|model| model.to_string()).collect(),
            )
        })
        .collect()
}

async fn stored_models(table: &TierTable) -> Vec<(String, String, String, i64, f64)> {
    sqlx::query("SELECT * FROM tier_models ORDER BY profile_id, tier, rank")
        .fetch_all(table.pool().await.unwrap())
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.get("profile_id"),
                row.get("tier"),
                row.get("model_id"),
                row.get("rank"),
                row.get("blended_cost"),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_first_sync_builds_each_provider_and_the_next_changes_nothing() {
    let fixture = Fixture::new();
    fixture.write_catalog(&[("anthropic", ANTHROPIC), ("openai", OPENAI)]);
    let table = fixture.table();

    let first = table
        .sync(builtin(&["anthropic", "openai", "ollama"]), &fixture.paths)
        .await
        .unwrap();
    assert_eq!(first.rebuilt, ["anthropic", "ollama", "openai"]);
    assert!(first.unchanged.is_empty() && first.removed.is_empty());
    // 3 Anthropic models and 2 OpenAI ones, ranked for each of 3 tiers.
    assert_eq!(first.models_written, 3 * 3 + 2 * 3);
    assert_eq!(stored_models(&table).await.len(), 15);

    let second = table
        .sync(builtin(&["anthropic", "openai", "ollama"]), &fixture.paths)
        .await
        .unwrap();
    assert_eq!(second.unchanged, ["anthropic", "ollama", "openai"]);
    assert!(!second.changed());
    assert_eq!(second.models_written + second.efforts_written, 0);
}

#[tokio::test]
async fn a_changed_price_rebuilds_only_that_provider_and_writes_only_what_differs() {
    let fixture = Fixture::new();
    fixture.write_catalog(&[("anthropic", ANTHROPIC), ("openai", OPENAI)]);
    let table = fixture.table();
    let profiles = || builtin(&["anthropic", "openai"]);
    table.sync(profiles(), &fixture.paths).await.unwrap();
    table.reload().await.unwrap();
    let generation = table.snapshot_read().generation;

    // models.dev changes Opus's output price; its ranking stays the same.
    let mut changed = ANTHROPIC.to_vec();
    changed[2].2 = 30.0;
    fixture.write_catalog(&[("anthropic", &changed), ("openai", OPENAI)]);
    let report = table.sync(profiles(), &fixture.paths).await.unwrap();
    assert_eq!(report.rebuilt, ["anthropic"]);
    assert_eq!(report.unchanged, ["openai"]);
    // Opus's row in each tier, nothing else.
    assert_eq!(report.models_written, 3);
    assert_eq!(report.models_deleted + report.efforts_written, 0);
    let opus: Vec<f64> = stored_models(&table)
        .await
        .into_iter()
        .filter(|row| row.2 == "anthropic/claude-opus-4-8")
        .map(|row| row.4)
        .collect();
    // (3 × 5 + 30) / 4
    assert_eq!(opus, [11.25, 11.25, 11.25]);

    // Readers notice, and only then reload.
    table.reload().await.unwrap();
    assert_ne!(table.snapshot_read().generation, generation);
}

#[tokio::test]
async fn a_custom_provider_is_built_alone_and_deleted_when_removed() {
    let fixture = Fixture::new();
    fixture.write_catalog(&[("anthropic", ANTHROPIC), ("acme", ACME)]);
    let table = fixture.table();
    table
        .sync(builtin(&["anthropic"]), &fixture.paths)
        .await
        .unwrap();

    let mut with_acme = builtin(&["anthropic"]);
    with_acme.push(acme());
    let added = table.sync(with_acme, &fixture.paths).await.unwrap();
    assert_eq!(added.rebuilt, ["acme"]);
    assert_eq!(added.unchanged, ["anthropic"]);
    assert_eq!(added.models_written, 2 * 3);

    table.reload().await.unwrap();
    table.set_runnable(runnable(&[("acme", &["acme/small", "acme/large"])]));
    let strong = table.resolve(ModelTier::Strong, &on("acme/small", None));
    assert_eq!(strong.model.model, "acme/large");
    assert_eq!(strong.basis, TierBasis::Catalog);

    let removed = table
        .sync(builtin(&["anthropic"]), &fixture.paths)
        .await
        .unwrap();
    assert_eq!(removed.removed, ["acme"]);
    assert_eq!(removed.unchanged, ["anthropic"]);
    assert_eq!(removed.models_deleted, 2 * 3);
    assert!(stored_models(&table)
        .await
        .iter()
        .all(|row| row.0 == "anthropic"));
    table.reload().await.unwrap();
    assert_eq!(
        table
            .resolve(ModelTier::Strong, &on("acme/small", None))
            .basis,
        TierBasis::Unavailable
    );
}

#[tokio::test]
async fn a_tier_is_the_first_ranked_model_the_account_can_run() {
    let fixture = Fixture::new();
    fixture.write_catalog(&[("anthropic", ANTHROPIC), ("openai", OPENAI)]);
    let table = fixture.table();
    table
        .sync(builtin(&["anthropic", "openai"]), &fixture.paths)
        .await
        .unwrap();
    table.reload().await.unwrap();
    // This account has not been given Opus, and has no OpenAI model list.
    table.set_runnable(runnable(&[(
        "anthropic",
        &["anthropic/claude-haiku-4-5", "anthropic/claude-sonnet-4-6"],
    )]));
    let current = on("anthropic/claude-sonnet-4-6", Some("medium"));

    let strong = table.resolve(ModelTier::Strong, &current);
    assert_eq!(strong.basis, TierBasis::Catalog);
    assert_eq!(
        strong.model.model, "anthropic/claude-sonnet-4-6",
        "Opus is not runnable"
    );
    assert_eq!(strong.model.reasoning_effort.as_deref(), Some("high"));
    let fast = table.resolve(ModelTier::Fast, &current);
    assert_eq!(fast.model.model, "anthropic/claude-haiku-4-5");
    assert_eq!(fast.model.reasoning_effort, None);
    assert_eq!(fast.model.image_input, Some(false));

    // No account list for OpenAI: the current model, at the tier's effort.
    let effort_only = table.resolve(ModelTier::Strong, &on("openai/gpt-5", Some("low")));
    assert_eq!(effort_only.basis, TierBasis::EffortOnly);
    assert_eq!(effort_only.model.model, "openai/gpt-5");
    assert_eq!(effort_only.model.reasoning_effort.as_deref(), Some("high"));

    // A provider the table does not know changes nothing.
    let ollama = on("ollama/llama3", None);
    let unknown = table.resolve(ModelTier::Strong, &ollama);
    assert_eq!(unknown.basis, TierBasis::Unavailable);
    assert!(unknown.matches(&ollama));
}

/// The account has not been given the newest Opus: the tier takes the
/// line's older release, not the next line down.
#[tokio::test]
async fn without_a_lines_newest_release_the_tier_takes_its_older_one() {
    const OPUS_GENERATIONS: &[ModelsDevRow] = &[
        (
            "anthropic/claude-sonnet-5-5",
            2.0,
            10.0,
            "2026-09-28",
            "claude-sonnet",
        ),
        (
            "anthropic/claude-opus-5-5",
            4.0,
            20.0,
            "2026-09-22",
            "claude-opus",
        ),
        (
            "anthropic/claude-opus-5",
            5.0,
            25.0,
            "2026-07-24",
            "claude-opus",
        ),
    ];
    let fixture = Fixture::new();
    fixture.write_catalog(&[("anthropic", OPUS_GENERATIONS)]);
    let table = fixture.table();
    table
        .sync(builtin(&["anthropic"]), &fixture.paths)
        .await
        .unwrap();
    table.reload().await.unwrap();
    table.set_runnable(runnable(&[(
        "anthropic",
        &["anthropic/claude-sonnet-5-5", "anthropic/claude-opus-5"],
    )]));
    let strong = table.resolve(ModelTier::Strong, &on("anthropic/claude-sonnet-5-5", None));
    assert_eq!(strong.model.model, "anthropic/claude-opus-5");
}

/// A model models.dev does not list (here, one the account's own catalog
/// returned) still moves its effort for a tier, by Forge's effort rules.
#[tokio::test]
async fn an_unlisted_model_takes_the_tiers_effort_from_forges_rules() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.paths.catalog.as_ref().unwrap(),
        "version = 2\n\n[models]\nxai = [\"xai/grok-4.6-fast\"]\nollama = [\"ollama/llama3.3\"]\n",
    )
    .unwrap();
    let table = fixture.table();
    table
        .sync(builtin(&["xai", "ollama"]), &fixture.paths)
        .await
        .unwrap();
    table.reload().await.unwrap();
    assert!(!table.has_rankings(), "models.dev says nothing about them");
    table
        .refresh_runnable(&fixture.paths, std::time::Duration::ZERO)
        .await
        .unwrap();

    let current = on("xai/grok-4.6-fast", Some("medium"));
    let strong = table.resolve(ModelTier::Strong, &current);
    assert_eq!(strong.basis, TierBasis::EffortOnly);
    assert_eq!(strong.model.model, "xai/grok-4.6-fast");
    assert_eq!(strong.model.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(
        table
            .resolve(ModelTier::Fast, &current)
            .model
            .reasoning_effort
            .as_deref(),
        Some("low")
    );
    // No adjustable effort: nothing to change.
    assert_eq!(
        table
            .resolve(ModelTier::Strong, &on("ollama/llama3.3", None))
            .basis,
        TierBasis::Unavailable
    );
}

#[tokio::test]
async fn the_table_survives_a_restart_and_is_read_without_the_catalog() {
    let fixture = Fixture::new();
    fixture.write_catalog(&[("anthropic", ANTHROPIC)]);
    fixture
        .table()
        .sync(builtin(&["anthropic"]), &fixture.paths)
        .await
        .unwrap();
    // A new process: its own handle on the file, and the catalog gone.
    std::fs::remove_file(fixture.paths.catalog.as_ref().unwrap()).unwrap();
    let restarted = TierTable::new(fixture.paths.table.as_ref().unwrap());
    assert!(!restarted.loaded());
    restarted.reload().await.unwrap();
    assert!(restarted.loaded() && restarted.has_rankings());
    restarted.set_runnable(runnable(&[(
        "anthropic",
        &["anthropic/claude-opus-4-8", "anthropic/claude-haiku-4-5"],
    )]));
    let strong = restarted.resolve(ModelTier::Strong, &on("anthropic/claude-haiku-4-5", None));
    assert_eq!(strong.model.model, "anthropic/claude-opus-4-8");
    assert_eq!(strong.model.context_window, Some(1_000_000));
    assert_eq!(strong.model.output_reserve, Some(64_000));
    assert_eq!(strong.model.image_input, Some(true));
}

/// The onboarding wait: an empty catalog, so models.dev is downloaded, and
/// the table is built and loaded before it returns.
#[tokio::test]
async fn preparing_downloads_models_dev_and_builds_the_table() {
    let Some(base) = forge_test_support::mock_http(vec![(
        200,
        r#"{"anthropic":{"models":{
            "claude-haiku-4-5":{"family":"claude-haiku","reasoning":true,"release_date":"2025-10-15","cost":{"input":1,"output":5},"limit":{"context":200000,"output":64000},"tool_call":true,"modalities":{"output":["text"]}},
            "claude-opus-4-8":{"family":"claude-opus","reasoning":true,"release_date":"2026-05-01","cost":{"input":5,"output":25},"limit":{"context":1000000,"output":64000},"tool_call":true,"modalities":{"output":["text"]}}
        }}}"#,
        vec![],
    )]) else {
        eprintln!("skipping: this host denies binding a listener");
        return;
    };
    // Only this test in the crate downloads models.dev.
    std::env::set_var("FORGE_MODELS_DEV_URL", &base);
    let fixture = Fixture::new();

    let started = Instant::now();
    let report = prepare_model_tiers(fixture.paths.clone(), None)
        .await
        .unwrap();
    std::env::remove_var("FORGE_MODELS_DEV_URL");
    eprintln!("prepare_model_tiers took {:?}", started.elapsed());
    assert!(
        report.rebuilt.contains(&"anthropic".to_string()),
        "{report:?}"
    );
    let table = fixture.table();
    assert!(table.loaded() && table.has_rankings());
    let catalog = ModelCatalogCache::new(fixture.paths.catalog.clone().unwrap());
    assert!(catalog.tier_metadata_ready());

    // Nothing changed since: a second run downloads nothing and writes
    // nothing.
    let again = prepare_model_tiers(fixture.paths.clone(), None)
        .await
        .unwrap();
    assert!(!again.changed(), "{again:?}");
}

/// A session resolves tiers from the table in memory.
#[tokio::test]
async fn a_session_resolves_tiers_from_the_table() {
    let fixture = Fixture::new();
    fixture.write_catalog(&[("anthropic", ANTHROPIC)]);
    let table = fixture.table();
    table
        .sync(builtin(&["anthropic"]), &fixture.paths)
        .await
        .unwrap();
    table.set_runnable(runnable(&[(
        "anthropic",
        &["anthropic/claude-opus-4-8", "anthropic/claude-haiku-4-5"],
    )]));

    let workspace = tempfile::tempdir().unwrap();
    let mut cfg = forge_config::Config {
        resolved_workspace: workspace.path().to_path_buf(),
        workspace_root: Some(workspace.path().display().to_string()),
        ..Default::default()
    };
    cfg.model.catalog_path = fixture.paths.catalog.clone();
    cfg.model.tiers_path = fixture.paths.table.clone();
    cfg.journal.path = workspace.path().join("journal").display().to_string();
    let model = Arc::new(forge_model::MockModelClient::script(vec![]));
    let opened = crate::open_session_with_model(&cfg, crate::SessionTarget::New, model)
        .await
        .unwrap();
    let resolved = opened
        .session
        .resolve_model_tier(ModelTier::Strong, &on("anthropic/claude-haiku-4-5", None));
    assert_eq!(resolved.basis, TierBasis::Catalog);
    assert_eq!(resolved.model.model, "anthropic/claude-opus-4-8");
}

/// The whole pipeline against the real models.dev (or the copy
/// `FORGE_MODELS_DEV_URL` names): download and ingest, build the table,
/// load it, resolve. Prints each phase's time and every built-in
/// provider's tiers, as if the account could run every model models.dev
/// lists for it. Needs the network, so it runs only when asked:
///
/// ```sh
/// cargo test --release -p forge-session live_models_dev -- --ignored --nocapture
/// ```
#[tokio::test]
#[ignore = "downloads models.dev"]
async fn live_models_dev_tiers_and_timings() {
    let fixture = Fixture::new();
    let paths = fixture.paths.clone();
    let profiles = paths.registry().profiles().to_vec();

    let download_paths = paths.clone();
    let download_profiles = profiles.clone();
    let (ingested, download_ms) = tokio::task::spawn_blocking(move || {
        let cache = download_paths.catalog();
        let started = Instant::now();
        let rows = forge_connect::refresh_models_dev_registry(&download_profiles, &cache)
            .expect("models.dev download");
        (rows, started.elapsed())
    })
    .await
    .unwrap();
    let catalog_kb = std::fs::metadata(paths.catalog.as_ref().unwrap())
        .unwrap()
        .len()
        / 1024;

    let table = fixture.table();
    let started = Instant::now();
    let first = table.sync(profiles.clone(), &paths).await.unwrap();
    let first_sync = started.elapsed();
    let started = Instant::now();
    let again = table.sync(profiles.clone(), &paths).await.unwrap();
    let no_op_sync = started.elapsed();
    let started = Instant::now();
    table.reload().await.unwrap();
    let reload = started.elapsed();
    let rows = stored_models(&table).await.len();

    // As if the account could run everything models.dev lists.
    let cache_paths = paths.clone();
    let everything: HashMap<String, HashSet<String>> = tokio::task::spawn_blocking(move || {
        let cache = cache_paths.catalog();
        let registry = cache_paths.registry();
        registry
            .profiles()
            .iter()
            .map(|profile| {
                let ids = cache
                    .registry_rows(&profile.id)
                    .into_iter()
                    .map(|row| row.id)
                    .collect();
                (profile.id.clone(), ids)
            })
            .collect()
    })
    .await
    .unwrap();
    table.set_runnable(everything);

    println!("\n== models.dev → tiers ==");
    println!(
        "download + parse + ingest + catalog save: {download_ms:?} ({ingested} models \
         kept for Forge's providers; catalog file {catalog_kb} KB)"
    );
    println!(
        "first build: {first_sync:?} (rebuilt {:?}, {} tier rows written)",
        first.rebuilt, first.models_written
    );
    println!(
        "no-change sync: {no_op_sync:?} (unchanged {:?})",
        again.unchanged
    );
    println!("load into memory: {reload:?} ({rows} tier rows)");

    let mut resolved = 0;
    let started = Instant::now();
    for profile in &profiles {
        let Some(first_choice) = table
            .snapshot_read()
            .profiles
            .get(&profile.id)
            .and_then(|tiers| tiers.ranked.get(&ModelTier::Default))
            .and_then(|rows| rows.first())
            .map(|row| row.model_id.clone())
        else {
            println!("{:>14}: no ranked models", profile.id);
            continue;
        };
        let current = ModelExecutionSelection {
            model: first_choice,
            profile_id: profile.id.clone(),
            reasoning_effort: None,
            thinking_enabled: true,
        };
        let line: Vec<String> = ModelTier::ALL
            .iter()
            .map(|tier| {
                resolved += 1;
                let resolution = table.resolve(*tier, &current);
                match resolution.model.reasoning_effort {
                    Some(effort) => format!("{tier}={} @ {effort}", resolution.model.model),
                    None => format!("{tier}={}", resolution.model.model),
                }
            })
            .collect();
        println!("{:>14}: {}", profile.id, line.join("  |  "));
    }
    let lookups = started.elapsed();

    let current = ModelExecutionSelection {
        model: "anthropic/claude-sonnet-4-5".into(),
        profile_id: "anthropic".into(),
        reasoning_effort: None,
        thinking_enabled: true,
    };
    let calls = 100_000u32;
    let started = Instant::now();
    for _ in 0..calls {
        std::hint::black_box(table.resolve(ModelTier::Strong, &current));
    }
    println!(
        "tier lookup during a turn: {:?} per call ({resolved} lookups above took {lookups:?})",
        started.elapsed() / calls
    );
    assert!(first.changed() && !again.changed());
    assert!(table.has_rankings());
}
