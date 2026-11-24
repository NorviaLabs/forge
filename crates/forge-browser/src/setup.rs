//! Startup shared by the agent's browser and the web-check browser.
//!
//! Both start a Chromium process and then need a page target attached over
//! the flattened DevTools session. What they do with that session stays
//! theirs: the agent browser follows the session gateway, and the checker
//! reaches only the app under test.

use serde_json::{json, Value};

use crate::cdp::{CdpConnection, CdpError};

/// Page target ids, in the browser's order.
pub(crate) async fn page_target_ids(cdp: &CdpConnection) -> Result<Vec<String>, CdpError> {
    let targets = cdp.call("Target.getTargets", json!({})).await?;
    Ok(page_ids(&targets))
}

fn page_ids(targets: &Value) -> Vec<String> {
    targets["targetInfos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|info| info["type"] == "page")
        .filter_map(|info| info["targetId"].as_str().map(str::to_string))
        .collect()
}

/// The first page, or a new `about:blank` when the browser opened with none.
pub(crate) async fn ensure_page_target(cdp: &CdpConnection) -> Result<String, CdpError> {
    if let Some(target) = page_target_ids(cdp).await?.into_iter().next() {
        return Ok(target);
    }
    let created = cdp
        .call("Target.createTarget", json!({"url": "about:blank"}))
        .await?;
    Ok(created["targetId"].as_str().unwrap_or_default().to_string())
}

/// Attach to `target_id` and return the flattened session id.
pub(crate) async fn attach_target(
    cdp: &CdpConnection,
    target_id: &str,
) -> Result<String, CdpError> {
    let attached = cdp
        .call(
            "Target.attachToTarget",
            json!({"targetId": target_id, "flatten": true}),
        )
        .await?;
    Ok(attached["sessionId"]
        .as_str()
        .unwrap_or_default()
        .to_string())
}
