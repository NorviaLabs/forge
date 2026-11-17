//! Model selection: a session's model, profile and effort, together with the
//! capabilities derived for that model from the model catalog. Every path
//! that puts a session on a model goes through here, so the model and its
//! capabilities cannot drift apart.

use forge_config::Config;
use forge_core::{AgentSession, CompactionPolicy, ModelExecutionSelection};

/// Put `session` on `selection` and derive that model's capabilities.
pub(crate) fn apply(session: &mut AgentSession, cfg: &Config, selection: ModelExecutionSelection) {
    session.set_model_selection(selection);
    derive_capabilities(session, cfg);
}

/// Set image-input support and the context window for the model the session
/// is on. Compaction reads the model's real window from the catalog; a model
/// the catalog does not know gets `CompactionPolicy`'s default, set
/// explicitly so the previous model's window never carries over. A session
/// with no model yet is left alone.
pub(crate) fn derive_capabilities(session: &mut AgentSession, cfg: &Config) {
    let model = session.model_selection().model;
    if model.is_empty() {
        return;
    }
    let cache = match &cfg.model.catalog_path {
        Some(path) => forge_connect::ModelCatalogCache::new(path.clone()),
        None => forge_connect::ModelCatalogCache::user_default(),
    };
    session.set_image_input_supported(cache.model_accepts_image_input(&model));
    match cache
        .model_limits(&model)
        .filter(|limits| limits.context > 0)
    {
        Some(limits) => {
            session.set_context_window(limits.context, (limits.output > 0).then_some(limits.output))
        }
        None => session.set_context_window(CompactionPolicy::default().context_window, None),
    }
}

/// Validates the model and reasoning effort a `spawn_agent` call asks for,
/// against the providers the user has and the model catalog, and looks up
/// the chosen model's limits, the same way a model switch in the app does.
pub(crate) struct CatalogChildModels {
    catalog_path: Option<std::path::PathBuf>,
    routes: Box<dyn Fn() -> forge_model::RouteTable + Send + Sync>,
    /// The catalog as last read, with the file's stamp then: Auto asks for
    /// efforts and resolves models at the start of every turn, and reading
    /// a large catalog takes tens of milliseconds.
    loaded: std::sync::Mutex<Option<(CatalogStamp, forge_connect::ModelCatalogCache)>>,
    /// How many times the catalog file was read.
    #[cfg(test)]
    reads: std::sync::atomic::AtomicUsize,
}

/// A file's size and modification time (`None`: no file).
type CatalogStamp = Option<(u64, Option<std::time::SystemTime>)>;

impl CatalogChildModels {
    /// Routes are read on every call, so a provider connected after the
    /// session started is accepted at once.
    pub(crate) fn new(cfg: &Config) -> Self {
        Self::with_routes(cfg.model.catalog_path.clone(), Box::new(crate::route_table))
    }

