//! Remote control in the desktop: the `forge-remote` service over this
//! window's `ProjectService`, the Settings commands that drive it, and the
//! status the page shows.
//!
//! The service starts with the window and listens only when the saved
//! settings say so. Self-test builds never start it.

use std::net::IpAddr;
use std::path::PathBuf;

use forge_remote::{PairingInfo, Reach, RemoteService, RemoteSettings, RemoteStatus};
use serde::Deserialize;

/// The remote service, managed alongside [`crate::bridge::DesktopState`].
pub struct RemoteState {
    pub service: RemoteService,
}

/// `<Forge data dir>/remote`.
pub fn remote_dir() -> Option<PathBuf> {
    forge_storage::forge_data_dir().map(|dir| dir.join(forge_remote::REMOTE_DIR))
}

/// What Settings sends: every field as the page holds it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RemoteSettingsInput {
    pub enabled: bool,
    /// `loopback` or `lan`.
    pub reach: String,
    /// The LAN address; empty for none.
    #[serde(default)]
    pub address: String,
    pub port: u16,
}

/// Check the page's input. Ports below 1024 and an unparsable address are
/// refused with a sentence the page can show.
pub fn settings_from_input(input: &RemoteSettingsInput) -> Result<RemoteSettings, String> {
    let reach = reach(&input.reach)
        .ok_or_else(|| format!("unknown reach `{}`", input.reach.escape_debug()))?;
    if input.port < 1024 {
        return Err("choose a port from 1024 to 65535".into());
    }
    let address = match input.address.trim() {
        "" => None,
        text => Some(
            parse_address(text)
                .ok_or_else(|| format!("`{}` is not an IP address", text.escape_debug()))?,
        ),
    };
    if input.enabled && reach == Reach::Lan && address.is_none() {
        return Err("choose this computer's address on the network".into());
    }
    Ok(RemoteSettings {
        enabled: input.enabled,
        reach,
        address,
        port: input.port,
    })
}

fn parse_address(text: &str) -> Option<IpAddr> {
    text.trim().parse().ok()
}

fn reach(text: &str) -> Option<Reach> {
    match text {
        "loopback" => Some(Reach::Loopback),
        "lan" => Some(Reach::Lan),
        _ => None,
    }
}

/// Start the service for `projects`, listening if it was left on.
pub fn start(projects: forge_session::ProjectServiceHandle) -> Option<RemoteState> {
    let dir = remote_dir()?;
    let service = RemoteService::new(forge_remote::ServiceOptions {
        dir,
        backend: forge_remote::SupervisorBackend::new(projects),
        clock: std::sync::Arc::new(forge_remote::SystemClock),
        limits: forge_remote::Limits::default(),
        hub: forge_remote::hub::HubConfig::default(),
    });
    let starting = service.clone();
    tauri::async_runtime::spawn(async move {
        starting.start_if_enabled().await;
    });
    Some(RemoteState { service })
}

/// Manage the remote service for a window that just got its projects, once.
pub fn manage(app: &tauri::AppHandle, projects: &forge_session::ProjectServiceHandle) {
    use tauri::Manager;
    if app.try_state::<RemoteState>().is_none() {
        if let Some(state) = start(projects.clone()) {
            app.manage(state);
        }
    }
}

/// Send every remote status change to the page until it goes away.
pub async fn forward_status(
    channel: tauri::ipc::Channel<crate::bridge::DesktopEvent>,
    mut status: tokio::sync::watch::Receiver<RemoteStatus>,
) {
    loop {
        let current = status.borrow_and_update().clone();
        let event = crate::bridge::DesktopEvent::Remote {
            status: Box::new(current),
        };
        if channel.send(event).is_err() || status.changed().await.is_err() {
            return;
        }
    }
}

fn service(app: &tauri::AppHandle) -> Result<RemoteService, String> {
    use tauri::Manager;
    app.try_state::<RemoteState>()
        .map(|state| state.service.clone())
        .ok_or_else(|| "remote control is not available in this window".to_string())
}

// The Settings commands. `bridge` registers them, as it does every command.

