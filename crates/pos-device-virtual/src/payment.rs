//! Mock payment provider (E10-T02, PAY-006, PRD §12.5): a scriptable terminal for tests and
//! the dev panel. Never shipped as a real payment method.
//!
//! Each new attempt takes the next scripted [`PaymentScenario`] (approve once the script is
//! empty). The mock keeps a ledger of what it "charged" and "gave back", so tests can assert
//! that retries never charge twice: initiating an attempt it already knows returns that
//! attempt's status and consumes nothing.
//!
//! Test data only: references look like `mock_ch_000001`, the masked card is `****4242`
//! (a well-known test card's last four), and no card number exists anywhere.

use pos_device_ports::{DeviceError, PaymentCapabilities, PaymentPort, PaymentRequest, PaymentStatus};
use pos_domain::{CurrencyCode, Money, PaymentAttemptId};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

/// What the terminal does with the next new attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaymentScenario {
    /// Approved straight away (and charged).
    Approve,
    /// Declined with this reason (nothing charged).
    Decline(String),
    /// The request is sent, then the answer is lost: `initiate` fails with
    /// [`DeviceError::Timeout`] and queries answer `Unknown`. Whether the card was charged
    /// is up to [`MockPaymentProvider::finish_on_terminal`] (the terminal's own record).
    Timeout,
    /// Approved, and the approval is delivered twice (the next query repeats it).
    DuplicateCallback,
    /// `Pending` for this many queries, then approved (and charged).
    Delayed(u32),
    /// Approved, then the provider reverses it on its own (e.g. an acquirer void): the next
    /// query answers `Reversed` and the charge is given back.
    Reverse,
    /// `initiate` answers `Unknown`, and so does every query until
    /// [`MockPaymentProvider::finish_on_terminal`].
    Unknown,
    /// The terminal cannot be reached: `initiate` fails with
    /// [`DeviceError::Disconnected`] and nothing is sent or charged.
    Disconnected,
    /// The request is sent and the terminal never answers (the caller's timeout decides).
    Hang,
}

/// One approval in the mock's ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Charge {
    pub attempt_id: PaymentAttemptId,
    pub amount: Money,
    pub currency: CurrencyCode,
    pub provider_ref: String,
}

#[derive(Debug)]
struct Txn {
    request: PaymentRequest,
    status: PaymentStatus,
    /// `Delayed`: queries still to answer `Pending`.
    pending_queries: u32,
    /// `Reverse`: the next query reverses it.
    reverse_on_query: bool,
    /// Approvals delivered (initiate + repeated callbacks).
    deliveries: u32,
}

#[derive(Debug, Default)]
struct State {
    script: VecDeque<PaymentScenario>,
    txns: HashMap<PaymentAttemptId, Txn>,
    charges: Vec<Charge>,
    refunds: Vec<Charge>,
    initiate_calls: usize,
    query_calls: usize,
    next_ref: u32,
}

#[derive(Debug)]
pub struct MockPaymentProvider {
    state: Mutex<State>,
    capabilities: PaymentCapabilities,
}

