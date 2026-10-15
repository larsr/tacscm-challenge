//! Internet-facing session adapter for one authoritative six-agent match.
//!
//! The economic engine remains free of sockets and clocks. This module owns
//! authentication, bounded outer framing, remote connection lifecycle, and a
//! read-only event fan-out before reducing all traffic through the same runner
//! session boundary as local subprocesses. An agent whose connection is lost
//! may authenticate again within the match; the server then replays the
//! draft1 section 10 recovery sequence on the new connection.

use std::collections::BTreeSet;
use std::io;
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use tac_scm_engine_core::cbor::Value;
use tac_scm_engine_core::crypto::{hex, sha256};
use tac_scm_engine_core::game::{GameEngine, GamePlan};

use super::{
    AgentArtifact, AgentSession, AuditEvidence, FrozenGamePlan, OperationalPolicy, ReadEvent,
    RunnerError, RunnerOutput, as_map, decode_canonical, get, get_text, get_u64, read_frame,
    run_game_with_sessions, write_frame,
};

pub const SESSION_SCHEMA_ID: &str = "tac-scm-session/1";
pub const DEFAULT_AUTH_FRAME_BYTES: usize = 4 * 1024;
const SESSION_QUEUE_DEPTH: usize = 512;
const SPECTATOR_QUEUE_DEPTH: usize = 524_288;
const SPECTATOR_QUEUE_BYTES: usize = 384 * 1024 * 1024;
const MAX_SPECTATORS: usize = 1;
const SPECTATOR_TERMINAL_DRAIN: Duration = Duration::from_secs(120);

struct SpectatorQueueState {
    queued_bytes: AtomicUsize,
    peak_bytes: AtomicUsize,
    drain_lock: Mutex<()>,
    changed: Condvar,
}

struct SpectatorSender {
    sender: SyncSender<Vec<u8>>,
    state: Arc<SpectatorQueueState>,
}

impl SpectatorSender {
    fn try_send(&self, frame: Vec<u8>) -> bool {
        let size = frame.len();
        let mut current = self.state.queued_bytes.load(Ordering::Acquire);
        loop {
            let Some(next) = current.checked_add(size) else {
                eprintln!("remote spectator dropped: queue byte counter overflow");
                return false;
            };
            if next > SPECTATOR_QUEUE_BYTES {
                eprintln!(
                    "remote spectator dropped: queue byte limit (queued={current}, frame={size}, limit={SPECTATOR_QUEUE_BYTES})"
                );
                return false;
            }
            match self.state.queued_bytes.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
        self.state
            .peak_bytes
            .fetch_max(current + size, Ordering::AcqRel);
        if self.sender.try_send(frame).is_err() {
            self.state.queued_bytes.fetch_sub(size, Ordering::AcqRel);
            eprintln!("remote spectator dropped: writer unavailable or frame queue full");
            return false;
        }
        true
    }

    fn wait_until_drained(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let Ok(mut guard) = self.state.drain_lock.lock() else {
            return false;
        };
        while self.state.queued_bytes.load(Ordering::Acquire) != 0 {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let Ok((next_guard, result)) = self.state.changed.wait_timeout(guard, deadline - now)
            else {
                return false;
            };
            guard = next_guard;
            if result.timed_out() && self.state.queued_bytes.load(Ordering::Acquire) != 0 {
                return false;
            }
        }
        true
    }
}

#[derive(Clone, Debug)]
pub struct RemoteServerConfig {
    pub bind: SocketAddr,
    pub output: PathBuf,
    pub engine_plan: GamePlan,
    pub policy: OperationalPolicy,
    pub agent_tokens: [String; 6],
    pub spectator_token: String,
    pub registration_timeout: Duration,
    pub authentication_timeout: Duration,
    pub max_pending_authentications: usize,
    /// Must be true for a non-loopback bind. The service itself deliberately
    /// has no certificate or TLS implementation.
    pub tls_terminated: bool,
}

impl RemoteServerConfig {
    #[must_use]
    pub fn loopback(
        bind: SocketAddr,
        output: PathBuf,
        engine_plan: GamePlan,
        agent_tokens: [String; 6],
        spectator_token: String,
    ) -> Self {
        Self {
            bind,
            output,
            engine_plan,
            policy: OperationalPolicy::default(),
            agent_tokens,
            spectator_token,
            registration_timeout: Duration::from_secs(60),
            authentication_timeout: Duration::from_secs(5),
            max_pending_authentications: 32,
            tls_terminated: false,
        }
    }
}

#[derive(Debug)]
pub struct RemoteMatchReport {
    pub listen_address: SocketAddr,
    pub plan: FrozenGamePlan,
    pub output: RunnerOutput,
    pub spectator_queue_peak_bytes: usize,
}

pub struct RemoteMatchServer {
    listener: TcpListener,
    listen_address: SocketAddr,
    config: RemoteServerConfig,
}

impl RemoteMatchServer {
    pub fn bind(config: RemoteServerConfig) -> Result<Self, RunnerError> {
        validate_config(&config)?;
        let listener = TcpListener::bind(config.bind)?;
        listener.set_nonblocking(true)?;
        let listen_address = listener.local_addr()?;
        Ok(Self {
            listener,
            listen_address,
            config,
        })
    }

    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.listen_address
    }

    pub fn run(self) -> Result<RemoteMatchReport, RunnerError> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let spectator_registration_open = Arc::new(AtomicBool::new(true));
        let spectators = Arc::new(Mutex::new(Vec::<SpectatorSender>::new()));
        let pending = Arc::new(AtomicUsize::new(0));
        let frozen_artifacts = Arc::new(OnceLock::<[[u8; 32]; 6]>::new());
        let (agent_sender, agent_receiver) = mpsc::sync_channel(12);
        let listener = self.listener;
        let auth_config = self.config.clone();
        let accept_shutdown = Arc::clone(&shutdown);
        let accept_spectators = Arc::clone(&spectator_registration_open);
        let accept_hub = Arc::clone(&spectators);
        let accept_pending = Arc::clone(&pending);
        let accept_frozen = Arc::clone(&frozen_artifacts);
        let accept_thread = thread::spawn(move || {
            accept_loop(
                listener,
                auth_config,
                agent_sender,
                accept_hub,
                accept_shutdown,
                accept_spectators,
                accept_pending,
                accept_frozen,
            );
        });

