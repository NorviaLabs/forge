//! The ranking both advisors read (`forge_session::model_ranking`): where
//! it comes from, and how it is kept. Its old home, `subagent_models`,
//! still names the same items.

use std::sync::Arc;

use forge_session::model_ranking::{
    JournalRanking, RankingSource, RunnableModels, UnknownRunnableModels, FULL_REBUILD_EVERY,
    MIN_KIND_TURNS, MIN_RANKED_GROUPS, STALE_AFTER, WINDOW_DAYS,
};
use forge_types::TaskKind;

/// A directory with no journals ranks nothing, and reading it creates
/// nothing.
#[tokio::test]
async fn an_empty_directory_builds_an_empty_ranking() {
    let dir = tempfile::tempdir().unwrap();
    let journals = dir.path().join("sessions");
    let source = JournalRanking::new(journals.clone());
    let snapshot = source.build().await.unwrap();
    assert!(snapshot.rank(&TaskKind::Main, |_| true, 1).is_none());
    assert!(!journals.exists());
    assert_eq!(source.journals_read(), 0);
}

/// The old paths are the same items, not copies.
#[test]
fn the_old_paths_name_the_same_items() {
    use forge_session::subagent_models as old;
    assert_eq!(old::WINDOW_DAYS, WINDOW_DAYS);
    assert_eq!(old::MIN_KIND_TURNS, MIN_KIND_TURNS);
    assert_eq!(old::MIN_RANKED_GROUPS, MIN_RANKED_GROUPS);
    assert_eq!(old::STALE_AFTER, STALE_AFTER);
    assert_eq!(old::FULL_REBUILD_EVERY, FULL_REBUILD_EVERY);
    let dir = tempfile::tempdir().unwrap();
    let _: Arc<dyn RankingSource> = Arc::new(old::JournalRanking::new(dir.path().into()));
    let _: Arc<dyn old::RankingSource> = Arc::new(JournalRanking::new(dir.path().into()));
    let _: Arc<dyn RunnableModels> = Arc::new(old::UnknownRunnableModels);
    let _: Arc<dyn old::RunnableModels> = Arc::new(UnknownRunnableModels);
}
