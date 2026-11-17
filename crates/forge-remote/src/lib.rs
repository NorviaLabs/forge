//! Remote control: one paired device follows Forge sessions live, sends a
//! prompt, stops a turn, and allows or denies a pending request.
//!
//! The service is off until the user turns it on; while off nothing listens.
//! When on, it serves a small HTTPS API (TLS 1.3, a self-signed certificate
//! the device pins by fingerprint) on loopback or one chosen LAN address.
//!
//! * [`pairing`]: a one-time code, held only in memory, that expires.
//! * [`store`]: the one paired device; its bearer token is stored only as a
//!   SHA-256 hash ([`token`]).
//! * [`hub`]: per-session event logs with contiguous sequence numbers, so a
//!   device sees events in order and resumes without a silent gap.
//! * [`service`]: the listener's lifecycle, and the status the desktop shows.
//! * [`backend`]: what the server needs from Forge; [`supervisor`]
//!   implements it over the desktop's `ProjectService`.
//!
//! The wire format is [`protocol`] (version 1).

pub mod activity;
pub mod backend;
pub mod clock;
mod fsutil;
mod http;
pub mod hub;
pub mod pairing;
pub mod protocol;
pub mod service;
pub mod settings;
pub mod store;
pub mod supervisor;
pub mod tls;
pub mod token;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use backend::{BackendError, BackendEvent, Interest, RemoteBackend};
pub use clock::{Clock, ManualClock, SystemClock};
pub use service::{Limits, PairingInfo, RemoteError, RemoteService, RemoteStatus, ServiceOptions};
pub use settings::{Reach, RemoteSettings, DEFAULT_PORT};
pub use supervisor::SupervisorBackend;

/// The folder remote control keeps its files in, inside Forge's data
/// directory: settings, the paired device's record, the TLS identity and the
/// activity log.
pub const REMOTE_DIR: &str = "remote";