        let registration_deadline = Instant::now() + self.config.registration_timeout;
        let mut registrations: [Option<AgentRegistration>; 6] = std::array::from_fn(|_| None);
        while registrations.iter().any(Option::is_none) {
            let now = Instant::now();
            if now >= registration_deadline {
                shutdown.store(true, Ordering::Release);
                let _ = accept_thread.join();
                return Err(RunnerError::Protocol("remote registration timeout".into()));
            }
            let wait = (registration_deadline - now).min(Duration::from_millis(20));
            match agent_receiver.recv_timeout(wait) {
                Ok(registration) => {
                    // The newest authenticated connection for a slot wins, so
                    // an agent that reconnects before the match starts keeps
                    // its place.
                    let slot = registration.slot;
                    registrations[slot] = Some(registration);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    shutdown.store(true, Ordering::Release);
                    let _ = accept_thread.join();
                    return Err(RunnerError::Protocol(
                        "remote registration service stopped".into(),
                    ));
                }
            }
        }

        spectator_registration_open.store(false, Ordering::Release);
        let mut artifacts = Vec::with_capacity(6);
        let mut digests = [[0_u8; 32]; 6];
        let mut sessions: Vec<Box<dyn AgentSession>> = Vec::with_capacity(6);
        let mut reconnect_senders = Vec::with_capacity(6);
        for registration in registrations.into_iter().flatten() {
            digests[registration.slot] = registration.artifact_sha256;
            artifacts.push(AgentArtifact::remote(
                registration.agent_id,
                registration.artifact_sha256,
            ));
            let (sender, receiver) = mpsc::channel();
            reconnect_senders.push(sender);
            sessions.push(Box::new(RemoteAgentSession::new(
                registration.stream,
                registration.slot,
                self.config.policy.max_frame_bytes,
                receiver,
            )?));
        }
        // Later agent authentications are reconnects. They must present the
        // frozen artifact identity of their slot and are routed to it.
        let _ = frozen_artifacts.set(digests);
        thread::spawn(move || route_reconnects(&agent_receiver, &reconnect_senders));
        let plan = FrozenGamePlan::freeze(
            self.config.engine_plan,
            artifacts
                .try_into()
                .map_err(|_| RunnerError::Plan("remote artifacts"))?,
            self.config.policy,
            "remote-match-v1",
            0,
        )?;
        broadcast_status(&spectators, "match-starting", &plan.game_definition_id);

        let spectator_count = spectators.lock().map_or(0, |hub| hub.len());
        let (engine, delivery) = if spectator_count == 0 {
            (GameEngine::new(plan.engine_plan.clone())?, "off")
        } else {
            let event_hub = Arc::clone(&spectators);
            let game_id = plan.game_definition_id.clone();
            (
                GameEngine::new_with_event_stream(plan.engine_plan.clone(), move |event| {
                    broadcast_event(&event_hub, &game_id, event);
                })?,
                "stream",
            )
        };
        let result =
            run_game_with_sessions(&plan, engine, AuditEvidence::Anomalies, delivery, sessions);
        shutdown.store(true, Ordering::Release);
        let _ = accept_thread.join();
        let output = result?;
        broadcast_status(
            &spectators,
            "match-finished",
            &format!("{}/{}", plan.game_definition_id, output.game_result_id),
        );
        output.write_summary_to(&plan, &self.config.output)?;
        let spectator_queue_peak_bytes =
            wait_for_spectator_drain(&spectators, SPECTATOR_TERMINAL_DRAIN);
        Ok(RemoteMatchReport {
            listen_address: self.listen_address,
            plan,
            output,
            spectator_queue_peak_bytes,
        })
    }
}

fn validate_config(config: &RemoteServerConfig) -> Result<(), RunnerError> {
    if !config.bind.ip().is_loopback() && !config.tls_terminated {
        return Err(RunnerError::Plan(
            "non-loopback remote service requires declared TLS termination",
        ));
    }
    if config.registration_timeout.is_zero()
        || config.authentication_timeout.is_zero()
        || config.max_pending_authentications == 0
    {
        return Err(RunnerError::Plan("remote service limit"));
    }
    let mut credentials = BTreeSet::new();
    for token in &config.agent_tokens {
        if token.len() < 16 || !credentials.insert(token.as_bytes()) {
            return Err(RunnerError::Plan("remote agent credential"));
        }
    }
    if config.spectator_token.len() < 16 || !credentials.insert(config.spectator_token.as_bytes()) {
        return Err(RunnerError::Plan("remote spectator credential"));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn accept_loop(
    listener: TcpListener,
    config: RemoteServerConfig,
    agents: SyncSender<AgentRegistration>,
    spectators: Arc<Mutex<Vec<SpectatorSender>>>,
    shutdown: Arc<AtomicBool>,
    spectator_registration_open: Arc<AtomicBool>,
    pending: Arc<AtomicUsize>,
    frozen_artifacts: Arc<OnceLock<[[u8; 32]; 6]>>,
) {
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                if pending.fetch_add(1, Ordering::AcqRel) >= config.max_pending_authentications {
                    pending.fetch_sub(1, Ordering::AcqRel);
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let worker_config = config.clone();
                let worker_agents = agents.clone();
                let worker_spectators = Arc::clone(&spectators);
                let worker_open = Arc::clone(&spectator_registration_open);
                let worker_pending = Arc::clone(&pending);
                let worker_frozen = Arc::clone(&frozen_artifacts);
                thread::spawn(move || {
                    authenticate_connection(
                        stream,
                        &worker_config,
                        &worker_agents,
                        &worker_spectators,
                        worker_open.load(Ordering::Acquire),
                        worker_frozen.get(),
                    );
                    worker_pending.fetch_sub(1, Ordering::AcqRel);
                });
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(2));
            }
            Err(_) => break,
        }
    }
}

