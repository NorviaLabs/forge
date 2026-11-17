//! A remote service on a scratch folder with a fake backend, and a client
//! that pins its certificate the way a phone does.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forge_remote::hub::HubConfig;
use forge_remote::testing::{FakeBackend, TestClient};
use forge_remote::tls::{Fingerprint, CERT_FILE};
use forge_remote::{Limits, ManualClock, Reach, RemoteService, RemoteSettings, ServiceOptions};

pub struct Harness {
    pub temp: tempfile::TempDir,
    pub clock: Arc<ManualClock>,
    pub backend: Arc<FakeBackend>,
    pub service: RemoteService,
}

impl Harness {
    pub fn new() -> Self {
        Self::with(Limits::default(), HubConfig::default())
    }

    pub fn with(limits: Limits, hub: HubConfig) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::new());
        let backend = FakeBackend::new();
        let service = RemoteService::new(ServiceOptions {
            dir: temp.path().join("remote"),
            backend: backend.clone(),
            clock: clock.clone(),
            limits,
            hub,
        });
        Self {
            temp,
            clock,
            backend,
            service,
        }
    }

    pub fn dir(&self) -> PathBuf {
        self.temp.path().join("remote")
    }

    /// Settings that listen on loopback, on a free port.
    pub fn on() -> RemoteSettings {
        RemoteSettings {
            enabled: true,
            reach: Reach::Loopback,
            address: None,
            port: 0,
        }
    }

    /// The pin a device would take from the pairing link.
    pub fn pin(&self) -> Fingerprint {
        Fingerprint::of_der(&std::fs::read(self.dir().join(CERT_FILE)).unwrap())
    }

    /// Turn the service on; its address and a pinned client.
    pub async fn enable(&self) -> (SocketAddr, TestClient) {
        let status = self.service.configure(Self::on()).await.unwrap();
        let addr = status.listening.expect("the service listens");
        self.backend.wait_subscribed().await;
        (addr, TestClient::new(addr, self.pin()))
    }

    /// Pair a device through the API: redeem a code, allow it on the
    /// "desktop", collect the token.
    pub async fn pair(&self, client: &TestClient, name: &str) -> String {
        let info = self.service.start_pairing().unwrap();
        let started = client
            .pair(&info.code, name)
            .await
            .unwrap_or_else(|reply| panic!("pairing failed: {} {}", reply.status, reply.text));
        self.allow_and_collect(client, &started).await
    }

    /// Allow a started request on the desktop and collect its token.
    pub async fn allow_and_collect(
        &self,
        client: &TestClient,
        started: &forge_remote::protocol::PairStarted,
    ) -> String {
        self.service
            .decide_pairing(&started.confirmation, true)
            .await
            .expect("the desktop allows it");
        client
            .token(&started.pairing_id)
            .await
            .unwrap_or_else(|reply| panic!("collecting failed: {} {}", reply.status, reply.text))
    }

    /// Enabled, with one device paired: the client and its token.
    pub async fn paired(&self) -> (SocketAddr, TestClient, String) {
        let (addr, client) = self.enable().await;
        let token = self.pair(&client, "Phone").await;
        (addr, client, token)
    }

    /// Every line of the activity log, parsed.
    pub fn activity(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.dir().join("activity.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

/// Poll `check` until it holds or five seconds pass.
pub async fn eventually(mut check: impl FnMut() -> bool) -> bool {
    for _ in 0..500 {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}
