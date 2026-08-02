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
