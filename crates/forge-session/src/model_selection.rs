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
}

impl CatalogChildModels {
    /// Routes are read on every call, so a provider connected after the
    /// session started is accepted at once.
    pub(crate) fn new(cfg: &Config) -> Self {
        Self {
            catalog_path: cfg.model.catalog_path.clone(),
            routes: Box::new(crate::route_table),
        }
    }

    fn catalog(&self) -> forge_connect::ModelCatalogCache {
        match &self.catalog_path {
            Some(path) => forge_connect::ModelCatalogCache::new(path.clone()),
            None => forge_connect::ModelCatalogCache::user_default(),
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
        let catalog = self.catalog();
        let support = forge_connect::effort::effort(&found.definition, bare, &catalog);
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
        if let Some(limits) = catalog
            .model_limits(&model)
            .filter(|limits| limits.context > 0)
        {
            child.context_window = Some(limits.context);
            child.output_reserve = (limits.output > 0).then_some(limits.output);
        }
        child.image_input = Some(catalog.model_accepts_image_input(&model));
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
        CatalogChildModels {
            catalog_path: Some(catalog),
            routes: Box::new(|| {
                forge_model::RouteTable::single(
                    forge_connect::RouteDefinition::from_spec(
                        &forge_connect::builtin::builtin_profile("anthropic"),
                    ),
                    forge_connect::Credential::Static(None),
                )
            }),
        }
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
