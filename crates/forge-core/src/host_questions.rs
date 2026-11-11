//! Hosts the egress gateway is holding a connection for, waiting on the user.
//!
//! When a confined command reaches a host no rule decides, the gateway does
//! not refuse it: it holds the connection and sends the question here. The
//! command is still running, and the session that started it is busy running
//! it, so the questions live outside the session, in a handle the owner (the
//! repository supervisor) can read and answer without the session lock.
//! Allowing the host lets the held connection through, and the command
//! carries on; nothing is run twice.
//!
//! Nothing here is durable. A held connection belongs to a running process,
//! and neither survives a restart. A question nobody answers times out at
//! the gateway, which refuses the connection the way it always did, and the
//! ordinary approval for the failed command follows.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use forge_tools::egress::{EgressShared, HostAnswer, HostAsk, HostAsker};
use tokio::sync::{mpsc, oneshot, watch};

use crate::audit::SessionAudit;

/// One host a running command is waiting to reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostQuestion {
    pub host: String,
    pub port: u16,
    /// The tool call (or MCP server) that opened the first held connection.
    pub call_id: Option<String>,
    /// Set when the question is not about reaching a host but about
    /// changing something on it as the user: what is being done. `host` is
    /// then the target (`host/owner/repo`, or the host).
    pub write: Option<String>,
}

/// What the user chose for a held host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostDecision {
    /// Let the command that asked reach it; ask again next time.
    AllowOnce,
    /// Allow it until the session ends.
    AllowForSession,
    /// Allow it for this project from now on.
    AllowAlways,
    Deny,
}

/// The outcome of [`HostQuestions::answer`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostAnswered {
    /// `AllowAlways` only: why the host could not be saved for the project.
    /// It is still allowed for this session.
    pub persist_error: Option<String>,
}

struct Pending {
    question: HostQuestion,
    waiters: Vec<oneshot::Sender<HostAnswer>>,
}

struct Inner {
    asker: HostAsker,
    shared: Mutex<EgressShared>,
    pending: Mutex<Vec<Pending>>,
    changed: watch::Sender<u64>,
    approve_all: Arc<AtomicBool>,
    audit: Arc<SessionAudit>,
    workspace: PathBuf,
}

/// The held hosts of one session. Cheap to clone; every clone is the same
/// set.
#[derive(Clone)]
pub struct HostQuestions {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for HostQuestions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostQuestions")
            .field("pending", &self.pending())
            .finish()
    }
}

impl HostQuestions {
    /// Start receiving the questions of every endpoint on `shared`.
    pub(crate) fn start(
        shared: EgressShared,
        approve_all: Arc<AtomicBool>,
        audit: Arc<SessionAudit>,
        workspace: PathBuf,
    ) -> Self {
        let (asker, asks) = mpsc::unbounded_channel();
        let (changed, _) = watch::channel(0);
        shared.set_asker(Some(asker.clone()));
        let inner = Arc::new(Inner {
            asker,
            shared: Mutex::new(shared),
            pending: Mutex::new(Vec::new()),
            changed,
            approve_all,
            audit,
            workspace,
        });
        tokio::spawn(receive(Arc::downgrade(&inner), asks));
        Self { inner }
    }

    /// Move to `shared` after the session replaced its egress state.
    pub(crate) fn attach(&self, shared: EgressShared) {
        shared.set_asker(Some(self.inner.asker.clone()));
        *lock(&self.inner.shared) = shared;
    }

    /// The hosts waiting for a decision, oldest first.
    pub fn pending(&self) -> Vec<HostQuestion> {
        lock(&self.inner.pending)
            .iter()
            .map(|pending| pending.question.clone())
            .collect()
    }