pub async fn status(app: &tauri::AppHandle) -> Result<RemoteStatus, String> {
    Ok(service(app)?.status())
}

pub async fn configure(
    app: &tauri::AppHandle,
    settings: RemoteSettingsInput,
) -> Result<RemoteStatus, String> {
    let settings = settings_from_input(&settings)?;
    service(app)?
        .configure(settings)
        .await
        .map_err(|error| error.to_string())
}

pub async fn pair_start(app: &tauri::AppHandle) -> Result<PairingInfo, String> {
    service(app)?
        .start_pairing()
        .map_err(|error| error.to_string())
}

/// Allow or refuse the device waiting to pair; `confirmation` is the number
/// the page showed, so a replaced request is never decided.
pub async fn pair_decide(
    app: &tauri::AppHandle,
    confirmation: String,
    allow: bool,
) -> Result<RemoteStatus, String> {
    service(app)?
        .decide_pairing(&confirmation, allow)
        .await
        .map_err(|error| error.to_string())
}

pub async fn pair_cancel(app: &tauri::AppHandle) -> Result<(), String> {
    service(app)?.cancel_pairing();
    Ok(())
}

pub async fn revoke(app: &tauri::AppHandle) -> Result<(), String> {
    service(app)?
        .revoke()
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(enabled: bool, reach: &str, address: &str, port: u16) -> RemoteSettingsInput {
        RemoteSettingsInput {
            enabled,
            reach: reach.into(),
            address: address.into(),
            port,
        }
    }

    #[test]
    fn the_remote_folder_is_inside_forges_data_directory() {
        let dir = remote_dir().expect("a data directory");
        assert!(dir.ends_with(forge_remote::REMOTE_DIR));
        assert_eq!(dir.parent(), forge_storage::forge_data_dir().as_deref());
    }

    #[test]
    fn settings_from_the_page_are_checked() {
        assert_eq!(
            settings_from_input(&input(true, "loopback", "", 47390)).unwrap(),
            RemoteSettings {
                enabled: true,
                reach: Reach::Loopback,
                address: None,
                port: 47390
            }
        );
        assert_eq!(
            settings_from_input(&input(true, "lan", " 192.168.1.20 ", 47400))
                .unwrap()
                .address,
            Some("192.168.1.20".parse().unwrap())
        );
        for (bad, why) in [
            (input(true, "lan", "not an address", 47390), "address"),
            (input(true, "everywhere", "", 47390), "reach"),
            (input(true, "loopback", "", 0), "port"),
            (input(true, "loopback", "", 1023), "port"),
        ] {
            let error = settings_from_input(&bad).unwrap_err();
            assert!(error.to_lowercase().contains(why), "{bad:?}: {error}");
        }
        assert!(
            settings_from_input(&input(false, "lan", "", 47390)).is_ok(),
            "turning it off never needs an address"
        );
    }

    #[test]
    fn a_status_event_reaches_the_page_tagged() {
        let status = RemoteStatus {
            enabled: true,
            reach: Reach::Loopback,
            address: None,
            port: 47390,
            listening: Some("127.0.0.1:47390".parse().unwrap()),
            error: None,
            fingerprint: Some("3FA2".into()),
            device: None,
            connected: false,
            pairing_expires_at: None,
            pairing_error: None,
            pairing_request: None,
            suggested_address: None,
        };
        let event = serde_json::to_value(crate::bridge::DesktopEvent::Remote {
            status: Box::new(status),
        })
        .unwrap();
        assert_eq!(event["type"], "remote");
        assert_eq!(event["status"]["listening"], "127.0.0.1:47390");
        assert_eq!(event["status"]["reach"], "loopback");
    }

    #[test]
    fn the_remote_commands_are_allowlisted() {
        for command in [
            "remote_status",
            "remote_configure",
            "remote_pair_start",
            "remote_pair_cancel",
            "remote_revoke",
            "remote_pair_decide",
        ] {
            assert!(
                crate::commands_list::DESKTOP_COMMANDS.contains(&command),
                "{command} must be callable from the page"
            );
        }
    }
}
