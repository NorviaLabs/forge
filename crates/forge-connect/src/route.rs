//! Route identity and model-picker projections.
//!
//! Catalog fetching and route identity are deliberately separate concerns:
//! the former deals with freshness and I/O, while this module answers which
//! user-visible model routes exist and how their IDs are interpreted.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::catalog::{CatalogEntry, CatalogSource};

/// One user-facing model, grouped from every [`CatalogEntry`] route that offers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPickerEntry {
    /// Bare model name shared by every route, e.g. `gpt-5.6` (no provider prefix).
    pub model_id: String,
    pub routes: Vec<ModelRoute>,
}

/// One way to reach a [`ModelPickerEntry`]'s model: a specific connect profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRoute {
    /// Bare model name (matches the owning [`ModelPickerEntry::model_id`]).
    pub model_id: String,
    pub profile_id: String,
    /// The `provider/model` string sent downstream to select this route.
    pub display_id: String,
    pub source: CatalogSource,
}

/// Return the model portion of a namespaced catalog ID.
pub fn route_model_id(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id)
}

/// Resolve a persisted profile ID to the stable route ID used by execution.
pub fn route_id_for_profile(profile_id: &str) -> String {
    crate::loaded_registry()
        .get(profile_id)
        .map(|profile| profile.route_id.clone())
        .unwrap_or_else(|| profile_id.to_string())
}

/// Group flat catalog routes by model, preserving every profile's route.
pub fn group_routes(entries: &[CatalogEntry]) -> Vec<ModelPickerEntry> {
    let mut out: Vec<ModelPickerEntry> = Vec::new();
    let mut index: BTreeMap<&str, usize> = BTreeMap::new();

    for entry in entries {
        let model_id = route_model_id(&entry.id);
        let route = ModelRoute {
            model_id: model_id.to_string(),
            profile_id: entry.profile_id.clone(),
            display_id: entry.id.clone(),
            source: entry.source,
        };
        match index.get(model_id) {
            Some(&i) => out[i].routes.push(route),
            None => {
                index.insert(model_id, out.len());
                out.push(ModelPickerEntry {
                    model_id: model_id.to_string(),
                    routes: vec![route],
                });
            }
        }
    }
    out
}

/// Normalize a `/model` argument into a provider/model string.
pub fn normalize_model_id(
    first: &str,
    second: Option<&str>,
    default_prefix: Option<&str>,
) -> String {
    let first = first.trim();
    let second = second.map(str::trim).filter(|value| !value.is_empty());
    if let Some(second) = second {
        if first.contains('/') {
            first.to_string()
        } else {
            format!("{first}/{second}")
        }
    } else if first.contains('/') {
        first.to_string()
    } else if let Some(prefix) = default_prefix.filter(|value| !value.is_empty()) {
        format!("{prefix}/{first}")
    } else {
        first.to_string()
    }
}
