//! The private **user-data stream**: fills, position reports, and heartbeat — the authoritative
//! ground-truth feed for the Position keeper (QE-217).
//!
//! Unlike the tier-partitioned market streams ([`crate::registry`]), the user-data stream is a single
//! **private** connection keyed by a venue **listen key** with its own lifecycle: create → keep-alive →
//! expire/renew. This module models that lifecycle over three deterministic seams — [`ListenKeyProvider`]
//! (create/keepalive the key), [`UserDataConnector`] (open a socket for a key), and
//! [`PositionSnapshotSource`] (REST position truth) — with a pull-based [`UserDataConnection::poll`] so the
//! tested core is single-threaded and deterministic, exactly like QE-202.
//!
//! The [`UserDataSession`] orchestrator ties them together: on connect it establishes an initial position
//! snapshot; on a disconnect **or** a listen-key expiry it **renews the key, reconnects, and re-snapshots**
//! so position truth is never lost across a gap (the AC). The concrete signed-REST listen-key client and
//! the real async wss adapter are deferred to runtime wiring (mirroring QE-202's deferred socket adapter);
//! a concrete [`sim::ScriptedUserData`] drives the whole loop in sim mode with no real venue.

use thiserror::Error;

use qe_domain::{Direction, InstrumentId, Price, Qty, Side};

/// An opaque venue listen key naming a private user-data stream.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ListenKey(String);

impl ListenKey {
    /// Wrap a raw listen-key string.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// The raw key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A user-data stream failure.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum UserDataError {
    /// Creating or keeping alive the listen key failed.
    #[error("listen-key error: {0}")]
    ListenKey(String),
    /// Opening the user-data socket failed.
    #[error("user-data connect error: {0}")]
    Connect(String),
    /// Taking the REST position snapshot failed.
    #[error("position snapshot error: {0}")]
    Snapshot(String),
}

/// A fill (order/trade update) on the private stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fill {
    /// The instrument.
    pub instrument: InstrumentId,
    /// Buy/sell.
    pub side: Side,
    /// Fill price.
    pub price: Price,
    /// Fill quantity.
    pub qty: Qty,
    /// Venue order id.
    pub order_id: u64,
    /// Venue trade id.
    pub trade_id: u64,
    /// Venue event time (epoch ms).
    pub event_time_ms: i64,
}

/// One instrument's position from an account update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionReport {
    /// The instrument.
    pub instrument: InstrumentId,
    /// Position direction, or `None` when flat (`Qty` is non-negative, so the sign lives here).
    pub direction: Option<Direction>,
    /// Absolute position size (0 when flat).
    pub qty: Qty,
    /// Average entry price (0 when flat).
    pub entry_price: Price,
    /// Venue event time (epoch ms).
    pub event_time_ms: i64,
}

/// A full set of position reports taken via REST at (re)connect — the position-truth carrier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionSnapshot {
    /// One report per instrument the account holds a position in.
    pub positions: Vec<PositionReport>,
    /// Venue event time the snapshot reflects (epoch ms).
    pub event_time_ms: i64,
}

/// A decoded event from the private stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserDataEvent {
    /// An order/trade fill.
    Fill(Fill),
    /// A single-instrument position update.
    Position(PositionReport),
    /// A keep-alive heartbeat.
    Heartbeat {
        /// Venue event time (epoch ms).
        event_time_ms: i64,
    },
    /// The venue signalled the listen key has expired — the session must renew + reconnect.
    ListenKeyExpired {
        /// Venue event time (epoch ms).
        event_time_ms: i64,
    },
    /// A full position snapshot (re-)established via REST on (re)connect.
    Snapshot(PositionSnapshot),
}

/// The outcome of polling a [`UserDataConnection`] once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserDataPoll {
    /// A decoded event.
    Event(UserDataEvent),
    /// The socket dropped — the session must renew the key, reconnect, and re-snapshot.
    Disconnected,
    /// No data available right now.
    Idle,
}

/// Creates and keeps alive the venue listen key that names the private stream.
pub trait ListenKeyProvider {
    /// Create a fresh listen key (subaccount-scoped by the concrete provider).
    ///
    /// # Errors
    /// [`UserDataError::ListenKey`] on failure.
    fn create(&self) -> Result<ListenKey, UserDataError>;

