//! The pairing code: how a device proves the person at the desktop let it in.
//!
//! Eight Crockford base32 characters (40 bits), shown as `ABCD-EFGH`. The
//! code lives only in memory, works once, expires after [`CODE_TTL`], and is
//! destroyed after [`MAX_ATTEMPTS`] wrong guesses, so guessing it over the
//! network is hopeless. Only one code exists at a time.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::clock::Clock;

/// Characters in a code, without the display hyphen.
pub const CODE_LEN: usize = 8;
/// How long a code works.
pub const CODE_TTL: Duration = Duration::from_secs(5 * 60);
/// Wrong guesses a code survives; the next one destroys it.
pub const MAX_ATTEMPTS: u32 = 5;
/// Crockford's base32: no I, L, O or U, so a code read aloud is unambiguous.
pub const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A code as the desktop shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairingTicket {
    /// `ABCD-EFGH`.
    pub code: String,
    pub expires_at: DateTime<Utc>,
}

/// Why a code was refused. The API answers every one of them the same way;
/// the variants exist for the desktop's status and for tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PairingError {
    #[error("invalid or expired code")]
    NoCode,
    #[error("invalid or expired code")]
    Expired,
    #[error("invalid or expired code")]
    Wrong { remaining: u32 },
    #[error("invalid or expired code")]
    Exhausted,
}

/// The code waiting to be redeemed, if any.
#[derive(Debug)]
struct Pending {
    /// Canonical form: eight alphabet characters, no hyphen.
    code: String,
    issued: Instant,
    expires_at: DateTime<Utc>,
    failures: u32,
}

pub struct Pairing {
    clock: Arc<dyn Clock>,
    pending: Mutex<Option<Pending>>,
}

impl Pairing {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            pending: Mutex::new(None),
        }
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, Option<Pending>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Issue a fresh code, replacing any earlier one.
    pub fn start(&self) -> PairingTicket {
        let code = random_code();
        let expires_at =
            self.clock.wall() + chrono::Duration::from_std(CODE_TTL).expect("five minutes fit");
        let ticket = PairingTicket {
            code: display(&code),
            expires_at,
        };
        *self.pending() = Some(Pending {
            code,
            issued: self.clock.now(),
            expires_at,
            failures: 0,
        });
        ticket
    }

    /// Withdraw the code, if there is one.
    pub fn cancel(&self) {
        *self.pending() = None;
    }

    /// The code waiting to be redeemed, unless it has expired.
    pub fn active(&self) -> Option<PairingTicket> {
        let pending = self.pending();
        pending
            .as_ref()
            .filter(|p| !self.expired(p))
            .map(|p| PairingTicket {
                code: display(&p.code),
                expires_at: p.expires_at,
            })
    }

    fn expired(&self, pending: &Pending) -> bool {
        self.clock.now().saturating_duration_since(pending.issued) >= CODE_TTL
    }

    /// Redeem `input`. Success consumes the code; a wrong guess counts
    /// against it. The check and the consumption happen under one lock, so
    /// two simultaneous guesses of the right code cannot both succeed.
    pub fn redeem(&self, input: &str) -> Result<(), PairingError> {
        let mut slot = self.pending();
        let Some(pending) = slot.as_mut() else {
            return Err(PairingError::NoCode);
        };
        if self.expired(pending) {
            *slot = None;
            return Err(PairingError::Expired);
        }
        let typed = normalize(input).unwrap_or_default();
        // Constant time over equal lengths; a malformed input is compared
        // against itself so it costs the same and still counts as a guess.
        let candidate = if typed.len() == pending.code.len() {
            typed.as_bytes()
        } else {
            pending.code.as_bytes()
        };
        let equal: bool = candidate.ct_eq(pending.code.as_bytes()).into();
        if equal && typed.len() == pending.code.len() {
            *slot = None;
            return Ok(());
        }
        pending.failures += 1;
        if pending.failures >= MAX_ATTEMPTS {
            *slot = None;
            return Err(PairingError::Exhausted);
        }
        Err(PairingError::Wrong {
            remaining: MAX_ATTEMPTS - pending.failures,
        })
    }
}