fn authenticate_connection(
    mut stream: TcpStream,
    config: &RemoteServerConfig,
    agents: &SyncSender<AgentRegistration>,
    spectators: &Arc<Mutex<Vec<SpectatorSender>>>,
    spectator_registration_open: bool,
    frozen_artifacts: Option<&[[u8; 32]; 6]>,
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(config.authentication_timeout));
    let _ = stream.set_write_timeout(Some(config.authentication_timeout));
    let raw = match read_frame(&mut stream, DEFAULT_AUTH_FRAME_BYTES) {
        Ok(Some(raw)) => raw,
        _ => return,
    };
    let authenticated = parse_authentication(&raw, config, spectator_registration_open).and_then(
        |authentication| match (&authentication, frozen_artifacts) {
            (
                Authentication::Agent {
                    slot,
                    artifact_sha256,
                    ..
                },
                Some(frozen),
            ) if frozen[*slot] != *artifact_sha256 => Err("reconnect artifact identity".into()),
            _ => Ok(authentication),
        },
    );
    match authenticated {
        Ok(Authentication::Agent {
            slot,
            agent_id,
            artifact_sha256,
        }) => {
            if write_session_frame(&mut stream, &status_value("authenticated", &agent_id)).is_err()
            {
                return;
            }
            let _ = stream.set_read_timeout(None);
            let _ = stream.set_write_timeout(None);
            let _ = stream.set_nonblocking(false);
            let _ = agents.try_send(AgentRegistration {
                slot,
                agent_id,
                artifact_sha256,
                stream,
            });
        }
        Ok(Authentication::Spectator) => {
            let Ok(mut hub) = spectators.lock() else {
                return;
            };
            if hub.len() >= MAX_SPECTATORS {
                let _ = write_session_frame(
                    &mut stream,
                    &status_value("authentication-rejected", "spectator capacity reached"),
                );
                let _ = stream.shutdown(Shutdown::Both);
                return;
            }
            if write_session_frame(&mut stream, &status_value("authenticated", "spectator"))
                .is_err()
            {
                return;
            }
            let _ = stream.set_write_timeout(None);
            if let Ok(sender) = spectator_writer(stream) {
                hub.push(sender);
            }
        }
        Err(_) => {
            let _ = write_session_frame(
                &mut stream,
                &status_value("authentication-rejected", "invalid credentials or claim"),
            );
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

enum Authentication {
    Agent {
        slot: usize,
        agent_id: String,
        artifact_sha256: [u8; 32],
    },
    Spectator,
}

fn parse_authentication(
    raw: &[u8],
    config: &RemoteServerConfig,
    spectator_registration_open: bool,
) -> Result<Authentication, String> {
    let value = decode_canonical(raw)?;
    let map = as_map(&value)?;
    if get_text(map, "schema_id")? != SESSION_SCHEMA_ID || get_text(map, "kind")? != "authenticate"
    {
        return Err("session identity".into());
    }
    let role = get_text(map, "role")?;
    let token = get_text(map, "token")?;
    match role {
        "agent" => {
            require_keys(
                map,
                &[
                    "schema_id",
                    "kind",
                    "role",
                    "token",
                    "slot",
                    "agent_id",
                    "artifact_sha256",
                ],
            )?;
            let slot =
                usize::try_from(get_u64(map, "slot")?).map_err(|_| "slot range".to_owned())?;
            if slot >= 6 || !constant_time_equal(token, &config.agent_tokens[slot]) {
                return Err("credential".into());
            }
            let expected_id = format!("agent-{}", char::from(b'a' + u8::try_from(slot).unwrap()));
            let agent_id = get_text(map, "agent_id")?;
            if agent_id != expected_id {
                return Err("agent identity".into());
            }
            let Value::Bytes(digest) = get(map, "artifact_sha256")? else {
                return Err("artifact digest".into());
            };
            let artifact_sha256: [u8; 32] = digest
                .as_slice()
                .try_into()
                .map_err(|_| "artifact digest".to_owned())?;
            Ok(Authentication::Agent {
                slot,
                agent_id: agent_id.into(),
                artifact_sha256,
            })
        }
        "spectator" => {
            require_keys(map, &["schema_id", "kind", "role", "token"])?;
            if !spectator_registration_open || !constant_time_equal(token, &config.spectator_token)
            {
                return Err("credential".into());
            }
            Ok(Authentication::Spectator)
        }
        _ => Err("role".into()),
    }
}

fn require_keys(map: &[(String, Value)], expected: &[&str]) -> Result<(), String> {
    let actual: BTreeSet<_> = map.iter().map(|(key, _)| key.as_str()).collect();
    let expected: BTreeSet<_> = expected.iter().copied().collect();
    if actual != expected {
        return Err("session fields".into());
    }
    Ok(())
}

fn constant_time_equal(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

struct AgentRegistration {
    slot: usize,
    agent_id: String,
    artifact_sha256: [u8; 32],
    stream: TcpStream,
}

/// Forwards each in-match agent authentication to its slot's session. The
/// authentication worker has already checked the slot credential and the
/// frozen artifact identity.
fn route_reconnects(registrations: &Receiver<AgentRegistration>, slots: &[Sender<TcpStream>]) {
    while let Ok(registration) = registrations.recv() {
        if let Some(sender) = slots.get(registration.slot) {
            let _ = sender.send(registration.stream);
        }
    }
}

/// One live TCP connection of a remote agent slot.
struct RemoteConnection {
    control: TcpStream,
    input: Option<SyncSender<Vec<u8>>>,
    output: Receiver<ReadEvent>,
    pending_writes: Arc<AtomicUsize>,
}

impl RemoteConnection {
    fn new(stream: TcpStream, slot: usize, limit: usize) -> Result<Self, RunnerError> {
        stream.set_nodelay(true)?;
        let control = stream.try_clone()?;
        let mut writer = stream.try_clone()?;
        let (input_sender, input_receiver) = mpsc::sync_channel::<Vec<u8>>(SESSION_QUEUE_DEPTH);
        let pending_writes = Arc::new(AtomicUsize::new(0));
        let writer_pending = Arc::clone(&pending_writes);
        thread::spawn(move || {
            while let Ok(frame) = input_receiver.recv() {
                let failed = write_frame(&mut writer, &frame).is_err();
                writer_pending.fetch_sub(1, Ordering::AcqRel);
                if failed {
                    break;
                }
            }
        });
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || read_remote_agent(stream, slot, limit, &sender));
        Ok(Self {
            control,
            input: Some(input_sender),
            output: receiver,
            pending_writes,
        })
    }

    fn send_frame(&mut self, frame: Vec<u8>) -> Result<(), RunnerError> {
        let input = self
            .input
            .as_ref()
            .ok_or(RunnerError::Plan("remote agent input closed"))?;
        self.pending_writes.fetch_add(1, Ordering::AcqRel);
        let sent = input.try_send(frame).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => RunnerError::Limit("remote agent input backlog"),
            mpsc::TrySendError::Disconnected(_) => RunnerError::Plan("remote agent input closed"),
        });
        if sent.is_err() {
            self.pending_writes.fetch_sub(1, Ordering::AcqRel);
        }
        sent
    }

    fn close(&mut self) {
        self.input.take();
        let deadline = Instant::now() + Duration::from_millis(100);
        while self.pending_writes.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        let _ = self.control.shutdown(Shutdown::Both);
    }
}

impl Drop for RemoteConnection {
    fn drop(&mut self) {
        self.close();
    }
}

