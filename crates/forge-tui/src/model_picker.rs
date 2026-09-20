//! Model-picker data and route grouping.

#[derive(Debug, Clone)]
pub struct ModelItem {
    pub provider: String,
    pub model: String,
    pub profile_id: Option<String>,
    pub source: forge_connect::CatalogSource,
    pub route_label: String,
}

#[derive(Debug, Clone)]
pub struct ModelGroup {
    pub model_id: String,
    pub routes: Vec<ModelItem>,
}

pub(crate) fn group_model_items(items: Vec<ModelItem>) -> Vec<ModelGroup> {
    let mut out: Vec<ModelGroup> = Vec::new();
    let mut index: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for item in items {
        let model_id = forge_connect::route_model_id(&item.model).to_string();
        match index.get(&model_id) {
            Some(&i) => out[i].routes.push(item),
            None => {
                index.insert(model_id.clone(), out.len());
                out.push(ModelGroup {
                    model_id,
                    routes: vec![item],
                });
            }
        }
    }
    out
}

pub(crate) fn promote_active_route(groups: &mut [ModelGroup], active_profile_id: Option<&str>) {
    let Some(active) = active_profile_id else {
        return;
    };
    for group in groups {
        if group.routes.len() < 2 {
            continue;
        }
        if let Some(pos) = group
            .routes
            .iter()
            .position(|route| route.profile_id.as_deref() == Some(active))
        {
            group.routes[..=pos].rotate_right(1);
        }
    }
}

pub(crate) fn model_matches_input(model_input: &str, item: &ModelItem) -> bool {
    let needle = model_input.trim().to_ascii_lowercase();
    needle.is_empty()
        || item.model.to_ascii_lowercase().contains(&needle)
        || item.route_label.to_ascii_lowercase().contains(&needle)
}

pub(crate) fn group_matches_input(model_input: &str, group: &ModelGroup) -> bool {
    let needle = model_input.trim().to_ascii_lowercase();
    needle.is_empty()
        || group.model_id.to_ascii_lowercase().contains(&needle)
        || group
            .routes
            .iter()
            .any(|item| model_matches_input(model_input, item))
}