/// A typed code in canonical form: case folded, hyphens and spaces dropped,
/// and the look-alikes Crockford defines mapped (`I`/`L` to `1`, `O` to
/// `0`). `None` unless exactly eight alphabet characters remain.
pub fn normalize(input: &str) -> Option<String> {
    let mut code = String::with_capacity(CODE_LEN);
    for c in input.chars() {
        let c = match c.to_ascii_uppercase() {
            '-' | ' ' => continue,
            'I' | 'L' => '1',
            'O' => '0',
            c => c,
        };
        if !c.is_ascii() || !ALPHABET.contains(&(c as u8)) || code.len() == CODE_LEN {
            return None;
        }
        code.push(c);
    }
    (code.len() == CODE_LEN).then_some(code)
}

/// Eight characters from the OS random source. 256 is a multiple of 32,
/// so taking each byte modulo 32 is unbiased.
fn random_code() -> String {
    let mut bytes = [0u8; CODE_LEN];
    getrandom::fill(&mut bytes).expect("the operating system's random source");
    bytes
        .iter()
        .map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()] as char)
        .collect()
}

/// `ABCDEFGH` as `ABCD-EFGH`.
fn display(code: &str) -> String {
    format!("{}-{}", &code[..4], &code[4..])
}

/// How long a redeemed code's pairing request waits for the desktop.
pub const REQUEST_TTL: Duration = Duration::from_secs(2 * 60);
/// Digits in the confirmation number both screens show.
pub const CONFIRMATION_DIGITS: usize = 6;

/// What the desktop shows while a device waits to pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequestView {
    pub device_name: String,
    /// Six digits, no separator.
    pub confirmation: String,
    pub expires_at: DateTime<Utc>,
}

/// What the device gets when its code opens a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedRequest {
    /// 256 random bits, base64url: the only handle to the request.
    pub pairing_id: String,
    pub confirmation: String,
    pub expires_at: DateTime<Utc>,
}

/// A poll's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    Pending,
    /// Allowed on the desktop; returned once, then the request is gone.
    Allowed {
        device_name: String,
    },
    Refused,
    /// No such request: never opened, expired, or already collected.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecideError {
    #[error("no device is waiting to pair")]
    NoRequest,
    #[error("that pairing request was replaced; check the number again")]
    Mismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestState {
    Pending,
    Allowed,
    Refused,
}

#[derive(Debug)]
struct Request {
    /// SHA-256 of the pairing id.
    id_hash: [u8; 32],
    device_name: String,
    confirmation: String,
    opened: Instant,
    expires_at: DateTime<Utc>,
    state: RequestState,
}

/// The one pairing request a redeemed code opens: the device waits on it
/// until the person at the desktop allows or refuses it.
pub struct PairingRequests {
    clock: Arc<dyn Clock>,
    slot: Mutex<Option<Request>>,
}

impl PairingRequests {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            slot: Mutex::new(None),
        }
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Request>> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn expired(&self, request: &Request) -> bool {
        self.clock.now().saturating_duration_since(request.opened) >= REQUEST_TTL
    }

    /// Open a request for `device_name`, replacing any other.
    pub fn open(&self, device_name: &str) -> OpenedRequest {
        let mut id = [0u8; 32];
        getrandom::fill(&mut id).expect("the operating system's random source");
        let pairing_id = URL_SAFE_NO_PAD.encode(id);
        let confirmation = random_confirmation();
        let expires_at =
            self.clock.wall() + chrono::Duration::from_std(REQUEST_TTL).expect("two minutes fit");
        *self.slot() = Some(Request {
            id_hash: Sha256::digest(pairing_id.as_bytes()).into(),
            device_name: device_name.to_string(),
            confirmation: confirmation.clone(),
            opened: self.clock.now(),
            expires_at,
            state: RequestState::Pending,
        });
        OpenedRequest {
            pairing_id,
            confirmation,
            expires_at,
        }
    }

    /// The request waiting on the desktop, unless decided or expired.
    pub fn view(&self) -> Option<RequestView> {
        let slot = self.slot();
        slot.as_ref()
            .filter(|r| r.state == RequestState::Pending && !self.expired(r))
            .map(|r| RequestView {
                device_name: r.device_name.clone(),
                confirmation: r.confirmation.clone(),
                expires_at: r.expires_at,
            })
    }

    /// Allow or refuse the request showing `confirmation`. `Ok` carries the
    /// device's name.
    pub fn decide(&self, confirmation: &str, allow: bool) -> Result<String, DecideError> {
        let mut slot = self.slot();
        let expired = slot.as_ref().is_some_and(|r| self.expired(r));
        if expired {
            *slot = None;
        }
        let request = slot
            .as_mut()
            .filter(|r| r.state == RequestState::Pending)
            .ok_or(DecideError::NoRequest)?;
        if !bool::from(
            confirmation
                .as_bytes()
                .ct_eq(request.confirmation.as_bytes()),
        ) {
            return Err(DecideError::Mismatch);
        }
        request.state = if allow {
            RequestState::Allowed
        } else {
            RequestState::Refused
        };
        Ok(request.device_name.clone())
    }

    /// What the holder of `pairing_id` learns. Compared in constant time.
    pub fn poll(&self, pairing_id: &str) -> Poll {
        let presented: [u8; 32] = Sha256::digest(pairing_id.as_bytes()).into();
        let mut slot = self.slot();
        let Some(request) = slot.as_ref() else {
            return Poll::Unknown;
        };
        if self.expired(request) {
            *slot = None;
            return Poll::Unknown;
        }
        if !bool::from(presented.ct_eq(&request.id_hash)) {
            return Poll::Unknown;
        }
        match request.state {
            RequestState::Pending => Poll::Pending,
            RequestState::Refused => Poll::Refused,
            RequestState::Allowed => {
                let device_name = request.device_name.clone();
                // Collected once: the request is gone with its answer.
                *slot = None;
                Poll::Allowed { device_name }
            }
        }
    }

    /// End the request, whatever its state.
    pub fn cancel(&self) {
        *self.slot() = None;
    }
}