/// The latest frames of each recovery kind sent to one slot. They are replayed
/// byte for byte on a replacement connection, so recovery adds no message
/// identity and no engine input.
#[derive(Default)]
struct RecoveryFrames {
    bootstrap: Option<Vec<u8>>,
    committed: Option<(u64, Vec<u8>)>,
    observation: Option<(u64, Vec<u8>)>,
    receipt: Option<(u64, Vec<u8>)>,
}

impl RecoveryFrames {
    fn record(&mut self, message: &Value, frame: &[u8]) {
        let Ok(envelope) = as_map(message) else {
            return;
        };
        let day = get_u64(envelope, "game_day").ok();
        match get_text(envelope, "phase") {
            Ok("setup") => self.bootstrap = Some(frame.to_vec()),
            Ok("action-commit") => self.committed = day.map(|day| (day, frame.to_vec())),
            Ok("observation") => {
                self.observation = day.map(|day| (day, frame.to_vec()));
                self.receipt = None;
            }
            Ok("action-window") => {
                // The retained candidate is the highest valid revision of the
                // open window; replay its receipt, not a later rejected one.
                let Some(payload) = get(envelope, "payload")
                    .ok()
                    .and_then(|value| as_map(value).ok())
                else {
                    return;
                };
                if get(payload, "candidate_valid").ok() != Some(&Value::Bool(true)) {
                    return;
                }
                let Ok(revision) = get_u64(payload, "revision") else {
                    return;
                };
                if self
                    .receipt
                    .as_ref()
                    .is_none_or(|(retained, _)| revision > *retained)
                {
                    self.receipt = Some((revision, frame.to_vec()));
                }
            }
            _ => {}
        }
    }

    /// `GameBootstrap`, the latest `ActionCommitted`, and, while its action
    /// window is open, the latest `DayObservation` and retained receipt.
    fn sequence(&self) -> Vec<Vec<u8>> {
        let mut frames: Vec<Vec<u8>> = self.bootstrap.iter().cloned().collect();
        if let Some((_, frame)) = &self.committed {
            frames.push(frame.clone());
        }
        if let Some((day, frame)) = &self.observation
            && self
                .committed
                .as_ref()
                .is_none_or(|(committed, _)| day > committed)
        {
            frames.push(frame.clone());
            if let Some((_, receipt)) = &self.receipt {
                frames.push(receipt.clone());
            }
        }
        frames
    }
}

/// A remote agent slot. Its connection may be lost and replaced within the
/// match; while it is absent, frames are only recorded for recovery and the
/// runner sees a lost connection, so the slot defaults for that window.
struct RemoteAgentSession {
    slot: usize,
    limit: usize,
    connection: Option<RemoteConnection>,
    reconnects: Receiver<TcpStream>,
    recovery: RecoveryFrames,
    reconnected: bool,
    stopped: bool,
}

impl RemoteAgentSession {
    fn new(
        stream: TcpStream,
        slot: usize,
        limit: usize,
        reconnects: Receiver<TcpStream>,
    ) -> Result<Self, RunnerError> {
        Ok(Self {
            slot,
            limit,
            connection: Some(RemoteConnection::new(stream, slot, limit)?),
            reconnects,
            recovery: RecoveryFrames::default(),
            reconnected: false,
            stopped: false,
        })
    }

    /// Attach the newest pending replacement connection, if any, and replay
    /// the recovery sequence on it before any later frame.
    fn poll_reconnect(&mut self) {
        let mut latest = None;
        while let Ok(stream) = self.reconnects.try_recv() {
            latest = Some(stream);
        }
        let Some(stream) = latest else {
            return;
        };
        if self.stopped {
            let _ = stream.shutdown(Shutdown::Both);
            return;
        }
        self.connection = None;
        let Ok(mut connection) = RemoteConnection::new(stream, self.slot, self.limit) else {
            return;
        };
        for frame in self.recovery.sequence() {
            if connection.send_frame(frame).is_err() {
                return;
            }
        }
        self.connection = Some(connection);
        self.reconnected = true;
    }
}

impl AgentSession for RemoteAgentSession {
    fn send_value(&mut self, value: &Value) -> Result<(), RunnerError> {
        if self.stopped {
            return Err(RunnerError::Plan("remote agent input closed"));
        }
        let frame = logical_value(self.slot, value.encode()).encode();
        self.recovery.record(value, &frame);
        let Some(connection) = self.connection.as_mut() else {
            return Ok(());
        };
        match connection.send_frame(frame) {
            Ok(()) => Ok(()),
            Err(RunnerError::Limit(detail)) => {
                self.connection = None;
                Err(RunnerError::Limit(detail))
            }
            // A closed writer is a lost connection: the frame is kept for
            // recovery and the reader reports the loss to the runner.
            Err(_) => Ok(()),
        }
    }

    fn try_recv(&mut self) -> Result<ReadEvent, mpsc::TryRecvError> {
        self.poll_reconnect();
        self.reconnected = false;
        let Some(connection) = self.connection.as_mut() else {
            return Err(mpsc::TryRecvError::Disconnected);
        };
        let event = connection.output.try_recv();
        match &event {
            Ok(ReadEvent::Frame(_)) | Err(mpsc::TryRecvError::Empty) => {}
            Ok(ReadEvent::Eof | ReadEvent::Io(_) | ReadEvent::Oversize(_))
            | Err(mpsc::TryRecvError::Disconnected) => self.connection = None,
        }
        event
    }

    fn stop(&mut self) {
        self.stopped = true;
        self.connection = None;
    }

    fn reconnectable(&self) -> bool {
        true
    }

    fn take_reconnected(&mut self) -> bool {
        self.poll_reconnect();
        let reconnected = self.reconnected && self.connection.is_some();
        self.reconnected = false;
        reconnected
    }

    fn disconnect(&mut self) {
        self.connection = None;
    }

    fn connected(&self) -> bool {
        self.connection.is_some()
    }
}

fn read_remote_agent(
    mut stream: TcpStream,
    slot: usize,
    limit: usize,
    sender: &mpsc::Sender<ReadEvent>,
) {
    loop {
        let raw = match read_frame(&mut stream, limit.saturating_add(1024)) {
            Ok(Some(raw)) => raw,
            Ok(None) => {
                let _ = sender.send(ReadEvent::Eof);
                return;
            }
            Err(RunnerError::Limit(_)) => {
                let _ = sender.send(ReadEvent::Oversize(u32::MAX));
                return;
            }
            Err(error) => {
                let _ = sender.send(ReadEvent::Io(error.to_string()));
                return;
            }
        };
        match parse_logical_frame(&raw, slot, limit) {
            Ok(payload) => {
                if sender.send(ReadEvent::Frame(payload)).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = sender.send(ReadEvent::Io(error));
                return;
            }
        }
    }
}