impl Default for MockPaymentProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl MockPaymentProvider {
    /// Approves everything until scripted.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            capabilities: PaymentCapabilities::default(),
        }
    }

    /// A mock that reports other capabilities (e.g. no cancel from the till).
    pub fn with_capabilities(capabilities: PaymentCapabilities) -> Self {
        Self {
            state: Mutex::new(State::default()),
            capabilities,
        }
    }

    /// Scenarios for the next new attempts, in order; afterwards it approves again.
    pub fn script(&self, scenarios: impl IntoIterator<Item = PaymentScenario>) {
        self.lock().script.extend(scenarios);
    }

    /// Every approval, in order (one per attempt, however often it was initiated).
    pub fn charges(&self) -> Vec<Charge> {
        self.lock().charges.clone()
    }

    /// Approvals given back (reversals).
    pub fn refunds(&self) -> Vec<Charge> {
        self.lock().refunds.clone()
    }

    /// Charged less given back, for one attempt.
    pub fn net_charged(&self, attempt: PaymentAttemptId) -> Money {
        let s = self.lock();
        let sum = |l: &[Charge]| {
            l.iter()
                .filter(|c| c.attempt_id == attempt)
                .map(|c| c.amount.minor())
                .sum::<i64>()
        };
        Money::from_minor(sum(&s.charges) - sum(&s.refunds))
    }

    pub fn initiate_calls(&self) -> usize {
        self.lock().initiate_calls
    }

    pub fn query_calls(&self) -> usize {
        self.lock().query_calls
    }

    /// How many times the approval of `attempt` was delivered (2 for `DuplicateCallback`).
    pub fn deliveries(&self, attempt: PaymentAttemptId) -> u32 {
        self.lock().txns.get(&attempt).map_or(0, |t| t.deliveries)
    }

    /// Whether the terminal ever received `attempt`.
    pub fn knows(&self, attempt: PaymentAttemptId) -> bool {
        self.lock().txns.contains_key(&attempt)
    }

    /// The terminal's own record settles an attempt whose answer was lost (`Timeout`,
    /// `Unknown`, `Hang`): `true` = it went through (charged), `false` = it did not. Later
    /// queries report it.
    pub fn finish_on_terminal(&self, attempt: PaymentAttemptId, approved: bool) {
        let mut s = self.lock();
        let Some(mut txn) = s.txns.remove(&attempt) else {
            return;
        };
        if approved {
            s.approve(&mut txn);
        } else {
            txn.status = PaymentStatus::Declined {
                reason: "not completed on the terminal".into(),
            };
        }
        s.txns.insert(attempt, txn);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl State {
    fn approve(&mut self, txn: &mut Txn) {
        self.next_ref += 1;
        let provider_ref = format!("mock_ch_{:06}", self.next_ref);
        self.charges.push(Charge {
            attempt_id: txn.request.attempt_id,
            amount: txn.request.amount,
            currency: txn.request.currency,
            provider_ref: provider_ref.clone(),
        });
        txn.deliveries += 1;
        txn.status = PaymentStatus::Approved {
            provider_ref,
            auth_code: Some(format!("A{:05}", self.next_ref)),
            card_brand: Some("MOCKCARD".into()),
            masked_pan: Some("****4242".into()),
        };
    }

    fn give_back(&mut self, txn: &mut Txn) {
        if let PaymentStatus::Approved { provider_ref, .. } = &txn.status {
            self.refunds.push(Charge {
                attempt_id: txn.request.attempt_id,
                amount: txn.request.amount,
                currency: txn.request.currency,
                provider_ref: provider_ref.clone(),
            });
            txn.status = PaymentStatus::Reversed;
        }
    }
}

enum Answer {
    Now(Result<PaymentStatus, DeviceError>),
    Never,
}

#[async_trait::async_trait]
impl PaymentPort for MockPaymentProvider {
    fn provider(&self) -> &'static str {
        "mock"
    }

    fn capabilities(&self) -> PaymentCapabilities {
        self.capabilities
    }

    async fn initiate(&self, request: &PaymentRequest) -> Result<PaymentStatus, DeviceError> {
        let answer = {
            let mut s = self.lock();
            s.initiate_calls += 1;
            // Idempotency: a known attempt answers with its transaction, nothing new.
            if let Some(txn) = s.txns.get(&request.attempt_id) {
                return if txn.request == *request {
                    Ok(txn.status.clone())
                } else {
                    Err(DeviceError::Unsupported(
                        "this attempt was already sent with a different amount or kind".into(),
                    ))
                };
            }
            let scenario = s.script.pop_front().unwrap_or(PaymentScenario::Approve);
            let mut txn = Txn {
                request: request.clone(),
                status: PaymentStatus::Pending,
                pending_queries: 0,
                reverse_on_query: false,
                deliveries: 0,
            };
            let answer = match scenario {
                PaymentScenario::Disconnected => {
                    return Err(DeviceError::Disconnected(
                        "the mock terminal is switched off".into(),
                    ));
                }
                PaymentScenario::Approve => {
                    s.approve(&mut txn);
                    Answer::Now(Ok(txn.status.clone()))
                }
                PaymentScenario::DuplicateCallback => {
                    s.approve(&mut txn);
                    // The second delivery of the same approval arrives with the next query.
                    txn.deliveries += 1;
                    Answer::Now(Ok(txn.status.clone()))
                }
                PaymentScenario::Reverse => {
                    s.approve(&mut txn);
                    txn.reverse_on_query = true;
                    Answer::Now(Ok(txn.status.clone()))
                }
                PaymentScenario::Decline(reason) => {
                    txn.status = PaymentStatus::Declined { reason };
                    Answer::Now(Ok(txn.status.clone()))
                }
                PaymentScenario::Delayed(0) => {
                    s.approve(&mut txn);
                    Answer::Now(Ok(txn.status.clone()))
                }
                PaymentScenario::Delayed(n) => {
                    txn.pending_queries = n;
                    Answer::Now(Ok(PaymentStatus::Pending))
                }
                PaymentScenario::Timeout => {
                    txn.status = PaymentStatus::Unknown;
                    Answer::Now(Err(DeviceError::Timeout))
                }
                PaymentScenario::Unknown => {
                    txn.status = PaymentStatus::Unknown;
                    Answer::Now(Ok(PaymentStatus::Unknown))
                }
                PaymentScenario::Hang => {
                    txn.status = PaymentStatus::Unknown;
                    Answer::Never
                }
            };
            s.txns.insert(request.attempt_id, txn);
            answer
        };
        match answer {
            Answer::Now(r) => r,
            Answer::Never => std::future::pending().await,
        }
    }

    async fn query(&self, attempt: PaymentAttemptId) -> Result<PaymentStatus, DeviceError> {
        let mut s = self.lock();
        s.query_calls += 1;
        let Some(mut txn) = s.txns.remove(&attempt) else {
            // Never received: the terminal cannot say anything about it.
            return Ok(PaymentStatus::Unknown);
        };
        if txn.pending_queries > 0 {
            txn.pending_queries -= 1;
            if txn.pending_queries == 0 {
                s.approve(&mut txn);
            }
        } else if txn.reverse_on_query {
            txn.reverse_on_query = false;
            s.give_back(&mut txn);
        }
        let status = txn.status.clone();
        s.txns.insert(attempt, txn);
        Ok(status)
    }

    async fn cancel(&self, attempt: PaymentAttemptId) -> Result<PaymentStatus, DeviceError> {
        if !self.capabilities.cancel {
            return Err(DeviceError::Unsupported("cancel on the terminal".into()));
        }
        let mut s = self.lock();
        let Some(txn) = s.txns.get_mut(&attempt) else {
            return Ok(PaymentStatus::Cancelled);
        };
        if txn.status == PaymentStatus::Pending {
            txn.status = PaymentStatus::Cancelled;
            txn.pending_queries = 0;
        }
        Ok(txn.status.clone())
    }

    async fn reverse(&self, attempt: PaymentAttemptId) -> Result<PaymentStatus, DeviceError> {
        if !self.capabilities.reversal {
            return Err(DeviceError::Unsupported("reversal".into()));
        }
        let mut s = self.lock();
        let Some(mut txn) = s.txns.remove(&attempt) else {
            return Err(DeviceError::Unsupported(
                "no such payment on this terminal".into(),
            ));
        };
        let result = match txn.status {
            PaymentStatus::Approved { .. } => {
                s.give_back(&mut txn);
                Ok(txn.status.clone())
            }
            PaymentStatus::Reversed => Ok(PaymentStatus::Reversed),
            _ => Err(DeviceError::Unsupported(
                "only an approved payment can be reversed".into(),
            )),
        };
        s.txns.insert(attempt, txn);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pos_domain::payment::PaymentKind;
    use pos_domain::{IdGenerator, UuidV7Generator};

    fn request(minor: i64) -> PaymentRequest {
        let ids: &dyn IdGenerator = &UuidV7Generator;
        PaymentRequest {
            attempt_id: PaymentAttemptId::generate(ids),
            amount: Money::from_minor(minor),
            currency: CurrencyCode::new("GBP").unwrap(),
            kind: PaymentKind::Sale,
            reference: "cart-1".into(),
        }
    }

    fn is_approved(s: &PaymentStatus) -> bool {
        matches!(s, PaymentStatus::Approved { .. })
    }

    #[tokio::test]
    async fn approves_by_default_and_never_charges_twice() {
        let p = MockPaymentProvider::new();
        let r = request(1000);
        let first = p.initiate(&r).await.unwrap();
        let again = p.initiate(&r).await.unwrap();
        assert!(is_approved(&first));
        assert_eq!(first, again, "the same transaction");
        assert_eq!(p.charges().len(), 1);
        assert_eq!(p.net_charged(r.attempt_id), Money::from_minor(1000));
        assert_eq!(p.initiate_calls(), 2);
        // The same key with another amount is refused, not charged.
        let mut other = r.clone();
        other.amount = Money::from_minor(1);
        assert!(matches!(
            p.initiate(&other).await,
            Err(DeviceError::Unsupported(_))
        ));
        assert_eq!(p.charges().len(), 1);
        if let PaymentStatus::Approved {
            provider_ref,
            masked_pan,
            ..
        } = first
        {
            assert!(provider_ref.starts_with("mock_ch_"));
            assert_eq!(masked_pan.as_deref(), Some("****4242"));
        }
    }

    #[tokio::test]
    async fn every_scenario() {
        let p = MockPaymentProvider::new();
        p.script([
            PaymentScenario::Decline("insufficient funds".into()),
            PaymentScenario::Timeout,
            PaymentScenario::DuplicateCallback,
            PaymentScenario::Delayed(2),
            PaymentScenario::Reverse,
            PaymentScenario::Unknown,
            PaymentScenario::Disconnected,
        ]);

        let declined = request(100);
        assert_eq!(
            p.initiate(&declined).await.unwrap(),
            PaymentStatus::Declined {
                reason: "insufficient funds".into()
            }
        );

        let timeout = request(100);
        assert_eq!(p.initiate(&timeout).await, Err(DeviceError::Timeout));
        assert_eq!(p.query(timeout.attempt_id).await.unwrap(), PaymentStatus::Unknown);
        p.finish_on_terminal(timeout.attempt_id, true);
        assert!(is_approved(&p.query(timeout.attempt_id).await.unwrap()));

        let dup = request(100);
        let a = p.initiate(&dup).await.unwrap();
        assert_eq!(
            p.query(dup.attempt_id).await.unwrap(),
            a,
            "the same approval again"
        );
        assert_eq!(p.deliveries(dup.attempt_id), 2);

        let delayed = request(100);
        assert_eq!(p.initiate(&delayed).await.unwrap(), PaymentStatus::Pending);
        assert_eq!(p.query(delayed.attempt_id).await.unwrap(), PaymentStatus::Pending);
        assert!(is_approved(&p.query(delayed.attempt_id).await.unwrap()));

        let reversed = request(100);
        assert!(is_approved(&p.initiate(&reversed).await.unwrap()));
        assert_eq!(
            p.query(reversed.attempt_id).await.unwrap(),
            PaymentStatus::Reversed
        );
        assert_eq!(p.net_charged(reversed.attempt_id), Money::ZERO);

        let unknown = request(100);
        assert_eq!(p.initiate(&unknown).await.unwrap(), PaymentStatus::Unknown);
        assert_eq!(p.query(unknown.attempt_id).await.unwrap(), PaymentStatus::Unknown);

        let off = request(100);
        assert!(matches!(
            p.initiate(&off).await,
            Err(DeviceError::Disconnected(_))
        ));
        assert!(!p.knows(off.attempt_id), "nothing was sent");

        // timeout (finished on the terminal), duplicate, delayed, reversed.
        assert_eq!(p.charges().len(), 4);
        assert_eq!(p.refunds().len(), 1);
    }

    #[tokio::test]
    async fn cancel_and_reverse() {
        let p = MockPaymentProvider::new();
        p.script([PaymentScenario::Delayed(5)]);
        let r = request(100);
        p.initiate(&r).await.unwrap();
        assert_eq!(p.cancel(r.attempt_id).await.unwrap(), PaymentStatus::Cancelled);
        assert_eq!(p.query(r.attempt_id).await.unwrap(), PaymentStatus::Cancelled);
        assert!(p.charges().is_empty());

        let r = request(250);
        p.initiate(&r).await.unwrap();
        assert!(
            is_approved(&p.cancel(r.attempt_id).await.unwrap()),
            "too late to cancel"
        );
        assert_eq!(p.reverse(r.attempt_id).await.unwrap(), PaymentStatus::Reversed);
        assert_eq!(p.reverse(r.attempt_id).await.unwrap(), PaymentStatus::Reversed);
        assert_eq!(p.refunds().len(), 1, "reversing twice gives back once");
        assert_eq!(p.net_charged(r.attempt_id), Money::ZERO);

        let none = MockPaymentProvider::with_capabilities(PaymentCapabilities {
            cancel: false,
            ..PaymentCapabilities::default()
        });
        assert!(none.cancel(r.attempt_id).await.is_err());
    }

    #[tokio::test]
    async fn hang_never_answers() {
        let p = MockPaymentProvider::new();
        p.script([PaymentScenario::Hang]);
        let r = request(100);
        let res = tokio::time::timeout(std::time::Duration::from_millis(20), p.initiate(&r)).await;
        assert!(res.is_err());
        assert!(p.knows(r.attempt_id), "it was sent");
        assert_eq!(p.query(r.attempt_id).await.unwrap(), PaymentStatus::Unknown);
    }
}