/// Six digits from the OS random source, each of the million equally
/// likely (rejection sampling, so no value is favoured).
fn random_confirmation() -> String {
    const RANGE: u32 = 1_000_000;
    const LIMIT: u32 = u32::MAX - (u32::MAX % RANGE);
    loop {
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes).expect("the operating system's random source");
        let value = u32::from_le_bytes(bytes);
        if value < LIMIT {
            return format!("{:06}", value % RANGE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;

    fn pairing() -> (Arc<ManualClock>, Pairing) {
        let clock = Arc::new(ManualClock::new());
        (clock.clone(), Pairing::new(clock))
    }

    fn wrong_code(right: &str) -> String {
        let canonical = normalize(right).unwrap();
        let first = if canonical.starts_with('0') { "1" } else { "0" };
        format!("{first}{}", &canonical[1..])
    }

    #[test]
    fn a_code_is_eight_crockford_characters_shown_in_two_groups() {
        let (_, pairing) = pairing();
        let ticket = pairing.start();
        let (left, right) = ticket.code.split_once('-').expect("ABCD-EFGH");
        assert_eq!((left.len(), right.len()), (4, 4), "{}", ticket.code);
        assert!(ticket
            .code
            .bytes()
            .filter(|b| *b != b'-')
            .all(|b| ALPHABET.contains(&b)));
    }

    #[test]
    fn codes_are_not_repeated() {
        let (_, pairing) = pairing();
        let codes: std::collections::HashSet<String> =
            (0..64).map(|_| pairing.start().code).collect();
        assert!(codes.len() >= 63, "codes must be random: {codes:?}");
    }

    #[test]
    fn typed_codes_are_normalised() {
        assert_eq!(normalize("abcd-efgh").as_deref(), Some("ABCDEFGH"));
        assert_eq!(normalize(" AB CD EF GH ").as_deref(), Some("ABCDEFGH"));
        assert_eq!(normalize("OIL0-1234").as_deref(), Some("01101234"));
        assert_eq!(normalize("ABCD-EFG"), None, "too short");
        assert_eq!(normalize("ABCD-EFGHJ"), None, "too long");
        assert_eq!(normalize("ABCD-EFGU"), None, "U is not in the alphabet");
        assert_eq!(
            normalize("ABCD_EFGH"),
            None,
            "only hyphens and spaces are dropped"
        );
    }

    #[test]
    fn a_code_works_once() {
        let (_, pairing) = pairing();
        let ticket = pairing.start();
        assert_eq!(pairing.redeem(&ticket.code), Ok(()));
        assert_eq!(pairing.redeem(&ticket.code), Err(PairingError::NoCode));
        assert_eq!(pairing.active(), None);
    }

    #[test]
    fn a_code_typed_differently_still_works() {
        let (_, pairing) = pairing();
        let ticket = pairing.start();
        let typed = ticket.code.to_lowercase().replace('-', " ");
        assert_eq!(pairing.redeem(&typed), Ok(()));
    }

    #[test]
    fn a_code_expires_after_five_minutes() {
        let (clock, pairing) = pairing();
        let ticket = pairing.start();
        assert_eq!(
            (ticket.expires_at - clock.wall()).num_seconds(),
            CODE_TTL.as_secs() as i64
        );
        clock.advance(CODE_TTL - Duration::from_secs(1));
        assert!(pairing.active().is_some(), "still valid at 4:59");
        clock.advance(Duration::from_secs(1));
        assert_eq!(pairing.active(), None, "gone at 5:00");
        assert_eq!(pairing.redeem(&ticket.code), Err(PairingError::Expired));
        assert_eq!(
            pairing.redeem(&ticket.code),
            Err(PairingError::NoCode),
            "an expired code is dropped"
        );
    }

    #[test]
    fn wrong_guesses_use_the_code_up() {
        let (_, pairing) = pairing();
        let ticket = pairing.start();
        let wrong = wrong_code(&ticket.code);
        for remaining in (1..MAX_ATTEMPTS).rev() {
            assert_eq!(
                pairing.redeem(&wrong),
                Err(PairingError::Wrong { remaining })
            );
        }
        assert_eq!(pairing.redeem(&wrong), Err(PairingError::Exhausted));
        assert_eq!(
            pairing.redeem(&ticket.code),
            Err(PairingError::NoCode),
            "after five wrong guesses the right code no longer works"
        );
        assert_eq!(pairing.active(), None);
    }

    #[test]
    fn malformed_input_counts_as_a_wrong_guess() {
        let (_, pairing) = pairing();
        let ticket = pairing.start();
        assert_eq!(
            pairing.redeem("nope"),
            Err(PairingError::Wrong {
                remaining: MAX_ATTEMPTS - 1
            })
        );
        assert_eq!(pairing.redeem(&ticket.code), Ok(()));
    }

    #[test]
    fn nothing_is_accepted_without_a_code() {
        let (_, pairing) = pairing();
        assert_eq!(pairing.redeem("ABCD-EFGH"), Err(PairingError::NoCode));
    }

    #[test]
    fn a_new_code_replaces_the_old_one_and_cancel_withdraws_it() {
        let (_, pairing) = pairing();
        let first = pairing.start();
        let second = pairing.start();
        if normalize(&first.code) != normalize(&second.code) {
            assert!(pairing.redeem(&first.code).is_err());
        }
        assert_eq!(pairing.active().map(|t| t.code), Some(second.code.clone()));
        pairing.cancel();
        assert_eq!(pairing.active(), None);
        assert_eq!(pairing.redeem(&second.code), Err(PairingError::NoCode));
    }

    #[test]
    fn two_simultaneous_redemptions_cannot_both_succeed() {
        for _ in 0..50 {
            let (_, pairing) = pairing();
            let pairing = Arc::new(pairing);
            let code = pairing.start().code;
            let barrier = Arc::new(std::sync::Barrier::new(8));
            let winners: usize = (0..8)
                .map(|_| {
                    let (pairing, code, barrier) = (pairing.clone(), code.clone(), barrier.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        pairing.redeem(&code).is_ok()
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| usize::from(handle.join().unwrap()))
                .sum();
            assert_eq!(winners, 1);
        }
    }

    #[test]
    fn every_refusal_reads_the_same() {
        let messages: std::collections::HashSet<String> = [
            PairingError::NoCode,
            PairingError::Expired,
            PairingError::Wrong { remaining: 3 },
            PairingError::Exhausted,
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(messages.len(), 1);
    }

    mod requests {
        use super::super::*;
        use crate::clock::ManualClock;

        fn requests() -> (Arc<ManualClock>, PairingRequests) {
            let clock = Arc::new(ManualClock::new());
            (clock.clone(), PairingRequests::new(clock))
        }

        #[test]
        fn a_request_has_a_long_random_id_and_a_six_digit_number() {
            let (clock, requests) = requests();
            let opened = requests.open("Pixel 8");
            assert_eq!(opened.pairing_id.len(), 43, "256 bits of base64url");
            assert!(opened
                .pairing_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
            assert_eq!(opened.confirmation.len(), CONFIRMATION_DIGITS);
            assert!(opened.confirmation.bytes().all(|b| b.is_ascii_digit()));
            assert_eq!(
                (opened.expires_at - clock.wall()).num_seconds(),
                REQUEST_TTL.as_secs() as i64
            );
            let view = requests.view().unwrap();
            assert_eq!(view.device_name, "Pixel 8");
            assert_eq!(view.confirmation, opened.confirmation);
            let ids: std::collections::HashSet<String> =
                (0..32).map(|_| requests.open("x").pairing_id).collect();
            assert_eq!(ids.len(), 32);
        }

        #[test]
        fn confirmation_numbers_use_every_digit() {
            let (_, requests) = requests();
            let digits: std::collections::HashSet<char> = (0..200)
                .flat_map(|_| requests.open("x").confirmation.chars().collect::<Vec<_>>())
                .collect();
            assert_eq!(digits.len(), 10);
        }

        #[test]
        fn a_request_is_pending_until_allowed_and_then_collected_once() {
            let (_, requests) = requests();
            let opened = requests.open("Pixel 8");
            assert_eq!(requests.poll(&opened.pairing_id), Poll::Pending);
            assert_eq!(
                requests.decide(&opened.confirmation, true),
                Ok("Pixel 8".into())
            );
            assert_eq!(requests.view(), None, "decided requests leave the desktop");
            assert_eq!(
                requests.poll(&opened.pairing_id),
                Poll::Allowed {
                    device_name: "Pixel 8".into()
                }
            );
            assert_eq!(
                requests.poll(&opened.pairing_id),
                Poll::Unknown,
                "collected once"
            );
        }

        #[test]
        fn a_refused_request_says_so_until_it_expires() {
            let (clock, requests) = requests();
            let opened = requests.open("Intruder");
            assert_eq!(
                requests.decide(&opened.confirmation, false),
                Ok("Intruder".into())
            );
            assert_eq!(requests.poll(&opened.pairing_id), Poll::Refused);
            assert_eq!(requests.poll(&opened.pairing_id), Poll::Refused);
            clock.advance(REQUEST_TTL);
            assert_eq!(requests.poll(&opened.pairing_id), Poll::Unknown);
        }

        #[test]
        fn an_undecided_request_expires_after_two_minutes() {
            let (clock, requests) = requests();
            let opened = requests.open("Pixel 8");
            clock.advance(REQUEST_TTL - Duration::from_secs(1));
            assert!(requests.view().is_some());
            clock.advance(Duration::from_secs(1));
            assert_eq!(requests.view(), None);
            assert_eq!(requests.poll(&opened.pairing_id), Poll::Unknown);
            assert_eq!(
                requests.decide(&opened.confirmation, true),
                Err(DecideError::NoRequest),
                "an expired request cannot be allowed"
            );
        }

        #[test]
        fn a_decision_must_name_the_request_on_screen() {
            let (_, requests) = requests();
            let first = requests.open("Pixel 8");
            let second = requests.open("Other");
            if first.confirmation != second.confirmation {
                assert_eq!(
                    requests.decide(&first.confirmation, true),
                    Err(DecideError::Mismatch)
                );
            }
            assert_eq!(requests.poll(&first.pairing_id), Poll::Unknown, "replaced");
            assert_eq!(
                requests.poll(&second.pairing_id),
                Poll::Pending,
                "still waiting"
            );
            assert_eq!(
                requests.decide("000000x", true).unwrap_err(),
                DecideError::Mismatch
            );
        }

        #[test]
        fn a_decided_request_cannot_be_decided_again() {
            let (_, requests) = requests();
            let opened = requests.open("Pixel 8");
            requests.decide(&opened.confirmation, false).unwrap();
            assert_eq!(
                requests.decide(&opened.confirmation, true),
                Err(DecideError::NoRequest),
                "a refusal stands"
            );
            assert_eq!(requests.poll(&opened.pairing_id), Poll::Refused);
        }

        #[test]
        fn cancel_ends_the_request() {
            let (_, requests) = requests();
            let opened = requests.open("Pixel 8");
            requests.cancel();
            assert_eq!(requests.view(), None);
            assert_eq!(requests.poll(&opened.pairing_id), Poll::Unknown);
            assert_eq!(
                requests.decide(&opened.confirmation, true),
                Err(DecideError::NoRequest)
            );
        }

        #[test]
        fn unknown_and_malformed_ids_are_just_unknown() {
            let (_, requests) = requests();
            requests.open("Pixel 8");
            for id in ["", "short", &"A".repeat(43), &"A".repeat(4000)] {
                assert_eq!(requests.poll(id), Poll::Unknown, "{}", id.len());
            }
        }
    }
}