fn parse_logical_frame(raw: &[u8], slot: usize, limit: usize) -> Result<Vec<u8>, String> {
    let value = decode_canonical(raw)?;
    let map = as_map(&value)?;
    require_keys(map, &["schema_id", "kind", "slot", "payload"])?;
    if get_text(map, "schema_id")? != SESSION_SCHEMA_ID
        || get_text(map, "kind")? != "logical"
        || get_u64(map, "slot")? != u64::try_from(slot).unwrap()
    {
        return Err("session logical routing".into());
    }
    let Value::Bytes(payload) = get(map, "payload")? else {
        return Err("session logical payload".into());
    };
    if payload.len() > limit {
        return Err("session logical payload limit".into());
    }
    decode_canonical(payload)?;
    Ok(payload.clone())
}

fn logical_value(slot: usize, payload: Vec<u8>) -> Value {
    Value::Map(vec![
        ("schema_id".into(), Value::Text(SESSION_SCHEMA_ID.into())),
        ("kind".into(), Value::Text("logical".into())),
        ("slot".into(), Value::U64(u64::try_from(slot).unwrap())),
        ("payload".into(), Value::Bytes(payload)),
    ])
}

fn status_value(status: &str, detail: &str) -> Value {
    Value::Map(vec![
        ("schema_id".into(), Value::Text(SESSION_SCHEMA_ID.into())),
        ("kind".into(), Value::Text("status".into())),
        ("status".into(), Value::Text(status.into())),
        ("detail".into(), Value::Text(detail.into())),
    ])
}

fn event_value(game_id: &str, event: &[u8]) -> Value {
    Value::Map(vec![
        ("schema_id".into(), Value::Text(SESSION_SCHEMA_ID.into())),
        ("kind".into(), Value::Text("event".into())),
        ("game_id".into(), Value::Text(game_id.into())),
        ("payload".into(), Value::Bytes(event.to_vec())),
    ])
}

fn write_session_frame(stream: &mut TcpStream, value: &Value) -> Result<(), RunnerError> {
    write_frame(stream, &value.encode())
}

fn spectator_writer(mut stream: TcpStream) -> Result<SpectatorSender, RunnerError> {
    stream.set_nodelay(true)?;
    let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(SPECTATOR_QUEUE_DEPTH);
    let state = Arc::new(SpectatorQueueState {
        queued_bytes: AtomicUsize::new(0),
        peak_bytes: AtomicUsize::new(0),
        drain_lock: Mutex::new(()),
        changed: Condvar::new(),
    });
    let writer_state = Arc::clone(&state);
    thread::spawn(move || {
        while let Ok(frame) = receiver.recv() {
            let size = frame.len();
            let failed = write_frame(&mut stream, &frame).is_err();
            writer_state.queued_bytes.fetch_sub(size, Ordering::AcqRel);
            writer_state.changed.notify_all();
            if failed {
                writer_state.queued_bytes.store(0, Ordering::Release);
                writer_state.changed.notify_all();
                break;
            }
        }
        let _ = stream.shutdown(Shutdown::Both);
    });
    Ok(SpectatorSender { sender, state })
}

fn wait_for_spectator_drain(
    spectators: &Arc<Mutex<Vec<SpectatorSender>>>,
    timeout: Duration,
) -> usize {
    let deadline = Instant::now() + timeout;
    let Ok(hub) = spectators.lock() else {
        return 0;
    };
    let peak = hub
        .iter()
        .map(|spectator| spectator.state.peak_bytes.load(Ordering::Acquire))
        .max()
        .unwrap_or(0);
    for spectator in hub.iter() {
        let now = Instant::now();
        if now >= deadline || !spectator.wait_until_drained(deadline - now) {
            break;
        }
    }
    peak
}

fn broadcast_event(spectators: &Arc<Mutex<Vec<SpectatorSender>>>, game_id: &str, event: &[u8]) {
    broadcast(spectators, event_value(game_id, event).encode());
}

fn broadcast_status(spectators: &Arc<Mutex<Vec<SpectatorSender>>>, status: &str, detail: &str) {
    broadcast(spectators, status_value(status, detail).encode());
}

fn broadcast(spectators: &Arc<Mutex<Vec<SpectatorSender>>>, frame: Vec<u8>) {
    if let Ok(mut hub) = spectators.lock() {
        hub.retain(|sender| sender.try_send(frame.clone()));
    }
}

#[must_use]
pub fn authentication_value(
    role: &str,
    token: &str,
    slot: Option<usize>,
    agent_id: Option<&str>,
    artifact_sha256: Option<[u8; 32]>,
) -> Value {
    let mut entries = vec![
        ("schema_id".into(), Value::Text(SESSION_SCHEMA_ID.into())),
        ("kind".into(), Value::Text("authenticate".into())),
        ("role".into(), Value::Text(role.into())),
        ("token".into(), Value::Text(token.into())),
    ];
    if let Some(slot) = slot {
        entries.push(("slot".into(), Value::U64(u64::try_from(slot).unwrap())));
    }
    if let Some(agent_id) = agent_id {
        entries.push(("agent_id".into(), Value::Text(agent_id.into())));
    }
    if let Some(digest) = artifact_sha256 {
        entries.push(("artifact_sha256".into(), Value::Bytes(digest.to_vec())));
    }
    Value::Map(entries)
}

#[must_use]
pub fn session_identity_document() -> Value {
    Value::Map(vec![
        ("schema_id".into(), Value::Text(SESSION_SCHEMA_ID.into())),
        (
            "logical_message_schema".into(),
            Value::Text(super::MESSAGE_SCHEMA_ID.into()),
        ),
        (
            "frame_prefix".into(),
            Value::Text("u32-big-endian-byte-length".into()),
        ),
        (
            "max_logical_frame_bytes".into(),
            Value::U64(u64::try_from(super::MAX_FRAME_BYTES).unwrap()),
        ),
        ("event_retention_default".into(), Value::Bool(false)),
        (
            "max_spectators".into(),
            Value::U64(u64::try_from(MAX_SPECTATORS).unwrap()),
        ),
        (
            "spectator_queue_bytes".into(),
            Value::U64(u64::try_from(SPECTATOR_QUEUE_BYTES).unwrap()),
        ),
        ("non_loopback_tls_required".into(), Value::Bool(true)),
        (
            "credential_storage".into(),
            Value::Text("operator-secret".into()),
        ),
        (
            "identity_sha256".into(),
            Value::Bytes(sha256(SESSION_SCHEMA_ID.as_bytes()).to_vec()),
        ),
        (
            "identity_text".into(),
            Value::Text(hex(&sha256(SESSION_SCHEMA_ID.as_bytes()))),
        ),
    ])
}

