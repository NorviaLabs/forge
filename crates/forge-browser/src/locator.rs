//! One lookup for every browser Forge starts.
//!
//! Browser control and web check both need the same program. They share a
//! locator, so a search of `PATH` and the application folders happens once
//! and a settings change moves both of them together.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use forge_config::BrowserConfig;

use crate::discovery::{discover, BrowserSearch, DiscoveryError, FoundBrowser};

/// How long a discovery result is reused. Availability is asked on every
/// model request; the programs on disk are not.
const DISCOVERY_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
struct Cache {
    /// The configured executable this result was for. `None` is detection.
    executable: Option<String>,
    at: Instant,
    result: Result<FoundBrowser, DiscoveryError>,
}

/// The browser program for one process's sessions.
#[derive(Debug)]
pub struct BrowserLocator {
    search: BrowserSearch,
    cache: Mutex<Option<Cache>>,
}

impl BrowserLocator {
    pub fn new(search: BrowserSearch) -> Arc<Self> {
        Arc::new(Self {
            search,
            cache: Mutex::new(None),
        })
    }

    /// Look on this machine.
    pub fn host() -> Arc<Self> {
        Self::new(BrowserSearch::host())
    }

    /// The browser `config` would start.
    pub fn resolve(&self, config: &BrowserConfig) -> Result<FoundBrowser, DiscoveryError> {
        let executable = config
            .executable
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(hit) = cache.as_ref() {
            if hit.executable == executable && hit.at.elapsed() < DISCOVERY_TTL {
                return hit.result.clone();
            }
        }
        let result = discover(executable.as_deref(), &self.search);
        *cache = Some(Cache {
            executable,
            at: Instant::now(),
            result: result.clone(),
        });
        result
    }
}
