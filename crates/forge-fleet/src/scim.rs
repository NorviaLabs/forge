use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScimError {
    #[error("user not found: {0}")]
    UserNotFound(String),
    #[error("group not found: {0}")]
    GroupNotFound(String),
    #[error("user already exists: {0}")]
    UserExists(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScimUser {
    pub id: String,
    pub user_name: String,
    pub active: bool,
    #[serde(default)]
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScimGroup {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub members: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryScimStore {
    users: HashMap<String, ScimUser>,
    groups: HashMap<String, ScimGroup>,
}

/// SCIM subset: Users + Groups provisioning for ACL principals.
pub struct ScimPlugin {
    store: InMemoryScimStore,
}

impl ScimPlugin {
    pub fn new(store: InMemoryScimStore) -> Self {
        Self { store }
    }

    pub fn create_user(&mut self, user: ScimUser) -> Result<ScimUser, ScimError> {
        if self.store.users.contains_key(&user.id) {
            return Err(ScimError::UserExists(user.id));
        }
        self.store.users.insert(user.id.clone(), user.clone());
        Ok(user)
    }

    pub fn get_user(&self, id: &str) -> Result<&ScimUser, ScimError> {
        self.store
            .users
            .get(id)
            .ok_or_else(|| ScimError::UserNotFound(id.into()))
    }

    pub fn deactivate_user(&mut self, id: &str) -> Result<(), ScimError> {
        let u = self
            .store
            .users
            .get_mut(id)
            .ok_or_else(|| ScimError::UserNotFound(id.into()))?;
        u.active = false;
        Ok(())
    }

    pub fn delete_user(&mut self, id: &str) -> Result<(), ScimError> {
        self.store
            .users
            .remove(id)
            .ok_or_else(|| ScimError::UserNotFound(id.into()))?;
        for g in self.store.groups.values_mut() {
            g.members.retain(|m| m != id);
        }
        Ok(())
    }

    pub fn add_user_to_group(
        &mut self,
        group_id: &str,
        display_name: &str,
        user_id: &str,
    ) -> Result<(), ScimError> {
        if !self.store.users.contains_key(user_id) {
            return Err(ScimError::UserNotFound(user_id.into()));
        }
        let g = self
            .store
            .groups
            .entry(group_id.to_string())
            .or_insert_with(|| ScimGroup {
                id: group_id.into(),
                display_name: display_name.into(),
                members: vec![],
            });
        if !g.members.iter().any(|m| m == user_id) {
            g.members.push(user_id.into());
        }
        Ok(())
    }

    pub fn get_group(&self, id: &str) -> Result<&ScimGroup, ScimError> {
        self.store
            .groups
            .get(id)
            .ok_or_else(|| ScimError::GroupNotFound(id.into()))
    }

    /// Map SCIM user → roles for ACL principal binding.
    pub fn roles_for_user(&self, id: &str) -> Vec<String> {
        self.store
            .users
            .get(id)
            .map(|u| u.roles.clone())
            .unwrap_or_default()
    }
}