#[must_use]
pub const fn is_loopback_address(address: SocketAddr) -> bool {
    match address.ip() {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_loopback(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::net::{Shutdown, SocketAddr, TcpStream};
    use std::path::PathBuf;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use tac_scm_engine_core::cbor::Value;
    use tac_scm_engine_core::crypto::sha256;
    use tac_scm_engine_core::game::{ActionInput, GameActionSet, GameEngine, GamePlan};

    use super::{
        RemoteMatchReport, RemoteMatchServer, RemoteServerConfig, SESSION_SCHEMA_ID,
        SPECTATOR_QUEUE_BYTES, authentication_value, logical_value, parse_logical_frame,
        session_identity_document,
    };
    use crate::{
        MAX_FRAME_BYTES, OperationalPolicy, RunnerError, as_map, decode_canonical, envelope, get,
        get_text, get_u32, get_u64, read_frame, write_frame,
    };

    fn tokens() -> [String; 6] {
        std::array::from_fn(|slot| format!("agent-token-{slot}-0123456789abcdef"))
    }

    fn temporary_output(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "tac-scm-remote-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn config(output: PathBuf) -> RemoteServerConfig {
        let mut config = RemoteServerConfig::loopback(
            "127.0.0.1:0".parse().unwrap(),
            output,
            GamePlan::materialize([41_u8; 32]).unwrap(),
            tokens(),
            "spectator-token-0123456789abcdef".into(),
        );
        config.policy = OperationalPolicy {
            startup_grace: Duration::from_millis(1),
            action_deadline: Duration::from_millis(50),
            ..OperationalPolicy::default()
        };
        config.registration_timeout = Duration::from_secs(5);
        config.authentication_timeout = Duration::from_secs(1);
        config
    }

    fn authenticate_agent(address: SocketAddr, slot: usize) -> TcpStream {
        let mut stream = TcpStream::connect(address).unwrap();
        stream.set_nodelay(true).unwrap();
        let agent_id = format!("agent-{}", char::from(b'a' + u8::try_from(slot).unwrap()));
        let auth = authentication_value(
            "agent",
            &tokens()[slot],
            Some(slot),
            Some(&agent_id),
            Some([u8::try_from(slot + 1).unwrap(); 32]),
        );
        write_frame(&mut stream, &auth.encode()).unwrap();
        let response = read_frame(&mut stream, 4096).unwrap().unwrap();
        let response = decode_canonical(&response).unwrap();
        assert_eq!(
            get_text(as_map(&response).unwrap(), "status").unwrap(),
            "authenticated"
        );
        stream
    }

    fn run_empty_agent(address: SocketAddr, slot: usize, disconnect: bool) {
        run_empty_agent_with_open_days(address, slot, disconnect, 0..0);
    }

    /// Like `run_empty_agent`, but on `open_days` its action is not final, so
    /// those windows run to their cutoff.
    fn run_empty_agent_with_open_days(
        address: SocketAddr,
        slot: usize,
        disconnect: bool,
        open_days: std::ops::Range<u32>,
    ) {
        let mut stream = authenticate_agent(address, slot);
        if disconnect {
            stream.shutdown(Shutdown::Both).unwrap();
            return;
        }
        let agent_id = format!("agent-{}", char::from(b'a' + u8::try_from(slot).unwrap()));
        loop {
            let raw = match read_frame(&mut stream, MAX_FRAME_BYTES + 1024) {
                Ok(Some(raw)) => raw,
                Ok(None) | Err(_) => return,
            };
            let payload = parse_logical_frame(&raw, slot, MAX_FRAME_BYTES).unwrap();
            let message = decode_canonical(&payload).unwrap();
            let message = as_map(&message).unwrap();
            assert_eq!(get_text(message, "recipient").unwrap(), agent_id);
            let payload = as_map(get(message, "payload").unwrap()).unwrap();
            let message_type = get_text(payload, "type").unwrap();
            if message_type == "game-finished" {
                return;
            }
            if message_type != "day-observation" {
                continue;
            }
            let day = get_u32(payload, "day").unwrap();
            let observation_id = get_text(payload, "observation_id").unwrap();
            let action = GameActionSet::empty(&agent_id, observation_id, day, 1).value();
            let mut response = envelope(
                get_text(message, "game_id").unwrap(),
                &format!("remote/{agent_id}/{day}"),
                "manufacturer",
                &agent_id,
                "runner",
                Some(day),
                "action-window",
                u64::from(day),
                action,
            );
            let Value::Map(entries) = &mut response else {
                unreachable!()
            };
            if !open_days.contains(&day) {
                entries.push(("final_submission".into(), Value::Bool(true)));
            }
            let frame = logical_value(slot, response.encode()).encode();
            write_frame(&mut stream, &frame).unwrap();
        }
    }

    fn run_silent_agent(address: SocketAddr, slot: usize) {
        let mut stream = authenticate_agent(address, slot);
        loop {
            match read_frame(&mut stream, MAX_FRAME_BYTES + 1024) {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => return,
            }
        }
    }

    fn start_server(
        output: PathBuf,
    ) -> (
        SocketAddr,
        thread::JoinHandle<Result<RemoteMatchReport, RunnerError>>,
    ) {
        let server = RemoteMatchServer::bind(config(output)).unwrap();
        let address = server.local_addr();
        (address, thread::spawn(move || server.run()))
    }

    fn connect_spectator(address: SocketAddr) -> TcpStream {
        let mut stream = TcpStream::connect(address).unwrap();
        let auth = authentication_value(
            "spectator",
            "spectator-token-0123456789abcdef",
            None,
            None,
            None,
        );
        write_frame(&mut stream, &auth.encode()).unwrap();
        let response = read_frame(&mut stream, 4096).unwrap().unwrap();
        let response = decode_canonical(&response).unwrap();
        assert_eq!(
            get_text(as_map(&response).unwrap(), "status").unwrap(),
            "authenticated"
        );
        stream
    }

    #[test]
    fn remote_match_authenticates_slots_streams_read_only_and_matches_engine() {
        let output = temporary_output("equivalence");
        let (address, server) = start_server(output.clone());

        let mut rejected = TcpStream::connect(address).unwrap();
        let wrong_claim = authentication_value(
            "agent",
            &tokens()[0],
            Some(1),
            Some("agent-b"),
            Some([9_u8; 32]),
        );
        write_frame(&mut rejected, &wrong_claim.encode()).unwrap();
        let response = read_frame(&mut rejected, 4096).unwrap().unwrap();
        let response = decode_canonical(&response).unwrap();
        assert_eq!(
            get_text(as_map(&response).unwrap(), "status").unwrap(),
            "authentication-rejected"
        );

        let mut spectator = connect_spectator(address);
        let mut excess_spectator = TcpStream::connect(address).unwrap();
        let excess_auth = authentication_value(
            "spectator",
            "spectator-token-0123456789abcdef",
            None,
            None,
            None,
        );
        write_frame(&mut excess_spectator, &excess_auth.encode()).unwrap();
        let response = read_frame(&mut excess_spectator, 4096).unwrap().unwrap();
        let response = decode_canonical(&response).unwrap();
        assert_eq!(
            get_text(as_map(&response).unwrap(), "status").unwrap(),
            "authentication-rejected"
        );
        thread::sleep(Duration::from_millis(20));
        let spectator_reader = thread::spawn(move || {
            while let Some(raw) = read_frame(&mut spectator, MAX_FRAME_BYTES + 1024).unwrap() {
                let value = decode_canonical(&raw).unwrap();
                let map = as_map(&value).unwrap();
                if get_text(map, "kind").unwrap() == "event" {
                    let Value::Bytes(event) = get(map, "payload").unwrap() else {
                        panic!("event payload is not bytes")
                    };
                    assert!(!event.is_empty());
                    spectator.shutdown(Shutdown::Both).unwrap();
                    return;
                }
            }
            panic!("spectator received no event")
        });

        let clients: Vec<_> = (0..6)
            .map(|slot| thread::spawn(move || run_empty_agent(address, slot, false)))
            .collect();
        let report = server.join().unwrap().unwrap();
        for client in clients {
            client.join().unwrap();
        }
        spectator_reader.join().unwrap();
        assert_eq!(report.output.event_delivery, "stream");
        assert_eq!(report.output.default_counts, [0; 6]);

        let mut engine = GameEngine::new(report.plan.engine_plan.clone()).unwrap();
        let mut expected = None;
        for _ in 0..220 {
            let observations = engine.open_day().unwrap().to_vec();
            let inputs = observations
                .iter()
                .map(|observation| {
                    ActionInput::Accepted(GameActionSet::empty(
                        &observation.agent_id,
                        &observation.id,
                        observation.day,
                        1,
                    ))
                })
                .collect();
            expected = engine.commit_day(inputs).unwrap();
        }
        let expected = expected.unwrap();
        assert_eq!(
            report.output.result.transcript_bytes,
            expected.transcript_bytes
        );
        assert_eq!(
            report.output.result.daily_commitments,
            expected.daily_commitments
        );
        assert_eq!(
            report.output.result.final_state_sha256,
            expected.final_state_sha256
        );
        assert_eq!(report.output.result.scores, expected.scores);

        let files: BTreeSet<_> = fs::read_dir(&output)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(
            files,
            BTreeSet::from([
                "action-transcript.cbor".into(),
                "economic-summary.cbor".into(),
                "game-plan.cbor".into(),
                "game-result.cbor".into(),
                "identities.txt".into(),
                "process-summary.cbor".into(),
                "result-record.cbor".into(),
            ])
        );
        fs::remove_dir_all(output).unwrap();
    }

    fn read_logical_message(stream: &mut TcpStream, slot: usize) -> Option<Value> {
        let raw = match read_frame(stream, MAX_FRAME_BYTES + 1024) {
            Ok(Some(raw)) => raw,
            Ok(None) | Err(_) => return None,
        };
        let payload = parse_logical_frame(&raw, slot, MAX_FRAME_BYTES).unwrap();
        Some(decode_canonical(&payload).unwrap())
    }

    fn message_type(message: &Value) -> String {
        let payload = as_map(get(as_map(message).unwrap(), "payload").unwrap()).unwrap();
        get_text(payload, "type").unwrap().to_owned()
    }

    fn send_empty_action(
        stream: &mut TcpStream,
        slot: usize,
        observation: &Value,
        revision: u32,
        final_submission: bool,
    ) {
        let message = as_map(observation).unwrap();
        let payload = as_map(get(message, "payload").unwrap()).unwrap();
        let agent_id = format!("agent-{}", char::from(b'a' + u8::try_from(slot).unwrap()));
        let day = get_u32(payload, "day").unwrap();
        let observation_id = get_text(payload, "observation_id").unwrap();
        let action = GameActionSet::empty(&agent_id, observation_id, day, revision).value();
        let mut response = envelope(
            get_text(message, "game_id").unwrap(),
            &format!("remote/{agent_id}/{day}/{revision}"),
            "manufacturer",
            &agent_id,
            "runner",
            Some(day),
            "action-window",
            u64::from(day) * 4 + u64::from(revision),
            action,
        );
        let Value::Map(entries) = &mut response else {
            unreachable!()
        };
        if final_submission {
            entries.push(("final_submission".into(), Value::Bool(true)));
        }
        write_frame(stream, &logical_value(slot, response.encode()).encode()).unwrap();
    }

    fn reconnect_config(output: PathBuf) -> RemoteServerConfig {
        let mut config = config(output);
        // Every other slot submits a final action at once, so ordinary windows
        // close early; the long cutoff only bounds the recovery window.
        config.policy.action_deadline = Duration::from_secs(5);
        config
    }

    /// Plays empty final actions; on `drop_day` it drops its connection and
    /// authenticates again, returning the recovery frame types it received.
    fn run_reconnecting_agent(
        address: SocketAddr,
        slot: usize,
        drop_day: u32,
        within_window: bool,
    ) -> Vec<String> {
        let mut stream = authenticate_agent(address, slot);
        let mut bootstrap = None;
        let mut recovery = Vec::new();
        let mut dropped = false;
        while let Some(message) = read_logical_message(&mut stream, slot) {
            let kind = message_type(&message);
            if kind == "game-bootstrap" && bootstrap.is_none() {
                bootstrap = Some(message.encode());
            }
            if kind == "game-finished" {
                break;
            }
            if kind != "day-observation" {
                continue;
            }
            let payload = as_map(get(as_map(&message).unwrap(), "payload").unwrap()).unwrap();
            let day = get_u32(payload, "day").unwrap();
            if dropped || day != drop_day {
                send_empty_action(&mut stream, slot, &message, 1, true);
                continue;
            }
            dropped = true;
            if within_window {
                // Submit a replaceable candidate, see its receipt, then lose
                // the connection before cutoff.
                send_empty_action(&mut stream, slot, &message, 1, false);
                loop {
                    let receipt = read_logical_message(&mut stream, slot).unwrap();
                    if message_type(&receipt) == "action-receipt" {
                        break;
                    }
                }
            }
            stream.shutdown(Shutdown::Both).unwrap();
            if !within_window {
                // Stay away past this window's cutoff so the slot defaults.
                thread::sleep(Duration::from_millis(250));
            }
            // A different artifact cannot take over the slot.
            let mut impostor = TcpStream::connect(address).unwrap();
            let agent_id = format!("agent-{}", char::from(b'a' + u8::try_from(slot).unwrap()));
            let claim = authentication_value(
                "agent",
                &tokens()[slot],
                Some(slot),
                Some(&agent_id),
                Some([0xEE; 32]),
            );
            write_frame(&mut impostor, &claim.encode()).unwrap();
            let response =
                decode_canonical(&read_frame(&mut impostor, 4096).unwrap().unwrap()).unwrap();
            assert_eq!(
                get_text(as_map(&response).unwrap(), "status").unwrap(),
                "authentication-rejected"
            );
            stream = authenticate_agent(address, slot);
            let replayed = read_logical_message(&mut stream, slot).unwrap();
            assert_eq!(
                Some(replayed.encode()),
                bootstrap,
                "bootstrap is replayed byte for byte"
            );
            recovery.push(message_type(&replayed));
            if within_window {
                for _ in 0..3 {
                    let replayed = read_logical_message(&mut stream, slot).unwrap();
                    recovery.push(message_type(&replayed));
                    if message_type(&replayed) == "day-observation" {
                        assert_eq!(
                            replayed.encode(),
                            message.encode(),
                            "observation is replayed"
                        );
                    }
                    if message_type(&replayed) == "action-receipt" {
                        let receipt =
                            as_map(get(as_map(&replayed).unwrap(), "payload").unwrap()).unwrap();
                        assert_eq!(get_u64(receipt, "revision").unwrap(), 1);
                        send_empty_action(&mut stream, slot, &message, 2, true);
                    }
                }
            }
        }
        recovery
    }

    #[test]
    fn lost_connection_recovers_within_the_window_and_keeps_the_candidate() {
        let output = temporary_output("reconnect-window");
        let server = RemoteMatchServer::bind(reconnect_config(output.clone())).unwrap();
        let address = server.local_addr();
        let server = thread::spawn(move || server.run());
        let clients: Vec<_> = (0..6)
            .map(|slot| {
                thread::spawn(move || {
                    if slot == 5 {
                        run_reconnecting_agent(address, slot, 7, true)
                    } else {
                        run_empty_agent(address, slot, false);
                        Vec::new()
                    }
                })
            })
            .collect();
        let report = server.join().unwrap().unwrap();
        let recovery = clients
            .into_iter()
            .map(|client| client.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            recovery[5],
            [
                "game-bootstrap",
                "action-committed",
                "day-observation",
                "action-receipt"
            ]
        );
        assert_eq!(report.output.default_counts, [0; 6]);
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn disconnected_slot_defaults_until_it_reconnects_then_plays_on() {
        let output = temporary_output("reconnect-later");
        let mut config = config(output.clone());
        config.policy.action_deadline = Duration::from_millis(100);
        let server = RemoteMatchServer::bind(config).unwrap();
        let address = server.local_addr();
        let server = thread::spawn(move || server.run());
        let clients: Vec<_> = (0..6)
            .map(|slot| {
                thread::spawn(move || {
                    if slot == 5 {
                        run_reconnecting_agent(address, slot, 3, false)
                    } else {
                        // Days 3 to 9 run to their 100 ms cutoff, so the
                        // absent slot misses a few windows, not the match.
                        run_empty_agent_with_open_days(address, slot, false, 3..10);
                        Vec::new()
                    }
                })
            })
            .collect();
        let report = server.join().unwrap().unwrap();
        let recovery = clients
            .into_iter()
            .map(|client| client.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(recovery[5], ["game-bootstrap"]);
        assert_eq!(&report.output.default_counts[..5], &[0; 5]);
        assert!(report.output.default_counts[5] >= 1);
        assert!(
            report.output.default_counts[5] <= 7,
            "{:?}",
            report.output.default_counts
        );
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn disconnect_defaults_only_its_bound_slot_and_service_shuts_down() {
        let output = temporary_output("disconnect");
        let (address, server) = start_server(output.clone());
        let clients: Vec<_> = (0..6)
            .map(|slot| thread::spawn(move || run_empty_agent(address, slot, slot == 5)))
            .collect();
        let report = server.join().unwrap().unwrap();
        for client in clients {
            client.join().unwrap();
        }
        assert_eq!(report.output.event_delivery, "off");
        assert_eq!(&report.output.default_counts[..5], &[0; 5]);
        assert_eq!(report.output.default_counts[5], 220);
        assert!(TcpStream::connect(report.listen_address).is_err());
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn server_clock_timeout_defaults_only_the_silent_remote_slot() {
        let output = temporary_output("deadline");
        let mut config = config(output.clone());
        config.policy.action_deadline = Duration::from_millis(10);
        let server = RemoteMatchServer::bind(config).unwrap();
        let address = server.local_addr();
        let server = thread::spawn(move || server.run());
        let clients: Vec<_> = (0..6)
            .map(|slot| {
                thread::spawn(move || {
                    if slot == 5 {
                        run_silent_agent(address, slot);
                    } else {
                        run_empty_agent(address, slot, false);
                    }
                })
            })
            .collect();
        let report = server.join().unwrap().unwrap();
        for client in clients {
            client.join().unwrap();
        }
        assert_eq!(&report.output.default_counts[..5], &[0; 5]);
        assert_eq!(report.output.default_counts[5], 220);
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn non_loopback_bind_requires_declared_tls_termination() {
        let mut config = config(temporary_output("tls"));
        config.bind = "0.0.0.0:0".parse().unwrap();
        assert!(RemoteMatchServer::bind(config.clone()).is_err());
        config.tls_terminated = true;
        let server = RemoteMatchServer::bind(config).unwrap();
        assert_eq!(
            server.local_addr().ip(),
            "0.0.0.0".parse::<std::net::IpAddr>().unwrap()
        );
    }

    #[test]
    fn session_identity_is_versioned() {
        assert_eq!(SESSION_SCHEMA_ID, "tac-scm-session/1");
        assert_eq!(sha256(SESSION_SCHEMA_ID.as_bytes()).len(), 32);
        let identity = session_identity_document();
        let identity = as_map(&identity).unwrap();
        assert_eq!(get_u64(identity, "max_spectators").unwrap(), 1);
        assert_eq!(
            get_u64(identity, "spectator_queue_bytes").unwrap(),
            u64::try_from(SPECTATOR_QUEUE_BYTES).unwrap()
        );
    }
}