    /// With the routes (and catalog) given rather than the user's own.
    pub(crate) fn with_routes(
        catalog_path: Option<std::path::PathBuf>,
        routes: Box<dyn Fn() -> forge_model::RouteTable + Send + Sync>,
    ) -> Self {
        Self {
            catalog_path,
            routes,
            loaded: Default::default(),
            #[cfg(test)]
            reads: Default::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn catalog_reads(&self) -> usize {
        self.reads.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Run `f` on the catalog: read once, and again only when the file
    /// changes (so an updated catalog is seen at once).
    fn with_catalog<R>(&self, f: impl FnOnce(&forge_connect::ModelCatalogCache) -> R) -> R {
        let path = self
            .catalog_path
            .clone()
            .unwrap_or_else(forge_connect::ModelCatalogCache::user_default_path);
        let stamp: CatalogStamp = std::fs::metadata(&path)
            .ok()
            .map(|meta| (meta.len(), meta.modified().ok()));
        let mut loaded = self
            .loaded
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if loaded.as_ref().is_none_or(|(kept, _)| *kept != stamp) {
            #[cfg(test)]
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *loaded = Some((stamp, forge_connect::ModelCatalogCache::new(path)));
        }
        let (_, catalog) = loaded.as_ref().expect("the catalog was just read");
        f(catalog)
    }
}

impl crate::auto_models::EffortLookup for CatalogChildModels {
    /// From the provider serving `model` and the catalog: its levels, lowest
    /// first, and its default — or none, for a model without an effort
    /// setting (or one no provider serves).
    fn efforts(&self, model: &str, _profile_id: &str) -> forge_core::ModelEfforts {
        let table = (self.routes)();
        let Some((found, bare)) = table.profile_for_request(model) else {
            return forge_core::ModelEfforts::default();
        };
        let support = self.with_catalog(|catalog| {
            forge_connect::effort::effort(&found.definition, bare, catalog)
        });
        if !support.supported() {
            return forge_core::ModelEfforts::default();
        }
        forge_core::ModelEfforts {
            options: support
                .options
                .iter()
                .map(|level| level.as_str().to_string())
                .collect(),
            default: Some(support.default.as_str().to_string()),
        }
    }
}

impl forge_core::ChildModelResolver for CatalogChildModels {
    fn resolve(
        &self,
        request: &forge_core::ChildModelRequest<'_>,
    ) -> Result<forge_core::ChildModel, String> {
        use forge_connect::effort::EffortLevel;

        let mut child = forge_core::resolve_child_model_shape(request)?;
        let model = child.model.clone();
        let table = (self.routes)();
        let Some((found, bare)) = table.profile_for_request(&model) else {
            if let Some(current) = forge_connect::old_prefix_hint(&model) {
                return Err(format!(
                    "model `{model}` uses an old provider prefix; use `{current}`"
                ));
            }
            let known: Vec<&str> = table
                .profiles()
                .map(|found| found.definition.profile_id.as_str())
                .collect();
            return Err(format!(
                "no provider serves model `{model}`; model ids are `<provider>/<model>` with one of: {}",
                known.join(", ")
            ));
        };
        child.profile_id = found.definition.profile_id.clone();
        let (support, limits, image_input) = self.with_catalog(|catalog| {
            (
                forge_connect::effort::effort(&found.definition, bare, catalog),
                catalog.model_limits(&model),
                catalog.model_accepts_image_input(&model),
            )
        });
        match request.reasoning_effort {
            Some(requested) => {
                let level = EffortLevel::parse(requested)
                    .ok_or_else(|| format!("`{requested}` is not a reasoning effort"))?;
                if !support.supported() {
                    return Err(format!(
                        "model `{model}` has no adjustable reasoning effort; omit `reasoning_effort`"
                    ));
                }
                if !support.options.contains(&level) {
                    let options: Vec<&str> =
                        support.options.iter().map(|level| level.as_str()).collect();
                    return Err(format!(
                        "model `{model}` takes reasoning effort {}, not `{}`",
                        options.join(", "),
                        level.as_str()
                    ));
                }
                child.reasoning_effort = Some(level.as_str().to_string());
            }
            // Another model starts on its own default effort.
            None if request.model.is_some() => {
                child.reasoning_effort = support
                    .supported()
                    .then(|| support.default.as_str().to_string());
            }
            None => {}
        }
        if let Some(limits) = limits.filter(|limits| limits.context > 0) {
            child.context_window = Some(limits.context);
            child.output_reserve = (limits.output > 0).then_some(limits.output);
        }
        child.image_input = Some(image_input);
        Ok(child)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// A catalog that knows one model: `test/small`, 200k context, 8k
    /// output, image input.
    pub(crate) fn write_catalog(dir: &Path) -> PathBuf {
        let path = dir.join("model-catalog.toml");
        std::fs::write(
            &path,
            "version = 2\n\
             registry_image_input = [\"test/small\"]\n\
             registry_image_input_ready = true\n\n\
             [registry_limits.\"test/small\"]\n\
             context = 200000\n\
             output = 8000\n",
        )
        .unwrap();
        path
    }

    async fn session(cfg: &Config) -> AgentSession {
        crate::open_session(cfg, crate::SessionTarget::New)
            .await
            .unwrap()
            .session
    }

    fn cfg(dir: &Path) -> Config {
        let mut cfg = Config {
            resolved_workspace: dir.to_path_buf(),
            workspace_root: Some(dir.display().to_string()),
            ..Default::default()
        };
        cfg.model.mock = true;
        cfg.model.catalog_path = Some(write_catalog(dir));
        cfg.journal.path = dir.join("journal").display().to_string();
        cfg
    }

    fn on(session: &AgentSession, model: &str) -> ModelExecutionSelection {
        ModelExecutionSelection {
            model: model.into(),
            ..session.model_selection()
        }
    }

    #[tokio::test]
    async fn a_known_model_gets_its_catalog_window_and_image_input() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let mut session = session(&cfg).await;
        // `mock` is not in the catalog: the session opens on the default.
        let default = CompactionPolicy::default();
        assert_eq!(
            session.compaction_policy().context_window,
            default.context_window
        );
        assert!(!session.image_input_supported());

        let selection = on(&session, "test/small");
        apply(&mut session, &cfg, selection);
        assert_eq!(session.model_selection().model, "test/small");
        assert_eq!(
            *session.compaction_policy(),
            CompactionPolicy::for_window(200_000, Some(8_000))
        );
        assert!(session.image_input_supported());
    }

    #[tokio::test]
    async fn an_unknown_model_does_not_keep_the_previous_models_window() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg(dir.path());
        let mut session = session(&cfg).await;
        let selection = on(&session, "test/small");
        apply(&mut session, &cfg, selection);

        let selection = on(&session, "test/unlisted");
        apply(&mut session, &cfg, selection);
        assert_eq!(*session.compaction_policy(), CompactionPolicy::default());
        assert!(!session.image_input_supported());
    }

    /// The efforts Auto's ladder climbs, from the provider and the catalog:
    /// an anthropic model with an effort setting lists its levels in order
    /// with its default; one without lists none.
    #[test]
    fn a_models_efforts_come_from_its_provider_and_the_catalog() {
        use crate::auto_models::EffortLookup;
        let dir = tempfile::tempdir().unwrap();
        let models = child_models(dir.path());
        let opus = models.efforts("anthropic/claude-opus-4-8", "anthropic");
        assert_eq!(opus.options, ["low", "medium", "high", "xhigh"]);
        assert_eq!(opus.default.as_deref(), Some("low"));
        assert_eq!(
            models.efforts("anthropic/claude-sonnet-4", "anthropic"),
            forge_core::ModelEfforts::default()
        );
        assert_eq!(
            models.efforts("nobody/serves-this", "nobody"),
            forge_core::ModelEfforts::default()
        );
    }

    /// Auto asks for efforts and resolves models at the start of every
    /// turn: the catalog is read once, and again only when it changes (a
    /// 2,000-model catalog took about 26 ms to read, three times a turn).
    #[test]
    fn the_catalog_is_read_once_until_it_changes() {
        use crate::auto_models::EffortLookup;
        use forge_core::ChildModelResolver;
        let dir = tempfile::tempdir().unwrap();
        let models = child_models(dir.path());
        let parent = forge_core::ModelExecutionSelection {
            model: "anthropic/claude-opus-4-8".into(),
            profile_id: "anthropic".into(),
            reasoning_effort: None,
            thinking_enabled: true,
        };
        for _ in 0..5 {
            let _ = models.efforts("anthropic/claude-opus-4-8", "anthropic");
            let _ = models.resolve(&forge_core::ChildModelRequest {
                model: Some("anthropic/claude-opus-4-8"),
                reasoning_effort: None,
                parent: &parent,
            });
        }
        assert_eq!(models.catalog_reads(), 1);
        assert_eq!(
            models
                .resolve(&forge_core::ChildModelRequest {
                    model: Some("anthropic/claude-opus-4-8"),
                    reasoning_effort: None,
                    parent: &parent,
                })
                .unwrap()
                .context_window,
            Some(1_000_000)
        );

        // The catalog is updated: the next call sees it.
        std::fs::write(
            dir.path().join("child-catalog.toml"),
            "version = 2\n\
             registry_effort_ready = true\n\n\
             [registry_effort]\n\
             \"anthropic/claude-opus-4-8\" = [\"low\", \"high\"]\n\n\
             [registry_limits.\"anthropic/claude-opus-4-8\"]\n\
             context = 500000\n\
             output = 32000\n",
        )
        .unwrap();
        let efforts = models.efforts("anthropic/claude-opus-4-8", "anthropic");
        assert_eq!(efforts.options, ["low", "high"]);
        assert_eq!(models.catalog_reads(), 2);
        let _ = models.efforts("anthropic/claude-opus-4-8", "anthropic");
        assert_eq!(models.catalog_reads(), 2);
    }

    fn child_models(dir: &Path) -> CatalogChildModels {
        let catalog = dir.join("child-catalog.toml");
        std::fs::write(
            &catalog,
            "version = 2\n\
             registry_image_input = [\"anthropic/claude-opus-4-8\"]\n\
             registry_image_input_ready = true\n\n\
             [registry_limits.\"anthropic/claude-opus-4-8\"]\n\
             context = 1000000\n\
             output = 64000\n",
        )
        .unwrap();
        CatalogChildModels::with_routes(
            Some(catalog),
            Box::new(|| {
                forge_model::RouteTable::single(
                    forge_connect::RouteDefinition::from_spec(
                        &forge_connect::builtin::builtin_profile("anthropic"),
                    ),
                    forge_connect::Credential::Static(None),
                )
            }),
        )
    }

    /// A child's model is checked against the providers and takes the
    /// catalog's limits; its effort against what that model accepts.
    #[test]
    fn a_child_model_is_validated_against_providers_and_catalog() {
        use forge_core::{ChildModelRequest, ChildModelResolver};
        let dir = tempfile::TempDir::new().unwrap();
        let resolver = child_models(dir.path());
        let parent = ModelExecutionSelection {
            model: "anthropic/claude-opus-4-8".into(),
            profile_id: "anthropic".into(),
            reasoning_effort: Some("medium".into()),
            thinking_enabled: true,
        };
        let resolve = |model: Option<&str>, effort: Option<&str>| {
            resolver.resolve(&ChildModelRequest {
                model,
                reasoning_effort: effort,
                parent: &parent,
            })
        };

        let chosen = resolve(Some("anthropic/claude-opus-4-8"), Some("High")).unwrap();
        assert_eq!(chosen.profile_id, "anthropic");
        assert_eq!(chosen.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(chosen.context_window, Some(1_000_000));
        assert_eq!(chosen.output_reserve, Some(64_000));
        assert_eq!(chosen.image_input, Some(true));

        let default_effort = resolve(Some("anthropic/claude-opus-4-8"), None).unwrap();
        assert_eq!(
            default_effort.reasoning_effort.as_deref(),
            Some("low"),
            "another model starts on its own default"
        );
        let effort_only = resolve(None, Some("xhigh")).unwrap();
        assert_eq!(effort_only.model, "anthropic/claude-opus-4-8");

        let error = resolve(Some("anthropic/claude-opus-4-8"), Some("max")).unwrap_err();
        assert!(
            error.contains("takes reasoning effort low, medium, high, xhigh"),
            "{error}"
        );
        let error = resolve(Some("anthropic/claude-3-haiku"), Some("high")).unwrap_err();
        assert!(error.contains("no adjustable reasoning effort"), "{error}");
        let error = resolve(Some("nope/model"), None).unwrap_err();
        assert!(
            error.contains("no provider serves model `nope/model`"),
            "{error}"
        );
        assert!(error.contains("anthropic"), "{error}");
        let error = resolve(Some("openai-codex/gpt-5"), None).unwrap_err();
        assert!(error.contains("old provider prefix"), "{error}");
    }
}