    /// Keep `key` alive (extend its TTL) without opening a new stream.
    ///
    /// # Errors
    /// [`UserDataError::ListenKey`] on failure.
    fn keepalive(&self, key: &ListenKey) -> Result<(), UserDataError>;
}

/// Opens a private user-data socket for a listen key.
pub trait UserDataConnector {
    /// Connect a fresh socket bound to `key`.
    ///
    /// # Errors
    /// [`UserDataError::Connect`] on failure.
    fn connect(&self, key: &ListenKey) -> Result<Box<dyn UserDataConnection>, UserDataError>;
}

/// One private user-data connection. The single network seam (pull-based, deterministic in tests).
pub trait UserDataConnection {
    /// Poll the next event (event / disconnect / idle).
    fn poll(&mut self) -> UserDataPoll;
}

/// Supplies the REST position snapshot used to (re-)establish position truth on (re)connect.
pub trait PositionSnapshotSource {
    /// Take a full position snapshot (e.g. REST `positionRisk`/account).
    ///
    /// # Errors
    /// [`UserDataError::Snapshot`] on failure.
    fn snapshot(&self) -> Result<PositionSnapshot, UserDataError>;
}

/// What a single [`UserDataSession::pump`] produced.
#[derive(Debug, Default)]
pub struct UserDataOutcome {
    /// A delivered event, if one was available (or the re-snapshot after a reconnect).
    pub event: Option<UserDataEvent>,
    /// Whether this pump renewed the key + reconnected (the re-snapshot is carried in `event`).
    pub reconnected: bool,
}

/// Orchestrates the private user-data stream: listen-key lifecycle, connection, reconnect-with-renewal,
/// and re-snapshot on reconnect.
pub struct UserDataSession<P, K, S>
where
    P: ListenKeyProvider,
    K: UserDataConnector,
    S: PositionSnapshotSource,
{
    provider: P,
    connector: K,
    snapshots: S,
    key: Option<ListenKey>,
    conn: Option<Box<dyn UserDataConnection>>,
}

impl<P, K, S> UserDataSession<P, K, S>
where
    P: ListenKeyProvider,
    K: UserDataConnector,
    S: PositionSnapshotSource,
{
    /// A session over the three seams. Call [`connect`](Self::connect) before pumping.
    pub fn new(provider: P, connector: K, snapshots: S) -> Self {
        Self {
            provider,
            connector,
            snapshots,
            key: None,
            conn: None,
        }
    }

    /// Establish the stream: create a listen key, open the socket, and take the initial position
    /// snapshot. The snapshot is returned so the caller establishes position truth from the start.
    ///
    /// This is the **initial-establish** entry point. Calling it again re-establishes a fresh
    /// key/connection/snapshot (the same mechanism [`pump`](Self::pump) uses on reconnect), so it is
    /// normally called exactly once at startup.
    ///
    /// # Errors
    /// [`UserDataError`] if the key, connection, or snapshot fails.
    pub fn connect(&mut self) -> Result<PositionSnapshot, UserDataError> {
        self.establish()
    }

    /// The current listen key (if connected).
    #[must_use]
    pub fn listen_key(&self) -> Option<&ListenKey> {
        self.key.as_ref()
    }

    /// Keep the current listen key alive. No-op if not yet connected.
    ///
    /// # Errors
    /// [`UserDataError::ListenKey`] if the keep-alive fails.
    pub fn keepalive(&mut self) -> Result<(), UserDataError> {
        if let Some(key) = &self.key {
            self.provider.keepalive(key)?;
        }
        Ok(())
    }

    /// Poll the stream once. Delivers the next event in order, or — on a disconnect or a listen-key
    /// expiry — renews the key, reconnects, re-snapshots, and surfaces the fresh
    /// [`UserDataEvent::Snapshot`] so position truth is re-established without loss.
    ///
    /// # Errors
    /// [`UserDataError`] if a renewal, reconnect, or re-snapshot fails.
    pub fn pump(&mut self) -> Result<UserDataOutcome, UserDataError> {
        let poll = match &mut self.conn {
            Some(conn) => conn.poll(),
            None => return Ok(UserDataOutcome::default()),
        };

        match poll {
            UserDataPoll::Idle => Ok(UserDataOutcome::default()),
            // An expired key means the socket's data can no longer be trusted — take the same safe path
            // as a hard disconnect: renew + reconnect + re-snapshot.
            UserDataPoll::Event(UserDataEvent::ListenKeyExpired { .. })
            | UserDataPoll::Disconnected => {
                let snapshot = self.establish()?;
                Ok(UserDataOutcome {
                    event: Some(UserDataEvent::Snapshot(snapshot)),
                    reconnected: true,
                })
            }
            UserDataPoll::Event(event) => Ok(UserDataOutcome {
                event: Some(event),
                reconnected: false,
            }),
        }
    }

    /// Create a fresh key, connect, and snapshot — the shared connect/reconnect path. Replaces any
    /// existing key/connection (renewal on reconnect). Only mutates `self` after all three seam calls
    /// succeed, so a mid-reconnect failure leaves the prior key/connection intact for the next `pump`.
    fn establish(&mut self) -> Result<PositionSnapshot, UserDataError> {
        let key = self.provider.create()?;
        let conn = self.connector.connect(&key)?;
        let snapshot = self.snapshots.snapshot()?;
        self.key = Some(key);
        self.conn = Some(conn);
        Ok(snapshot)
    }
}

