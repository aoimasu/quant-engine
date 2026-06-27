//! Tier-partitioned websocket connection registry.
//!
//! One connection per [`StreamTier`] (Market and Realtime held in **separate** slots — the partition the
//! AC requires). Each slot owns its connection, its recorded subscriptions, and the last event time per
//! stream. On a disconnect the registry reconnects via the [`WsConnector`], **resubscribes** the recorded
//! subscriptions on the fresh socket, and — because the per-stream `last_event_ms` is preserved across the
//! outage — the first post-reconnect message naturally **reports the gap**. The same rule reports an
//! in-stream skip without a disconnect.

use std::collections::HashMap;

use crate::stream::{Gap, StreamMessage, StreamTier, Subscription};
use crate::ws::{WsConnection, WsConnector, WsError, WsPoll};

/// What a single [`ConnectionRegistry::pump`] produced.
#[derive(Debug, Default)]
pub struct PumpOutcome {
    /// A decoded message, if one was available.
    pub message: Option<StreamMessage>,
    /// A detected discontinuity (across a reconnect or an in-stream skip), if any.
    pub gap: Option<Gap>,
    /// Whether this pump reconnected + resubscribed the tier (the gap, if any, surfaces on the next pump).
    pub reconnected: bool,
}

/// One tier's live connection plus its bookkeeping.
struct TierConn {
    conn: Box<dyn WsConnection>,
    subs: Vec<Subscription>,
    /// Last event time seen per stream name — preserved across reconnects so the outage gap is detectable.
    last_event_ms: HashMap<String, i64>,
}

/// A websocket registry partitioned by [`StreamTier`].
pub struct ConnectionRegistry<C: WsConnector> {
    connector: C,
    tiers: HashMap<StreamTier, TierConn>,
}

impl<C: WsConnector> ConnectionRegistry<C> {
    /// An empty registry over `connector`.
    pub fn new(connector: C) -> Self {
        Self {
            connector,
            tiers: HashMap::new(),
        }
    }

    /// Subscribe `subs` on `tier`, connecting the tier's socket if it is not yet open. Subscriptions are
    /// appended to the tier's recorded set (replayed on every reconnect). Market and Realtime are kept in
    /// separate slots — a subscription on one tier never enters the other.
    ///
    /// # Errors
    /// [`WsError`] if the connect or subscribe fails.
    pub fn subscribe(&mut self, tier: StreamTier, subs: &[Subscription]) -> Result<(), WsError> {
        if !self.tiers.contains_key(&tier) {
            let conn = self.connector.connect(tier)?;
            self.tiers.insert(
                tier,
                TierConn {
                    conn,
                    subs: Vec::new(),
                    last_event_ms: HashMap::new(),
                },
            );
        }
        let entry = self
            .tiers
            .get_mut(&tier)
            .expect("tier was just inserted if absent");
        entry.conn.subscribe(subs)?;
        for s in subs {
            if !entry.subs.contains(s) {
                entry.subs.push(s.clone());
            }
        }
        Ok(())
    }

    /// Poll `tier` once: deliver the next message (with any gap detected against the last event on its
    /// stream), or — on a disconnect — reconnect and resubscribe the recorded subscriptions.
    ///
    /// # Errors
    /// [`WsError`] if a reconnect or resubscribe fails.
    pub fn pump(&mut self, tier: StreamTier) -> Result<PumpOutcome, WsError> {
        let poll = {
            let entry = match self.tiers.get_mut(&tier) {
                Some(e) => e,
                None => return Ok(PumpOutcome::default()),
            };
            entry.conn.poll()
        };

        match poll {
            WsPoll::Idle => Ok(PumpOutcome::default()),
            WsPoll::Message(msg) => {
                let entry = self.tiers.get_mut(&tier).expect("tier present");
                let stream = msg.subscription.stream_name();
                let cadence = msg.subscription.channel.cadence_ms();
                let gap = entry.last_event_ms.get(&stream).and_then(|&last| {
                    // A contiguous next message is exactly one cadence later; only a larger jump is a hole.
                    (msg.event_time_ms - last > cadence).then(|| Gap {
                        stream: stream.clone(),
                        from_ms: last,
                        to_ms: msg.event_time_ms,
                    })
                });
                entry.last_event_ms.insert(stream, msg.event_time_ms);
                Ok(PumpOutcome {
                    message: Some(msg),
                    gap,
                    reconnected: false,
                })
            }
            WsPoll::Disconnected => {
                // Reconnect + resubscribe; preserve last_event_ms so the next message reports the gap.
                let new_conn = self.connector.connect(tier)?;
                let entry = self.tiers.get_mut(&tier).expect("tier present");
                entry.conn = new_conn;
                let subs = entry.subs.clone();
                entry.conn.subscribe(&subs)?;
                Ok(PumpOutcome {
                    message: None,
                    gap: None,
                    reconnected: true,
                })
            }
        }
    }

