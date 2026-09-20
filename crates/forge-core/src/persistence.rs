//! Session-scoped persistence boundary.
//!
//! Keeping the durable journal behind this small adapter gives the agent
//! session one place to gain replay/migration policy without exposing the
//! storage implementation throughout its public shape.

use std::ops::{Deref, DerefMut};

use forge_durable::Journal;

pub(crate) struct SessionPersistence {
    journal: Journal,
}

impl SessionPersistence {
    pub(crate) fn new(journal: Journal) -> Self {
        Self { journal }
    }
}

impl Deref for SessionPersistence {
    type Target = Journal;

    fn deref(&self) -> &Self::Target {
        &self.journal
    }
}

impl DerefMut for SessionPersistence {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.journal
    }
}

#[cfg(test)]
mod tests {
    use super::SessionPersistence;
    use forge_durable::{new_session_id, Journal};

    #[tokio::test]
    async fn persistence_forwards_journal_access() {
        let dir = tempfile::tempdir().unwrap();
        let session_id = new_session_id();
        let journal = Journal::open(dir.path(), session_id).await.unwrap();
        let mut persistence = SessionPersistence::new(journal);
        let _ = &mut *persistence;
        persistence
            .append_session_created(session_id)
            .await
            .unwrap();
        let state = persistence.replay(session_id).await.unwrap();
        assert_eq!(state.session_id, session_id);
    }
}