/// A deterministic, in-memory user-data implementation for **sim mode** (not test-only): scripted
/// connections, a queue of snapshots handed out per connect, and monotonic listen keys. Drives a
/// [`UserDataSession`] through the full fills/positions/heartbeat + reconnect loop with no real venue.
pub mod sim {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::{
        ListenKey, ListenKeyProvider, PositionSnapshot, PositionSnapshotSource, UserDataConnection,
        UserDataConnector, UserDataError, UserDataPoll,
    };

    /// Shared, cloneable script state so one `ScriptedUserData` value can back all three seams.
    #[derive(Clone, Default)]
    pub struct ScriptedUserData {
        inner: Rc<Inner>,
    }

    #[derive(Default)]
    struct Inner {
        /// Per-connect connection scripts; each `connect` pops the next.
        conn_scripts: RefCell<Vec<Vec<UserDataPoll>>>,
        /// Per-connect snapshots; each `snapshot()` pops the next (last repeats once drained).
        snapshots: RefCell<Vec<PositionSnapshot>>,
        /// Monotonic listen-key counter.
        keys_created: RefCell<u64>,
        /// Keep-alive call count (observability for tests / sim assertions).
        keepalives: RefCell<u64>,
    }

    impl ScriptedUserData {
        /// An empty sim source.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Queue a connection script (one `Vec<UserDataPoll>` served per `connect`).
        #[must_use]
        pub fn with_connection(self, polls: Vec<UserDataPoll>) -> Self {
            self.inner.conn_scripts.borrow_mut().push(polls);
            self
        }

        /// Queue a snapshot handed out on the next `snapshot()` (the last queued repeats once drained).
        #[must_use]
        pub fn with_snapshot(self, snapshot: PositionSnapshot) -> Self {
            self.inner.snapshots.borrow_mut().push(snapshot);
            self
        }

        /// How many listen keys have been created (renewals are observable).
        #[must_use]
        pub fn keys_created(&self) -> u64 {
            *self.inner.keys_created.borrow()
        }

        /// How many keep-alives have been issued.
        #[must_use]
        pub fn keepalives(&self) -> u64 {
            *self.inner.keepalives.borrow()
        }
    }

    /// A scripted connection: serves a queue of poll outcomes, then `Idle`.
    struct ScriptedConnection {
        script: Vec<UserDataPoll>,
        pos: usize,
    }

    impl UserDataConnection for ScriptedConnection {
        fn poll(&mut self) -> UserDataPoll {
            if self.pos < self.script.len() {
                let out = self.script[self.pos].clone();
                self.pos += 1;
                out
            } else {
                UserDataPoll::Idle
            }
        }
    }

    impl ListenKeyProvider for ScriptedUserData {
        fn create(&self) -> Result<ListenKey, UserDataError> {
            let mut n = self.inner.keys_created.borrow_mut();
            *n += 1;
            Ok(ListenKey::new(format!("sim-key-{n}")))
        }

        fn keepalive(&self, _key: &ListenKey) -> Result<(), UserDataError> {
            *self.inner.keepalives.borrow_mut() += 1;
            Ok(())
        }
    }

