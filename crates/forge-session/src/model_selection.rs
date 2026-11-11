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
}
