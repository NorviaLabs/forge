//! What the user chose in Settings: on or off, where the service is
//! reachable from, and its port. Kept in `remote.toml`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::fsutil::write_private;

pub const SETTINGS_FILE: &str = "remote.toml";
pub const DEFAULT_PORT: u16 = 47390;

/// Where a device can reach the service from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    /// `127.0.0.1` only. A phone needs its own tunnel.
    #[default]
    Loopback,
    /// One address of this computer on the local network. Never every
    /// interface.
    Lan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteSettings {
    /// Off until the user turns it on.
    pub enabled: bool,
    pub reach: Reach,
    /// The LAN address to listen on; ignored for [`Reach::Loopback`].
    pub address: Option<IpAddr>,
    /// `0` picks a free port (tests); the desktop offers 1024–65535.
    pub port: u16,
}

impl Default for RemoteSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            reach: Reach::Loopback,
            address: None,
            port: DEFAULT_PORT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("cannot read remote control settings: {0}")]
    Io(String),
    #[error("{0}")]
    Invalid(String),
}

impl RemoteSettings {
    /// `dir/remote.toml`, or the defaults when there is none. A file that
    /// does not parse is an error: the caller keeps the service off.
    pub fn load(dir: &Path) -> Result<Self, SettingsError> {
        match std::fs::read_to_string(dir.join(SETTINGS_FILE)) {
            Ok(text) => toml::from_str(&text).map_err(|error| {
                SettingsError::Invalid(format!(
                    "remote control settings are damaged, so it stays off: {}",
                    error.message()
                ))
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(SettingsError::Io(error.to_string())),
        }
    }

    pub fn save(&self, dir: &Path) -> Result<(), SettingsError> {
        let text = toml::to_string(self).map_err(|e| SettingsError::Io(e.to_string()))?;
        write_private(&dir.join(SETTINGS_FILE), text.as_bytes())
            .map_err(|e| SettingsError::Io(e.to_string()))
    }

    /// The one address to listen on. A LAN address must be a specific
    /// unicast address: never unspecified (`0.0.0.0`, `::`), multicast or
    /// broadcast. Ports below 1024 are refused (0 picks a free one).
    pub fn bind_addr(&self) -> Result<SocketAddr, SettingsError> {
        if self.port != 0 && self.port < 1024 {
            return Err(SettingsError::Invalid(format!(
                "port {} is reserved; choose one from 1024 to 65535",
                self.port
            )));
        }
        let ip = match self.reach {
            Reach::Loopback => LOOPBACK,
            Reach::Lan => {
                let ip = self.address.ok_or_else(|| {
                    SettingsError::Invalid("choose this computer's address on the network".into())
                })?;
                if !is_one_unicast_address(ip) {
                    return Err(SettingsError::Invalid(format!(
                        "{ip} is not one address of this computer"
                    )));
                }
                if !is_local_network(ip) {
                    return Err(SettingsError::Invalid(format!(
                        "{ip} is reachable from the internet; choose a local-network \
                         address (192.168.x.x, 10.x.x.x, 172.16–31.x.x, or a Tailscale \
                         100.x address)"
                    )));
                }
                ip
            }
        };
        Ok(SocketAddr::new(ip, self.port))
    }
}

/// This computer's address on its primary network, found by asking the OS
/// which local address would route to a public one. A UDP `connect` sends
/// nothing. `None` without a network.
pub fn detect_lan_address() -> Option<IpAddr> {
    // TEST-NET-1 (RFC 5737): routable in form, never answered, and a UDP
    // connect only picks a route.
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (is_one_unicast_address(ip) && !ip.is_loopback() && is_local_network(ip)).then_some(ip)
}

/// Addresses a local network (or a private overlay such as Tailscale) uses,
/// never a globally routable one: RFC 1918, shared address space
/// (100.64.0.0/10), link-local, unique local (fc00::/7) and loopback.
fn is_local_network(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_private()
                || v4.is_link_local()
                || v4.is_loopback()
                || (a == 100 && (b & 0b1100_0000) == 0b0100_0000)
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// A specific address a socket can be bound to: not every interface, not a
/// group, not broadcast.
fn is_one_unicast_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !(v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast()),
        IpAddr::V6(v6) => !(v6.is_unspecified() || v6.is_multicast()),
    }
}

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_control_is_off_and_local_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let settings = RemoteSettings::load(dir.path()).unwrap();
        assert_eq!(settings, RemoteSettings::default());
        assert!(!settings.enabled);
        assert_eq!(settings.reach, Reach::Loopback);
        assert_eq!(settings.port, DEFAULT_PORT);
    }

    #[test]
    fn settings_round_trip_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let settings = RemoteSettings {
            enabled: true,
            reach: Reach::Lan,
            address: Some("192.168.1.20".parse().unwrap()),
            port: 47400,
        };
        settings.save(dir.path()).unwrap();
        assert_eq!(RemoteSettings::load(dir.path()).unwrap(), settings);
    }

    #[test]
    fn a_damaged_file_is_an_error_not_a_silent_default() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SETTINGS_FILE), "enabled = maybe").unwrap();
        assert!(matches!(
            RemoteSettings::load(dir.path()),
            Err(SettingsError::Invalid(_))
        ));
    }

    #[test]
    fn loopback_listens_on_127_0_0_1_whatever_the_address_says() {
        let settings = RemoteSettings {
            address: Some("192.168.1.20".parse().unwrap()),
            ..RemoteSettings::default()
        };
        assert_eq!(
            settings.bind_addr().unwrap(),
            SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT))
        );
    }

    #[test]
    fn a_lan_address_must_be_one_specific_address() {
        let lan = |address: Option<&str>| RemoteSettings {
            enabled: true,
            reach: Reach::Lan,
            address: address.map(|a| a.parse().unwrap()),
            port: DEFAULT_PORT,
        };
        assert_eq!(
            lan(Some("192.168.1.20")).bind_addr().unwrap(),
            "192.168.1.20:47390".parse().unwrap()
        );
        assert!(lan(Some("fe80::1")).bind_addr().is_ok());
        for refused in [
            None,
            Some("0.0.0.0"),
            Some("::"),
            Some("224.0.0.1"),
            Some("255.255.255.255"),
            Some("ff02::1"),
        ] {
            assert!(
                matches!(lan(refused).bind_addr(), Err(SettingsError::Invalid(_))),
                "{refused:?} must be refused"
            );
        }
    }

    #[test]
    fn a_lan_address_must_be_private_never_on_the_internet() {
        let lan = |address: &str| RemoteSettings {
            enabled: true,
            reach: Reach::Lan,
            address: Some(address.parse().unwrap()),
            port: DEFAULT_PORT,
        };
        for private in [
            "10.0.0.5",
            "172.16.4.2",
            "172.31.255.1",
            "192.168.1.20",
            "100.64.0.7",
            "100.127.255.254",
            "169.254.10.10",
            "fd12:3456:789a::1",
            "fe80::1",
        ] {
            assert!(
                lan(private).bind_addr().is_ok(),
                "{private} is a local-network address"
            );
        }
        for public in [
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "203.0.113.9",
            "2001:4860:4860::8888",
        ] {
            match lan(public).bind_addr() {
                Err(SettingsError::Invalid(why)) => {
                    assert!(why.contains("internet"), "{public}: {why}")
                }
                other => panic!("{public} must be refused: {other:?}"),
            }
        }
    }

    #[test]
    fn privileged_ports_are_refused_and_zero_picks_one() {
        let with_port = |port| RemoteSettings {
            port,
            ..RemoteSettings::default()
        };
        assert!(with_port(80).bind_addr().is_err());
        assert!(with_port(1023).bind_addr().is_err());
        assert_eq!(with_port(1024).bind_addr().unwrap().port(), 1024);
        assert_eq!(with_port(0).bind_addr().unwrap().port(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn the_settings_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        RemoteSettings::default().save(dir.path()).unwrap();
        let mode = std::fs::metadata(dir.path().join(SETTINGS_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_detected_lan_address_is_never_loopback_or_unspecified() {
        if let Some(address) = detect_lan_address() {
            assert!(!address.is_loopback(), "{address}");
            assert!(!address.is_unspecified(), "{address}");
        }
    }
}