    /// Changes whenever [`Self::pending`] does.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.inner.changed.subscribe()
    }

    /// Decide `host`. Every connection held for it is released with the
    /// answer. `Err` when the host is no longer waiting (answered already,
    /// or the command gave up).
    pub fn answer(&self, host: &str, decision: HostDecision) -> Result<HostAnswered, String> {
        let pending = {
            let mut all = lock(&self.inner.pending);
            let index = all
                .iter()
                .position(|pending| pending.question.host == host)
                .ok_or_else(|| format!("{host} is no longer waiting for a decision"))?;
            all.remove(index)
        };
        self.inner.changed.send_modify(|version| *version += 1);

        let call_id = pending.question.call_id.as_deref();
        let mut answered = HostAnswered::default();
        if pending.question.write.is_some() {
            // A write target, not a host: recorded in its own list.
            let reply = match decision {
                HostDecision::Deny => HostAnswer::Deny,
                HostDecision::AllowOnce => HostAnswer::AllowOnce,
                HostDecision::AllowForSession => {
                    lock(&self.inner.shared).grant_write(host);
                    HostAnswer::Allow
                }
                HostDecision::AllowAlways => {
                    answered.persist_error =
                        forge_config::append_project_write(&self.inner.workspace, host)
                            .err()
                            .map(|error| error.to_string());
                    lock(&self.inner.shared).grant_write(host);
                    HostAnswer::Allow
                }
            };
            for waiter in pending.waiters {
                let _ = waiter.send(reply);
            }
            return Ok(answered);
        }
        let reply = match decision {
            HostDecision::Deny => HostAnswer::Deny,
            HostDecision::AllowOnce => HostAnswer::AllowOnce,
            HostDecision::AllowForSession => {
                lock(&self.inner.shared).grant_host(host);
                self.inner
                    .audit
                    .queue_host_grant(call_id, host, None, "held_connection");
                HostAnswer::Allow
            }
            HostDecision::AllowAlways => {
                answered.persist_error =
                    forge_config::append_project_host(&self.inner.workspace, host)
                        .err()
                        .map(|error| error.to_string());
                lock(&self.inner.shared).grant_host(host);
                self.inner.audit.queue_host_grant(
                    call_id,
                    host,
                    Some((&format!("host({host})"), answered.persist_error.as_deref())),
                    "held_connection",
                );
                HostAnswer::Allow
            }
        };
        for waiter in pending.waiters {
            let _ = waiter.send(reply);
        }
        Ok(answered)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Collect asks into the pending set, one entry per host, and drop entries
/// whose every connection has gone (the gateway timed out, or the command
/// was stopped). Ends when the last handle is dropped.
async fn receive(inner: Weak<Inner>, mut asks: mpsc::UnboundedReceiver<HostAsk>) {
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        let ask = tokio::select! {
            ask = asks.recv() => ask,
            _ = tick.tick() => None,
        };
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let mut changed = false;
        {
            let mut pending = lock(&inner.pending);
            if let Some(ask) = ask {
                // Approve-all never asks. Refusing here leaves the command to
                // the path it already had: it fails, and the session grants
                // the host and runs it again.
                if inner.approve_all.load(Ordering::Acquire) {
                    let _ = ask.reply.send(HostAnswer::Deny);
                } else if let Some(existing) =
                    pending.iter_mut().find(|p| p.question.host == ask.host)
                {
                    existing.waiters.push(ask.reply);
                } else {
                    pending.push(Pending {
                        question: HostQuestion {
                            host: ask.host,
                            port: ask.port,
                            call_id: ask.label,
                            write: ask.write,
                        },
                        waiters: vec![ask.reply],
                    });
                    changed = true;
                }
            }
            let before = pending.len();
            pending.retain_mut(|entry| {
                entry.waiters.retain(|waiter| !waiter.is_closed());
                !entry.waiters.is_empty()
            });
            changed |= pending.len() != before;
        }
        if changed {
            inner.changed.send_modify(|version| *version += 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_tools::egress::{EgressOptions, EgressPolicy};

    fn questions(options: EgressOptions) -> (HostQuestions, EgressShared, tempfile::TempDir) {
        let shared = EgressShared::new(EgressPolicy::new(), options);
        let workspace = tempfile::tempdir().unwrap();
        let questions = HostQuestions::start(
            shared.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::default(),
            workspace.path().to_path_buf(),
        );
        (questions, shared, workspace)
    }

    async fn wait_for(questions: &HostQuestions, count: usize) {
        let mut changed = questions.subscribe();
        while questions.pending().len() != count {
            tokio::time::timeout(Duration::from_secs(5), changed.changed())
                .await
                .expect("the pending set changes")
                .unwrap();
        }
    }

    /// A write under the user's identity is asked about like a host, and
    /// the answer is recorded as a write target, not as a reachable host.
    #[tokio::test]
    async fn a_write_is_asked_about_and_allowed_for_the_session() {
        let (questions, shared, _workspace) = questions(EgressOptions::default());
        let write = forge_tools::write_gate::Write {
            target: "github.com/o/r".into(),
            summary: "git push to o/r".into(),
        };
        let held = tokio::spawn({
            let (shared, write) = (shared.clone(), write.clone());
            async move { shared.admit_write(&write).await }
        });
        wait_for(&questions, 1).await;
        let pending = questions.pending();
        assert_eq!(pending[0].host, "github.com/o/r");
        assert_eq!(pending[0].write.as_deref(), Some("git push to o/r"));

        questions
            .answer("github.com/o/r", HostDecision::AllowForSession)
            .unwrap();
        assert!(held.await.unwrap().allowed());
        assert!(shared.permits_write("github.com/o/r"));
        assert!(!shared.permits_host("github.com"));
        assert!(shared.admit_write(&write).await.allowed());
    }

    /// Several connections to one host are one question, and one answer
    /// releases them all. Allowing for the session changes the policy.
    #[tokio::test]
    async fn connections_to_one_host_share_a_question_and_an_answer() {
        let (questions, shared, _workspace) = questions(EgressOptions::default());
        let first = tokio::spawn({
            let shared = shared.clone();
            async move { shared.admit("crates.invalid", 443).await }
        });
        let second = tokio::spawn({
            let shared = shared.clone();
            async move { shared.admit("crates.invalid", 443).await }
        });
        wait_for(&questions, 1).await;
        assert_eq!(questions.pending()[0].host, "crates.invalid");

        // Both asks have arrived before the answer goes out.
        tokio::time::sleep(Duration::from_millis(50)).await;
        questions
            .answer("crates.invalid", HostDecision::AllowForSession)
            .unwrap();
        assert!(first.await.unwrap() && second.await.unwrap());
        assert!(shared.permits_host("crates.invalid"));
        assert!(questions.pending().is_empty());
        assert!(questions
            .answer("crates.invalid", HostDecision::Deny)
            .is_err());
    }

    #[tokio::test]
    async fn once_and_deny_leave_the_policy_alone() {
        let (questions, shared, _workspace) = questions(EgressOptions::default());
        // Refused first: an allow-once would cover a second connection from
        // the same endpoint.
        for (decision, admitted) in [(HostDecision::Deny, false), (HostDecision::AllowOnce, true)] {
            let held = tokio::spawn({
                let shared = shared.clone();
                async move { shared.admit("x.invalid", 443).await }
            });
            wait_for(&questions, 1).await;
            questions.answer("x.invalid", decision).unwrap();
            assert_eq!(held.await.unwrap(), admitted, "{decision:?}");
        }
        assert!(shared.asks_about("x.invalid"), "no rule was added");
    }

    /// A question whose connection the gateway gave up on disappears.
    #[tokio::test]
    async fn an_abandoned_question_is_dropped() {
        let (questions, shared, _workspace) = questions(EgressOptions {
            hold_timeout: Duration::from_millis(100),
            ..Default::default()
        });
        let held = tokio::spawn(async move { shared.admit("slow.invalid", 443).await });
        wait_for(&questions, 1).await;
        assert!(!held.await.unwrap());
        wait_for(&questions, 0).await;
    }

    /// Approve-all never shows a question; the connection is refused at
    /// once and the existing grant-and-retry path takes over.
    #[tokio::test]
    async fn approve_all_refuses_without_asking() {
        let shared = EgressShared::new(EgressPolicy::new(), EgressOptions::default());
        let workspace = tempfile::tempdir().unwrap();
        let questions = HostQuestions::start(
            shared.clone(),
            Arc::new(AtomicBool::new(true)),
            Arc::default(),
            workspace.path().to_path_buf(),
        );
        assert!(!shared.admit("auto.invalid", 443).await);
        assert!(questions.pending().is_empty());
    }
}