    impl UserDataConnector for ScriptedUserData {
        fn connect(&self, _key: &ListenKey) -> Result<Box<dyn UserDataConnection>, UserDataError> {
            let mut scripts = self.inner.conn_scripts.borrow_mut();
            let script = if scripts.is_empty() {
                Vec::new()
            } else {
                scripts.remove(0)
            };
            Ok(Box::new(ScriptedConnection { script, pos: 0 }))
        }
    }

    impl PositionSnapshotSource for ScriptedUserData {
        fn snapshot(&self) -> Result<PositionSnapshot, UserDataError> {
            let mut snaps = self.inner.snapshots.borrow_mut();
            let snap = if snaps.len() > 1 {
                Some(snaps.remove(0))
            } else {
                snaps.first().cloned()
            };
            snap.ok_or_else(|| UserDataError::Snapshot("no scripted snapshot".to_owned()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sim::ScriptedUserData;
    use super::*;
    use rust_decimal::Decimal;

    fn inst() -> InstrumentId {
        InstrumentId::new("BTCUSDT").unwrap()
    }

    fn price(v: i64) -> Price {
        Price::new(Decimal::from(v)).unwrap()
    }

    fn qty(v: i64) -> Qty {
        Qty::new(Decimal::from(v)).unwrap()
    }

    fn fill(trade_id: u64, t: i64) -> Fill {
        Fill {
            instrument: inst(),
            side: Side::Buy,
            price: price(30_000),
            qty: qty(1),
            order_id: 100,
            trade_id,
            event_time_ms: t,
        }
    }

    fn long_position(size: i64, t: i64) -> PositionReport {
        PositionReport {
            instrument: inst(),
            direction: Some(Direction::Long),
            qty: qty(size),
            entry_price: price(30_000),
            event_time_ms: t,
        }
    }

    fn snapshot(size: i64, t: i64) -> PositionSnapshot {
        PositionSnapshot {
            positions: vec![long_position(size, t)],
            event_time_ms: t,
        }
    }

    /// AC part 1: fills, positions, and heartbeat are delivered in the order they arrive.
    #[test]
    fn events_are_delivered_in_order() {
        let position = long_position(2, 1_002);
        let sim = ScriptedUserData::new()
            .with_snapshot(snapshot(0, 1_000))
            .with_connection(vec![
                UserDataPoll::Event(UserDataEvent::Fill(fill(1, 1_001))),
                UserDataPoll::Event(UserDataEvent::Position(position.clone())),
                UserDataPoll::Event(UserDataEvent::Heartbeat {
                    event_time_ms: 1_003,
                }),
            ]);
        let mut session = UserDataSession::new(sim.clone(), sim.clone(), sim);
        session.connect().unwrap();

        let a = session.pump().unwrap();
        assert_eq!(a.event, Some(UserDataEvent::Fill(fill(1, 1_001))));
        assert!(!a.reconnected);
        let b = session.pump().unwrap();
        assert_eq!(b.event, Some(UserDataEvent::Position(position)));
        let c = session.pump().unwrap();
        assert_eq!(
            c.event,
            Some(UserDataEvent::Heartbeat {
                event_time_ms: 1_003
            })
        );
        // Nothing left → idle, no event.
        let d = session.pump().unwrap();
        assert!(d.event.is_none() && !d.reconnected);
    }

    /// AC part 2: a dropped stream reconnects, **renews the listen key**, and re-snapshots so position
    /// truth is not lost across the gap.
    #[test]
    fn reconnect_renews_key_and_resnapshots_without_losing_position_truth() {
        // Snapshot A at connect (flat); after the drop the position has moved to size 5 (snapshot B).
        let sim = ScriptedUserData::new()
            .with_snapshot(snapshot(0, 1_000)) // initial truth (flat)
            .with_snapshot(snapshot(5, 2_000)) // post-reconnect truth (long 5)
            .with_connection(vec![
                UserDataPoll::Event(UserDataEvent::Fill(fill(1, 1_001))),
                UserDataPoll::Disconnected,
            ])
            .with_connection(vec![]); // fresh socket after reconnect
        let mut session = UserDataSession::new(sim.clone(), sim.clone(), sim.clone());

        let initial = session.connect().unwrap();
        assert_eq!(initial, snapshot(0, 1_000));
        let key1 = session.listen_key().unwrap().clone();
        assert_eq!(sim.keys_created(), 1);

        // Fill delivered.
        let a = session.pump().unwrap();
        assert_eq!(a.event, Some(UserDataEvent::Fill(fill(1, 1_001))));

        // Disconnect → renew key + reconnect + re-snapshot; the fresh snapshot re-establishes truth.
        let b = session.pump().unwrap();
        assert!(b.reconnected, "a dropped stream must reconnect");
        assert_eq!(
            b.event,
            Some(UserDataEvent::Snapshot(snapshot(5, 2_000))),
            "position truth is re-established from a fresh snapshot"
        );
        let key2 = session.listen_key().unwrap().clone();
        assert_ne!(key1, key2, "reconnect renews the listen key");
        assert_eq!(sim.keys_created(), 2);
    }

    /// A listen-key expiry event drives the same renew + re-snapshot path as a disconnect.
    #[test]
    fn listen_key_expiry_triggers_renew_and_resnapshot() {
        let sim = ScriptedUserData::new()
            .with_snapshot(snapshot(0, 1_000))
            .with_snapshot(snapshot(3, 2_000))
            .with_connection(vec![UserDataPoll::Event(UserDataEvent::ListenKeyExpired {
                event_time_ms: 1_500,
            })])
            .with_connection(vec![]);
        let mut session = UserDataSession::new(sim.clone(), sim.clone(), sim.clone());
        session.connect().unwrap();
        assert_eq!(sim.keys_created(), 1);

        let out = session.pump().unwrap();
        assert!(out.reconnected);
        assert_eq!(out.event, Some(UserDataEvent::Snapshot(snapshot(3, 2_000))));
        assert_eq!(sim.keys_created(), 2, "expiry renews the listen key");
    }

    /// `keepalive()` extends the current key without reconnecting.
    #[test]
    fn keepalive_holds_the_current_key() {
        let sim = ScriptedUserData::new()
            .with_snapshot(snapshot(0, 1_000))
            .with_connection(vec![]);
        let mut session = UserDataSession::new(sim.clone(), sim.clone(), sim.clone());
        // No key yet → keepalive is a no-op.
        session.keepalive().unwrap();
        assert_eq!(sim.keepalives(), 0);

        session.connect().unwrap();
        session.keepalive().unwrap();
        session.keepalive().unwrap();
        assert_eq!(sim.keepalives(), 2);
        assert_eq!(sim.keys_created(), 1, "keepalive does not create a new key");
    }

    /// Pumping before connect (no socket) is a no-op, not a panic.
    #[test]
    fn pump_before_connect_is_a_noop() {
        let sim = ScriptedUserData::new().with_snapshot(snapshot(0, 0));
        let mut session = UserDataSession::new(sim.clone(), sim.clone(), sim);
        let out = session.pump().unwrap();
        assert!(out.event.is_none() && !out.reconnected);
    }

    /// The sim implementation drives a full session end-to-end (the sim-mode equivalent is usable).
    #[test]
    fn sim_scripted_user_data_drives_a_full_session() {
        let sim = ScriptedUserData::new()
            .with_snapshot(snapshot(0, 1_000))
            .with_snapshot(snapshot(1, 2_000))
            .with_connection(vec![
                UserDataPoll::Event(UserDataEvent::Fill(fill(7, 1_100))),
                UserDataPoll::Event(UserDataEvent::Heartbeat {
                    event_time_ms: 1_200,
                }),
                UserDataPoll::Disconnected,
            ])
            .with_connection(vec![UserDataPoll::Event(UserDataEvent::Position(
                long_position(1, 2_100),
            ))]);
        let mut session = UserDataSession::new(sim.clone(), sim.clone(), sim.clone());

        assert_eq!(session.connect().unwrap(), snapshot(0, 1_000));
        assert_eq!(
            session.pump().unwrap().event,
            Some(UserDataEvent::Fill(fill(7, 1_100)))
        );
        assert_eq!(
            session.pump().unwrap().event,
            Some(UserDataEvent::Heartbeat {
                event_time_ms: 1_200
            })
        );
        // Disconnect → snapshot B.
        let reconnected = session.pump().unwrap();
        assert!(reconnected.reconnected);
        assert_eq!(
            reconnected.event,
            Some(UserDataEvent::Snapshot(snapshot(1, 2_000)))
        );
        // Post-reconnect position event flows on the fresh socket.
        assert_eq!(
            session.pump().unwrap().event,
            Some(UserDataEvent::Position(long_position(1, 2_100)))
        );
        assert_eq!(sim.keys_created(), 2);
    }
}