    /// The tiers with an open connection (the active partitions).
    #[must_use]
    pub fn active_tiers(&self) -> Vec<StreamTier> {
        let mut t: Vec<StreamTier> = self.tiers.keys().copied().collect();
        t.sort();
        t
    }

    /// The recorded subscriptions on `tier` (empty if the tier is not open).
    #[must_use]
    pub fn subscriptions(&self, tier: StreamTier) -> Vec<Subscription> {
        self.tiers
            .get(&tier)
            .map(|e| e.subs.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::{StreamChannel, StreamMessage};
    use qe_domain::{InstrumentId, Resolution};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn inst() -> InstrumentId {
        InstrumentId::new("BTCUSDT").unwrap()
    }

    /// A scripted connection: serves a queue of poll outcomes and records every `subscribe` call.
    struct FakeConnection {
        script: RefCell<Vec<WsPoll>>,
        subscribe_calls: Rc<RefCell<usize>>,
    }

    impl WsConnection for FakeConnection {
        fn subscribe(&mut self, _subs: &[Subscription]) -> Result<(), WsError> {
            *self.subscribe_calls.borrow_mut() += 1;
            Ok(())
        }
        fn poll(&mut self) -> WsPoll {
            let mut s = self.script.borrow_mut();
            if s.is_empty() {
                WsPoll::Idle
            } else {
                s.remove(0)
            }
        }
    }

    /// A connector that hands out pre-built scripted connections per `connect` call and records how many
    /// connections each tier asked for (so reconnects are observable).
    struct FakeConnector {
        /// Per-tier queue of scripts; each `connect` pops the next.
        scripts: RefCell<HashMap<StreamTier, Vec<Vec<WsPoll>>>>,
        connects: Rc<RefCell<HashMap<StreamTier, usize>>>,
        subscribe_calls: Rc<RefCell<usize>>,
    }

    impl FakeConnector {
        fn new() -> Self {
            Self {
                scripts: RefCell::new(HashMap::new()),
                connects: Rc::new(RefCell::new(HashMap::new())),
                subscribe_calls: Rc::new(RefCell::new(0)),
            }
        }

        fn script(&self, tier: StreamTier, polls: Vec<Vec<WsPoll>>) {
            self.scripts.borrow_mut().insert(tier, polls);
        }
    }

    impl WsConnector for FakeConnector {
        fn connect(&self, tier: StreamTier) -> Result<Box<dyn WsConnection>, WsError> {
            *self.connects.borrow_mut().entry(tier).or_insert(0) += 1;
            let script = self
                .scripts
                .borrow_mut()
                .get_mut(&tier)
                .and_then(|q| {
                    if q.is_empty() {
                        None
                    } else {
                        Some(q.remove(0))
                    }
                })
                .unwrap_or_default();
            Ok(Box::new(FakeConnection {
                script: RefCell::new(script),
                subscribe_calls: Rc::clone(&self.subscribe_calls),
            }))
        }
    }

    fn msg(sub: &Subscription, t: i64) -> WsPoll {
        WsPoll::Message(StreamMessage {
            subscription: sub.clone(),
            event_time_ms: t,
            payload: format!("p{t}"),
        })
    }

    #[test]
    fn market_and_realtime_tiers_are_partitioned() {
        let connector = FakeConnector::new();
        let connects = Rc::clone(&connector.connects);
        let mut reg = ConnectionRegistry::new(connector);

        let kline = Subscription::kline(inst(), Resolution::M5);
        let mark = Subscription::mark_price(inst());
        reg.subscribe(StreamTier::Market, &[kline.clone(), mark.clone()])
            .unwrap();
        // Establish the Realtime partition independently (its streams arrive in QE-203).
        let rt = Subscription::new(
            InstrumentId::new("ETHUSDT").unwrap(),
            StreamChannel::MarkPrice,
        );
        reg.subscribe(StreamTier::Realtime, std::slice::from_ref(&rt))
            .unwrap();

        // Two distinct partitions, each with its own connection.
        assert_eq!(
            reg.active_tiers(),
            vec![StreamTier::Market, StreamTier::Realtime]
        );
        assert_eq!(connects.borrow().get(&StreamTier::Market), Some(&1));
        assert_eq!(connects.borrow().get(&StreamTier::Realtime), Some(&1));
        // Subscriptions don't bleed across the partition.
        let market_subs = reg.subscriptions(StreamTier::Market);
        assert!(market_subs.contains(&kline) && market_subs.contains(&mark));
        assert!(!market_subs.contains(&rt));
        assert_eq!(reg.subscriptions(StreamTier::Realtime), vec![rt]);
    }

    #[test]
    fn disconnect_reconnects_resubscribes_and_reports_the_gap() {
        let kline = Subscription::kline(inst(), Resolution::M5); // cadence 300_000ms
        let connector = FakeConnector::new();
        // First connection: two contiguous bars then a drop. Second (post-reconnect) connection: a bar
        // that resumes 3 cadences later — a 2-bar hole.
        connector.script(
            StreamTier::Market,
            vec![
                vec![
                    msg(&kline, 300_000),
                    msg(&kline, 600_000),
                    WsPoll::Disconnected,
                ],
                vec![msg(&kline, 1_500_000)],
            ],
        );
        let connects = Rc::clone(&connector.connects);
        let subscribe_calls = Rc::clone(&connector.subscribe_calls);
        let mut reg = ConnectionRegistry::new(connector);
        reg.subscribe(StreamTier::Market, std::slice::from_ref(&kline))
            .unwrap();
        assert_eq!(*subscribe_calls.borrow(), 1, "initial subscribe");

        // Two contiguous messages, no gap.
        let a = reg.pump(StreamTier::Market).unwrap();
        assert_eq!(a.message.unwrap().event_time_ms, 300_000);
        assert!(a.gap.is_none());
        let b = reg.pump(StreamTier::Market).unwrap();
        assert_eq!(b.message.unwrap().event_time_ms, 600_000);
        assert!(b.gap.is_none());

        // Disconnect → reconnect + resubscribe (no message yet).
        let r = reg.pump(StreamTier::Market).unwrap();
        assert!(r.reconnected && r.message.is_none() && r.gap.is_none());
        assert_eq!(
            connects.borrow()[&StreamTier::Market],
            2,
            "reconnected once"
        );
        assert_eq!(
            *subscribe_calls.borrow(),
            2,
            "resubscribed on the fresh socket"
        );

        // The resume message reports the outage gap (600_000 → 1_500_000, missed 900_000ms = 2 bars > cadence).
        let c = reg.pump(StreamTier::Market).unwrap();
        assert_eq!(c.message.unwrap().event_time_ms, 1_500_000);
        let gap = c.gap.expect("a gap across the outage must be reported");
        assert_eq!(gap.from_ms, 600_000);
        assert_eq!(gap.to_ms, 1_500_000);
        assert_eq!(gap.missed_ms(), 900_000);
    }

    #[test]
    fn contiguous_resume_reports_no_gap() {
        let kline = Subscription::kline(inst(), Resolution::M5);
        let connector = FakeConnector::new();
        connector.script(
            StreamTier::Market,
            vec![
                vec![msg(&kline, 300_000), WsPoll::Disconnected],
                vec![msg(&kline, 600_000)], // exactly one cadence later — contiguous
            ],
        );
        let mut reg = ConnectionRegistry::new(connector);
        reg.subscribe(StreamTier::Market, &[kline]).unwrap();
        reg.pump(StreamTier::Market).unwrap(); // 300_000
        reg.pump(StreamTier::Market).unwrap(); // reconnect
        let c = reg.pump(StreamTier::Market).unwrap();
        assert_eq!(c.message.unwrap().event_time_ms, 600_000);
        assert!(
            c.gap.is_none(),
            "a one-cadence resume is contiguous, not a gap"
        );
    }

    #[test]
    fn in_stream_skip_is_reported_without_a_disconnect() {
        let mark = Subscription::mark_price(inst()); // cadence 1_000ms
        let connector = FakeConnector::new();
        connector.script(
            StreamTier::Market,
            vec![vec![msg(&mark, 1_000), msg(&mark, 5_000)]], // jump of 4_000 > 1_000
        );
        let mut reg = ConnectionRegistry::new(connector);
        reg.subscribe(StreamTier::Market, &[mark]).unwrap();
        reg.pump(StreamTier::Market).unwrap();
        let skip = reg.pump(StreamTier::Market).unwrap();
        let gap = skip.gap.expect("an in-stream skip is a gap");
        assert_eq!((gap.from_ms, gap.to_ms), (1_000, 5_000));
        assert!(!skip.reconnected);
    }

    #[test]
    fn pump_on_an_unopened_tier_is_a_noop() {
        let mut reg = ConnectionRegistry::new(FakeConnector::new());
        let out = reg.pump(StreamTier::Realtime).unwrap();
        assert!(out.message.is_none() && out.gap.is_none() && !out.reconnected);
    }
}
