//! Local six-process runner for the TAC SCM 2026 reference transport.
//!
//! The runner is deliberately outside the authoritative engine. Wall-clock
//! outcomes are reduced to accepted actions or declared defaults before the
//! engine sees them. Normal operation retains accepted actions plus anomalies;
//! raw transport evidence and canonical events require explicit opt-in.

pub mod remote;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use tac_scm_engine_core::cbor::Value;
use tac_scm_engine_core::crypto::{hex, sha256};
use tac_scm_engine_core::game::{
    ActionInput, COMPONENTS, CommandRejection, CustomerOffer, CustomerOrderStatus, DefaultCause,
    DeliveryLine, FACTORY_CYCLES, GameActionSet, GameEngine, GameError, GamePlan, GameResult,
    Observation, PRODUCTS, ProductionLine, SupplierOrderCommand, SupplierRfqCommand,
};
use tac_scm_engine_core::historical::HistoricalProfile;
use tac_scm_engine_core::{
    DRAFT3_ENGINE_SEMANTIC_ID, DRAFT3_PROFILE_ID, ENGINE_PACKAGE_VERSION, ENGINE_SEMANTIC_ID,
    EVENT_SCHEMA_ID, PROFILE_ID,
};

pub const TRANSPORT_ID: &str = "tac-scm-framed-cbor-stdio/1";
pub const MESSAGE_SCHEMA_ID: &str = "tac-scm-message/2";
pub const PLAN_SCHEMA_ID: &str = "tac-scm-runner-plan/1";
pub const AUDIT_SCHEMA_ID: &str = "tac-scm-protocol-audit/1";
pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;
const RESULT_RECORD_FIELDS: [&str; 26] = [
    "campaign_id",
    "campaign_manifest_sha256",
    "schedule_ordinal",
    "block_index",
    "context_id",
    "seat_rotation",
    "game_definition_id",
    "game_result_id",
    "profile_id",
    "engine_artifact_sha256",
    "runner_artifact_sha256",
    "ordered_agent_artifact_sha256",
    "root_seed_commitment",
    "termination",
    "scores_microcredits",
    "default_counts",
    "action_transcript_sha256",
    "economic_summary_sha256",
    "event_count",
    "event_delivery",
    "event_log_sha256",
    "final_state_sha256",
    "replay_verified",
    "protocol_audit_sha256",
    "process_summary_sha256",
    "infrastructure_attempts",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventEvidence {
    Off,
    Capture,
}

impl EventEvidence {
    const fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Capture => "capture",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditEvidence {
    Anomalies,
    Full,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EvidencePolicy {
    pub events: EventEvidence,
    pub audit: AuditEvidence,
}

impl Default for EvidencePolicy {
    fn default() -> Self {
        Self {
            events: EventEvidence::Off,
            audit: AuditEvidence::Anomalies,
        }
    }
}

impl EvidencePolicy {
    pub const FULL: Self = Self {
        events: EventEvidence::Capture,
        audit: AuditEvidence::Full,
    };
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentArtifact {
    pub agent_id: String,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub sha256: [u8; 32],
    pub byte_length: u64,
}

impl AgentArtifact {
    pub fn inspect(
        agent_id: impl Into<String>,
        executable: impl Into<PathBuf>,
        arguments: Vec<String>,
    ) -> Result<Self, RunnerError> {
        let executable = executable.into();
        let bytes = fs::read(&executable)?;
        Ok(Self {
            agent_id: agent_id.into(),
            executable,
            arguments,
            sha256: sha256(&bytes),
            byte_length: u64::try_from(bytes.len()).map_err(|_| RunnerError::Limit("artifact"))?,
        })
    }

    #[must_use]
    pub fn remote(agent_id: impl Into<String>, artifact_sha256: [u8; 32]) -> Self {
        Self {
            agent_id: agent_id.into(),
            executable: PathBuf::new(),
            arguments: vec!["tac-scm-session/1".into()],
            sha256: artifact_sha256,
            byte_length: 0,
        }
    }

    fn value(&self) -> Value {
        let remote = self.executable.as_os_str().is_empty();
        Value::Map(vec![
            ("name".into(), Value::Text(self.agent_id.clone())),
            ("version".into(), Value::Text("frozen-executable".into())),
            ("sha256".into(), Value::Bytes(self.sha256.to_vec())),
            (
                "media_type".into(),
                Value::Text(
                    if remote {
                        "application/vnd.tac-scm.remote-agent-identity"
                    } else {
                        "application/vnd.tac-scm.agent-executable"
                    }
                    .into(),
                ),
            ),
            ("byte_length".into(), Value::U64(self.byte_length)),
            (
                "source_revision".into(),
                Value::Text("external-artifact".into()),
            ),
            (
                "build_recipe_sha256".into(),
                Value::Bytes([0_u8; 32].to_vec()),
            ),
            (
                "dependency_lock_sha256".into(),
                Value::Bytes([0_u8; 32].to_vec()),
            ),
            ("license".into(), Value::Text("manifest-required".into())),
            (
                "platform".into(),
                Value::Text(
                    if remote {
                        "remote"
                    } else {
                        std::env::consts::OS
                    }
                    .into(),
                ),
            ),
            (
                "arguments".into(),
                Value::Array(self.arguments.iter().cloned().map(Value::Text).collect()),
            ),
        ])
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationalPolicy {
    pub startup_grace: Duration,
    pub action_deadline: Duration,
    pub shutdown_grace: Duration,
    pub max_frame_bytes: usize,
    pub max_frames_per_window: u32,
    pub stderr_limit_bytes: usize,
}

impl Default for OperationalPolicy {
    fn default() -> Self {
        Self {
            startup_grace: Duration::from_millis(250),
            action_deadline: Duration::from_secs(15),
            shutdown_grace: Duration::from_millis(100),
            max_frame_bytes: MAX_FRAME_BYTES,
            max_frames_per_window: 16,
            stderr_limit_bytes: 64 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrozenGamePlan {
    pub engine_plan: GamePlan,
    pub agents: [AgentArtifact; 6],
    pub policy: OperationalPolicy,
    pub campaign_id: String,
    pub campaign_manifest_sha256: [u8; 32],
    pub schedule_ordinal: u64,
    pub block_index: u64,
    pub context_id: String,
    pub seat_rotation: u32,
    pub canonical_bytes: Vec<u8>,
    pub game_definition_id: String,
}

impl FrozenGamePlan {
    pub fn freeze(
        engine_plan: GamePlan,
        agents: [AgentArtifact; 6],
        policy: OperationalPolicy,
        campaign_id: impl Into<String>,
        schedule_ordinal: u64,
    ) -> Result<Self, RunnerError> {
        let campaign_id = campaign_id.into();
        let campaign_manifest_sha256 = sha256(campaign_id.as_bytes());
        Self::freeze_scheduled(
            engine_plan,
            agents,
            policy,
            campaign_id,
            campaign_manifest_sha256,
            schedule_ordinal,
            schedule_ordinal,
            "manual",
            0,
        )
    }

    /// Freezes all pre-published campaign coordinates into the authoritative
    /// plan before any process is launched.
    #[allow(clippy::too_many_arguments)]
    pub fn freeze_scheduled(
        mut engine_plan: GamePlan,
        agents: [AgentArtifact; 6],
        policy: OperationalPolicy,
        campaign_id: impl Into<String>,
        campaign_manifest_sha256: [u8; 32],
        schedule_ordinal: u64,
        block_index: u64,
        context_id: impl Into<String>,
        seat_rotation: u32,
    ) -> Result<Self, RunnerError> {
        for (slot, artifact) in agents.iter().enumerate() {
            if artifact.agent_id != engine_plan.participants[slot] {
                return Err(RunnerError::Plan("agent slot identity mismatch"));
            }
        }
        let campaign_id = campaign_id.into();
        let context_id = context_id.into();
        let value = plan_value(
            &engine_plan,
            &agents,
            &policy,
            &campaign_id,
            campaign_manifest_sha256,
            schedule_ordinal,
            block_index,
            &context_id,
            seat_rotation,
        );
        let canonical_bytes = value.encode();
        let plan_digest = sha256(&canonical_bytes);
        let game_definition_id = format!("plan/{}", hex(&plan_digest));
        engine_plan = engine_plan.bind_external_plan(plan_digest);
        Ok(Self {
            engine_plan,
            agents,
            policy,
            campaign_id,
            campaign_manifest_sha256,
            schedule_ordinal,
            block_index,
            context_id,
            seat_rotation,
            canonical_bytes,
            game_definition_id,
        })
    }

    #[must_use]
    pub fn sha256(&self) -> [u8; 32] {
        sha256(&self.canonical_bytes)
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_value(
    engine: &GamePlan,
    agents: &[AgentArtifact; 6],
    policy: &OperationalPolicy,
    campaign_id: &str,
    campaign_manifest_sha256: [u8; 32],
    schedule_ordinal: u64,
    block_index: u64,
    context_id: &str,
    seat_rotation: u32,
) -> Value {
    Value::Map(vec![
        ("schema_id".into(), Value::Text(PLAN_SCHEMA_ID.into())),
        ("profile_id".into(), Value::Text(engine.profile_id.clone())),
        (
            "profile_revision".into(),
            Value::U64(u64::from(engine.profile_revision)),
        ),
        (
            "configuration_sha256".into(),
            Value::Bytes(engine.configuration_identity.to_vec()),
        ),
        (
            "root_seed_commitment".into(),
            Value::Bytes(sha256(&engine.root_seed).to_vec()),
        ),
        ("seed_reference".into(), Value::Text("runner-secret".into())),
        ("transport_id".into(), Value::Text(TRANSPORT_ID.into())),
        ("campaign_id".into(), Value::Text(campaign_id.into())),
        (
            "campaign_manifest_sha256".into(),
            Value::Bytes(campaign_manifest_sha256.to_vec()),
        ),
        ("schedule_ordinal".into(), Value::U64(schedule_ordinal)),
        ("block_index".into(), Value::U64(block_index)),
        ("context_id".into(), Value::Text(context_id.into())),
        ("seat_rotation".into(), Value::U64(u64::from(seat_rotation))),
        (
            "participants".into(),
            Value::Array(
                engine
                    .participants
                    .iter()
                    .cloned()
                    .map(Value::Text)
                    .collect(),
            ),
        ),
        (
            "agents".into(),
            Value::Array(agents.iter().map(AgentArtifact::value).collect()),
        ),
        (
            "operational".into(),
            Value::Map(vec![
                (
                    "startup_grace_ms".into(),
                    Value::U64(u64::try_from(policy.startup_grace.as_millis()).unwrap()),
                ),
                (
                    "action_deadline_ms".into(),
                    Value::U64(u64::try_from(policy.action_deadline.as_millis()).unwrap()),
                ),
                (
                    "shutdown_grace_ms".into(),
                    Value::U64(u64::try_from(policy.shutdown_grace.as_millis()).unwrap()),
                ),
                (
                    "max_frame_bytes".into(),
                    Value::U64(u64::try_from(policy.max_frame_bytes).unwrap()),
                ),
                (
                    "max_frames_per_window".into(),
                    Value::U64(u64::from(policy.max_frames_per_window)),
                ),
                (
                    "stderr_limit_bytes".into(),
                    Value::U64(u64::try_from(policy.stderr_limit_bytes).unwrap()),
                ),
                (
                    "receipt_policy".into(),
                    Value::Text("day-slot-local-sequence-final-cutoff/v2".into()),
                ),
                (
                    "shutdown_policy".into(),
                    Value::Text("close-stdin-bounded-grace-then-kill/v2".into()),
                ),
            ]),
        ),
        ("run_nonce".into(), Value::Bytes([0_u8; 16].to_vec())),
    ])
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditRecord {
    pub day: u32,
    pub slot: u8,
    pub receipt_ordinal: u64,
    pub raw_bytes: Vec<u8>,
    pub disposition: &'static str,
    pub detail: String,
}

impl AuditRecord {
    fn value(&self) -> Value {
        Value::Map(vec![
            ("day".into(), Value::U64(u64::from(self.day))),
            ("slot".into(), Value::U64(u64::from(self.slot))),
            ("receipt_ordinal".into(), Value::U64(self.receipt_ordinal)),
            ("raw_bytes".into(), Value::Bytes(self.raw_bytes.clone())),
            ("disposition".into(), Value::Text(self.disposition.into())),
            ("detail".into(), Value::Text(self.detail.clone())),
        ])
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunnerOutput {
    pub result: GameResult,
    pub plan_sha256: [u8; 32],
    pub audit_bytes: Vec<u8>,
    pub audit_sha256: [u8; 32],
    pub game_result_manifest_bytes: Vec<u8>,
    pub result_record_bytes: Vec<u8>,
    pub game_result_id: String,
    pub default_counts: [u64; 6],
    pub event_delivery: &'static str,
    pub full_audit: bool,
    pub audit_record_count: usize,
    pub process_summary_bytes: Vec<u8>,
    pub process_summary_sha256: [u8; 32],
    pub runtime_summary_bytes: Vec<u8>,
}

impl RunnerOutput {
    pub fn write_to(&self, plan: &FrozenGamePlan, output: &Path) -> Result<(), RunnerError> {
        if !self.result.events_captured || !self.full_audit {
            return Err(RunnerError::Plan("full evidence was not captured"));
        }
        fs::create_dir_all(output)?;
        write_new(output.join("game-plan.cbor"), &plan.canonical_bytes)?;
        write_new(output.join("protocol-audit.cbor"), &self.audit_bytes)?;
        write_new(
            output.join("action-transcript.cbor"),
            &self.result.transcript_bytes,
        )?;
        write_new(
            output.join("event-log.cborseq"),
            &self.result.event_log_bytes,
        )?;
        write_new(
            output.join("economic-summary.cbor"),
            &self.result.economic_summary_bytes,
        )?;
        write_new(
            output.join("process-summary.cbor"),
            &self.process_summary_bytes,
        )?;
        if std::env::var_os("TAC_SCM_RUNTIME_SUMMARY").is_some_and(|value| value == "1") {
            write_new(
                output.join("runtime-summary.cbor"),
                &self.runtime_summary_bytes,
            )?;
        }
        write_new(
            output.join("game-result.cbor"),
            &self.game_result_manifest_bytes,
        )?;
        write_new(output.join("result-record.cbor"), &self.result_record_bytes)?;
        self.write_identities(plan, output)
    }

    /// Writes only replay-verified identities and closed result metadata. The
    /// canonical transcript, event log, and protocol audit have already been
    /// verified in memory and remain bound by their hashes in the result.
    pub fn write_summary_to(
        &self,
        plan: &FrozenGamePlan,
        output: &Path,
    ) -> Result<(), RunnerError> {
        fs::create_dir_all(output)?;
        write_new(output.join("game-plan.cbor"), &plan.canonical_bytes)?;
        write_new(
            output.join("action-transcript.cbor"),
            &self.result.transcript_bytes,
        )?;
        write_new(
            output.join("economic-summary.cbor"),
            &self.result.economic_summary_bytes,
        )?;
        write_new(
            output.join("process-summary.cbor"),
            &self.process_summary_bytes,
        )?;
        if std::env::var_os("TAC_SCM_RUNTIME_SUMMARY").is_some_and(|value| value == "1") {
            write_new(
                output.join("runtime-summary.cbor"),
                &self.runtime_summary_bytes,
            )?;
        }
        if self.audit_record_count > 0 {
            write_new(output.join("protocol-anomalies.cbor"), &self.audit_bytes)?;
        }
        write_new(
            output.join("game-result.cbor"),
            &self.game_result_manifest_bytes,
        )?;
        write_new(output.join("result-record.cbor"), &self.result_record_bytes)?;
        self.write_identities(plan, output)
    }

    fn write_identities(&self, plan: &FrozenGamePlan, output: &Path) -> Result<(), RunnerError> {
        let summary = format!(
            "game_definition_id={}\ngame_result_id={}\nplan_sha256={}\naudit_sha256={}\ntranscript_sha256={}\neconomic_summary_sha256={}\nprocess_summary_sha256={}\nevent_delivery={}\nevent_count={}\nevent_log_sha256={}\nfinal_state_sha256={}\n",
            plan.game_definition_id,
            self.game_result_id,
            hex(&self.plan_sha256),
            hex(&self.audit_sha256),
            hex(&self.result.transcript_sha256),
            hex(&self.result.economic_summary_sha256),
            hex(&self.process_summary_sha256),
            self.event_delivery,
            self.result.event_count,
            hex(&self.result.event_log_sha256),
            hex(&self.result.final_state_sha256),
        );
        write_new(output.join("identities.txt"), summary.as_bytes())
    }
}

fn write_new(path: PathBuf, bytes: &[u8]) -> Result<(), RunnerError> {
    let mut file = File::options().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[derive(Debug)]
pub enum RunnerError {
    Io(io::Error),
    Plan(&'static str),
    Protocol(String),
    Limit(&'static str),
    Engine(GameError),
}

impl fmt::Display for RunnerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for RunnerError {}
impl From<io::Error> for RunnerError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<GameError> for RunnerError {
    fn from(value: GameError) -> Self {
        Self::Engine(value)
    }
}

pub(crate) enum ReadEvent {
    Frame(Vec<u8>),
    Oversize(u32),
    Eof,
    Io(String),
}

struct AgentProcess {
    child: Child,
    input: Option<SyncSender<Vec<u8>>>,
    output: Receiver<ReadEvent>,
    shutdown_grace: Duration,
}

pub(crate) trait AgentSession: Send {
    fn send_value(&mut self, value: &Value) -> Result<(), RunnerError>;
    fn try_recv(&mut self) -> Result<ReadEvent, mpsc::TryRecvError>;
    fn stop(&mut self);
    /// True when a lost connection can be replaced within the match (remote
    /// sessions). Such a slot is never permanently quarantined: it defaults
    /// while disconnected and recovers at the next poll after reconnecting.
    fn reconnectable(&self) -> bool {
        false
    }
    /// True once after a replacement connection was attached and the
    /// recovery sequence was replayed on it.
    fn take_reconnected(&mut self) -> bool {
        false
    }
    /// Close only the current connection; the slot stays open for recovery.
    fn disconnect(&mut self) {}
    /// False while a reconnectable session has no live connection.
    fn connected(&self) -> bool {
        true
    }
}

impl AgentProcess {
    fn spawn(
        artifact: &AgentArtifact,
        limit: usize,
        shutdown_grace: Duration,
    ) -> Result<Self, RunnerError> {
        let stderr = if std::env::var_os("TAC_SCM_RUNNER_INHERIT_STDERR").is_some() {
            Stdio::inherit()
        } else {
            Stdio::null()
        };
        let mut child = Command::new(&artifact.executable)
            .args(&artifact.arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()?;
        let mut input = child.stdin.take().ok_or(RunnerError::Plan("agent stdin"))?;
        let mut output = child
            .stdout
            .take()
            .ok_or(RunnerError::Plan("agent stdout"))?;
        // One bounded game-worth of frames keeps an agent that stops reading
        // from blocking the authoritative runner thread. The queue is bounded
        // and the child is still killed during final shutdown.
        let (input_sender, input_receiver) = mpsc::sync_channel::<Vec<u8>>(512);
        thread::spawn(move || {
            while let Ok(frame) = input_receiver.recv() {
                if input.write_all(&frame).is_err() || input.flush().is_err() {
                    break;
                }
            }
        });
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            loop {
                let mut prefix = [0_u8; 4];
                match output.read_exact(&mut prefix) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                        let _ = sender.send(ReadEvent::Eof);
                        break;
                    }
                    Err(error) => {
                        let _ = sender.send(ReadEvent::Io(error.to_string()));
                        break;
                    }
                }
                let length = u32::from_be_bytes(prefix);
                if usize::try_from(length).unwrap_or(usize::MAX) > limit {
                    let _ = sender.send(ReadEvent::Oversize(length));
                    break;
                }
                let mut bytes = vec![0_u8; usize::try_from(length).unwrap()];
                if let Err(error) = output.read_exact(&mut bytes) {
                    let _ = sender.send(ReadEvent::Io(error.to_string()));
                    break;
                }
                if sender.send(ReadEvent::Frame(bytes)).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            input: Some(input_sender),
            output: receiver,
            shutdown_grace,
        })
    }

    fn send(&mut self, value: &Value) -> Result<(), RunnerError> {
        let bytes = value.encode();
        let length = u32::try_from(bytes.len()).map_err(|_| RunnerError::Limit("frame"))?;
        let mut frame = Vec::with_capacity(4 + bytes.len());
        frame.extend_from_slice(&length.to_be_bytes());
        frame.extend_from_slice(&bytes);
        self.input
            .as_ref()
            .ok_or(RunnerError::Plan("agent input closed"))?
            .try_send(frame)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => RunnerError::Limit("agent input backlog"),
                mpsc::TrySendError::Disconnected(_) => RunnerError::Plan("agent input closed"),
            })
    }

    fn stop(&mut self) {
        self.input.take();
        let deadline = Instant::now() + self.shutdown_grace;
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl AgentSession for AgentProcess {
    fn send_value(&mut self, value: &Value) -> Result<(), RunnerError> {
        self.send(value)
    }

    fn try_recv(&mut self) -> Result<ReadEvent, mpsc::TryRecvError> {
        self.output.try_recv()
    }

    fn stop(&mut self) {
        Self::stop(self);
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone, Debug)]
struct Candidate {
    action: GameActionSet,
    revision: u32,
    final_submission: bool,
    message_id: String,
    raw: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ThinAgentSummary {
    minimum_cash_microcredits: i64,
    overdue_unit_days: u64,
    production_starvation_days: u64,
    total_inventory_unit_days: u64,
    terminal_exposure_units: u64,
}

impl Default for ThinAgentSummary {
    fn default() -> Self {
        Self {
            minimum_cash_microcredits: i64::MAX,
            overdue_unit_days: 0,
            production_starvation_days: 0,
            total_inventory_unit_days: 0,
            terminal_exposure_units: 0,
        }
    }
}

impl ThinAgentSummary {
    fn observe(&mut self, observation: &Observation) {
        self.minimum_cash_microcredits = self.minimum_cash_microcredits.min(observation.cash);
        let inventory = observation
            .component_inventory
            .values()
            .chain(observation.pc_inventory.values())
            .chain(observation.pending_components.values())
            .chain(observation.pending_pcs.values())
            .map(|quantity| u64::from(*quantity))
            .sum::<u64>();
        let open_supplier_units = observation
            .supplier_orders
            .iter()
            .map(|order| u64::from(order.quantity.saturating_sub(order.delivered)))
            .sum::<u64>();
        self.total_inventory_unit_days += inventory;
        self.terminal_exposure_units = inventory + open_supplier_units;
        self.overdue_unit_days += observation
            .customer_orders
            .iter()
            .filter(|order| {
                matches!(order.status, CustomerOrderStatus::Open) && order.due_day < observation.day
            })
            .map(|order| u64::from(order.quantity))
            .sum::<u64>();
    }

    fn observe_action(&mut self, observation: &Observation, input: &ActionInput) {
        let urgent = observation.customer_orders.iter().any(|order| {
            matches!(order.status, CustomerOrderStatus::Open)
                && order.due_day <= observation.day.saturating_add(3)
        });
        let scheduled = matches!(
            input,
            ActionInput::Accepted(action)
                if !action.production_schedule.is_empty() || !action.delivery_schedule.is_empty()
        );
        self.production_starvation_days += u64::from(urgent && !scheduled);
    }

    fn value(self) -> Value {
        Value::Map(vec![
            (
                "minimum_cash_microcredits".into(),
                Value::I64(self.minimum_cash_microcredits),
            ),
            (
                "overdue_unit_days".into(),
                Value::U64(self.overdue_unit_days),
            ),
            (
                "production_starvation_days".into(),
                Value::U64(self.production_starvation_days),
            ),
            (
                "total_inventory_unit_days".into(),
                Value::U64(self.total_inventory_unit_days),
            ),
            (
                "terminal_exposure_units".into(),
                Value::U64(self.terminal_exposure_units),
            ),
        ])
    }
}

pub fn run_game(plan: &FrozenGamePlan) -> Result<RunnerOutput, RunnerError> {
    run_game_with_evidence(plan, EvidencePolicy::default())
}

pub fn run_game_with_evidence(
    plan: &FrozenGamePlan,
    evidence: EvidencePolicy,
) -> Result<RunnerOutput, RunnerError> {
    let mut engine = match evidence.events {
        EventEvidence::Off => GameEngine::new(plan.engine_plan.clone())?,
        EventEvidence::Capture => GameEngine::new_with_event_capture(plan.engine_plan.clone())?,
    };
    if training_without_commitments() {
        engine = engine.without_state_commitments();
    }
    run_game_with_engine(plan, engine, evidence.audit, evidence.events.label())
}

pub fn run_game_with_event_stream<F>(
    plan: &FrozenGamePlan,
    sink: F,
) -> Result<RunnerOutput, RunnerError>
where
    F: FnMut(&[u8]) + Send + 'static,
{
    let engine = GameEngine::new_with_event_stream(plan.engine_plan.clone(), sink)?;
    run_game_with_engine(plan, engine, AuditEvidence::Anomalies, "stream")
}

fn run_game_with_engine(
    plan: &FrozenGamePlan,
    engine: GameEngine,
    audit_evidence: AuditEvidence,
    event_delivery: &'static str,
) -> Result<RunnerOutput, RunnerError> {
    let processes: Vec<Box<dyn AgentSession>> = plan
        .agents
        .iter()
        .map(|artifact| {
            AgentProcess::spawn(
                artifact,
                plan.policy.max_frame_bytes,
                plan.policy.shutdown_grace,
            )
            .map(|process| Box::new(process) as Box<dyn AgentSession>)
        })
        .collect::<Result<_, _>>()?;
    run_game_with_sessions(plan, engine, audit_evidence, event_delivery, processes)
}

pub(crate) fn run_game_with_sessions(
    plan: &FrozenGamePlan,
    mut engine: GameEngine,
    audit_evidence: AuditEvidence,
    event_delivery: &'static str,
    mut sessions: Vec<Box<dyn AgentSession>>,
) -> Result<RunnerOutput, RunnerError> {
    if sessions.len() != 6 {
        return Err(RunnerError::Plan("exactly six agent sessions required"));
    }
    for (slot, session) in sessions.iter_mut().enumerate() {
        session.send_value(&bootstrap_message(plan, slot))?;
    }
    // Process launch latency is not part of an action window. This declared,
    // frozen grace lets all six artifacts reach their read loop before day 0.
    thread::sleep(plan.policy.startup_grace);
    let mut audit = Vec::new();
    let mut defaults = [0_u64; 6];
    let mut received_frames = [0_u64; 6];
    let mut thin_summaries = [ThinAgentSummary::default(); 6];
    let mut decision_durations_ns: [Vec<u64>; 6] = std::array::from_fn(|_| Vec::new());
    let mut replay_inputs = Vec::with_capacity(220);
    let mut quarantined = [false; 6];
    let mut seen: Vec<BTreeMap<String, Vec<u8>>> = vec![BTreeMap::new(); 6];
    let mut engine_sender_sequence = 0_u64;
    let mut runner_sender_sequence = 6_u64;
    let mut recorder = TrainingRecorder::from_environment(&plan.game_definition_id)?;
    loop {
        let observations = engine.open_day()?.to_vec();
        for (summary, observation) in thin_summaries.iter_mut().zip(&observations) {
            summary.observe(observation);
        }
        let mut send_failed = [false; 6];
        for (slot, session) in sessions.iter_mut().enumerate() {
            if quarantined[slot] {
                send_failed[slot] = true;
                continue;
            }
            let message = observation_message(
                &plan.game_definition_id,
                &observations[slot],
                engine_sender_sequence,
            );
            engine_sender_sequence += 1;
            if session.send_value(&message).is_err() {
                send_failed[slot] = true;
            }
        }
        let window_started = Instant::now();
        let deadline = window_started + plan.policy.action_deadline;
        let mut candidates: Vec<Option<Candidate>> = vec![None; 6];
        let mut candidate_durations_ns = [None; 6];
        let mut terminal: Vec<Option<DefaultCause>> = send_failed
            .iter()
            .map(|failed| failed.then_some(DefaultCause::ProcessExit))
            .collect();
        // A reconnectable slot whose window ended only because its connection
        // was lost may recover before cutoff; protocol violations stay final.
        let mut connection_lost = [false; 6];
        // Only a connection lost during this window holds it open to cutoff
        // for recovery; a slot already absent at opening does not.
        let absent_at_open: [bool; 6] = std::array::from_fn(|slot| !sessions[slot].connected());
        let mut frames = [0_u32; 6];
        // A valid candidate is replaceable until cutoff. Keep receiving for the
        // entire declared window so scheduling cannot decide which revision is
        // authoritative.
        while Instant::now() < deadline {
            let mut progress = false;
            for slot in 0..6 {
                if terminal[slot].is_some() {
                    if connection_lost[slot] && sessions[slot].take_reconnected() {
                        progress = true;
                        terminal[slot] = None;
                        connection_lost[slot] = false;
                    } else {
                        continue;
                    }
                }
                match sessions[slot].try_recv() {
                    Ok(ReadEvent::Frame(raw)) => {
                        progress = true;
                        frames[slot] += 1;
                        received_frames[slot] += 1;
                        let ordinal = receipt_ordinal(observations[slot].day, slot, frames[slot]);
                        if candidates[slot]
                            .as_ref()
                            .is_some_and(|candidate| candidate.final_submission)
                        {
                            audit.push(audit_record(
                                &observations[slot],
                                slot,
                                ordinal,
                                raw,
                                "ignored",
                                "after-final-submission",
                            ));
                            continue;
                        }
                        if frames[slot] > plan.policy.max_frames_per_window {
                            terminal[slot] = Some(DefaultCause::ResourceLimit);
                            audit.push(audit_record(
                                &observations[slot],
                                slot,
                                ordinal,
                                raw,
                                "default",
                                "frame-count-limit",
                            ));
                            continue;
                        }
                        match parse_action_message(
                            &raw,
                            &plan.game_definition_id,
                            &observations[slot],
                        ) {
                            Ok(candidate) => {
                                let receipt = action_receipt_message(
                                    &plan.game_definition_id,
                                    &observations[slot],
                                    ordinal,
                                    runner_sender_sequence,
                                    Some(&candidate),
                                    None,
                                );
                                runner_sender_sequence += 1;
                                let _ = sessions[slot].send_value(&receipt);
                                if let Some(previous) = seen[slot].get(&candidate.message_id) {
                                    if previous != &raw {
                                        terminal[slot] = Some(DefaultCause::TransportError);
                                        candidates[slot] = None;
                                        audit.push(audit_record(
                                            &observations[slot],
                                            slot,
                                            ordinal,
                                            raw,
                                            "default",
                                            "idempotency-conflict",
                                        ));
                                    } else {
                                        audit.push(audit_record(
                                            &observations[slot],
                                            slot,
                                            ordinal,
                                            raw,
                                            "ignored",
                                            "idempotent-repeat",
                                        ));
                                    }
                                } else {
                                    seen[slot].insert(candidate.message_id.clone(), raw.clone());
                                    let replace = candidates[slot].as_ref().is_none_or(|current| {
                                        candidate.revision > current.revision
                                    });
                                    audit.push(audit_record(
                                        &observations[slot],
                                        slot,
                                        ordinal,
                                        raw,
                                        if replace { "candidate" } else { "ignored" },
                                        if replace { "valid" } else { "lower-revision" },
                                    ));
                                    if replace {
                                        candidates[slot] = Some(candidate);
                                        candidate_durations_ns[slot] = Some(
                                            u64::try_from(window_started.elapsed().as_nanos())
                                                .unwrap_or(u64::MAX),
                                        );
                                    }
                                }
                            }
                            Err(detail) => {
                                if std::env::var_os("TAC_SCM_RUNNER_INHERIT_STDERR").is_some()
                                    && observations[slot].day == 0
                                {
                                    eprintln!("runner rejected slot {slot}: {detail}");
                                }
                                let receipt = action_receipt_message(
                                    &plan.game_definition_id,
                                    &observations[slot],
                                    ordinal,
                                    runner_sender_sequence,
                                    None,
                                    Some(&detail),
                                );
                                runner_sender_sequence += 1;
                                let _ = sessions[slot].send_value(&receipt);
                                let late =
                                    matches!(detail.as_str(), "wrong-day" | "stale-observation");
                                if !late {
                                    terminal[slot] = Some(DefaultCause::TransportError);
                                }
                                audit.push(audit_record(
                                    &observations[slot],
                                    slot,
                                    ordinal,
                                    raw,
                                    if late { "ignored" } else { "default" },
                                    &detail,
                                ));
                            }
                        }
                    }
                    Ok(ReadEvent::Oversize(length)) => {
                        progress = true;
                        received_frames[slot] += 1;
                        terminal[slot] = Some(DefaultCause::ResourceLimit);
                        audit.push(audit_record(
                            &observations[slot],
                            slot,
                            receipt_ordinal(observations[slot].day, slot, 1),
                            length.to_be_bytes().to_vec(),
                            "default",
                            "oversized-frame",
                        ));
                    }
                    Ok(ReadEvent::Eof) => {
                        if std::env::var_os("TAC_SCM_RUNNER_INHERIT_STDERR").is_some() {
                            eprintln!("runner observed EOF for slot {slot}");
                        }
                        progress = true;
                        terminal[slot] = Some(DefaultCause::ProcessExit);
                        connection_lost[slot] = sessions[slot].reconnectable();
                    }
                    Ok(ReadEvent::Io(detail)) => {
                        if std::env::var_os("TAC_SCM_RUNNER_INHERIT_STDERR").is_some() {
                            eprintln!("runner input error for slot {slot}: {detail}");
                        }
                        progress = true;
                        terminal[slot] = Some(DefaultCause::TransportError);
                        connection_lost[slot] = sessions[slot].reconnectable();
                        audit.push(audit_record(
                            &observations[slot],
                            slot,
                            receipt_ordinal(observations[slot].day, slot, 1),
                            Vec::new(),
                            "default",
                            &detail,
                        ));
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => {
                        terminal[slot] = Some(DefaultCause::ProcessExit);
                        connection_lost[slot] = sessions[slot].reconnectable();
                    }
                }
            }
            if !progress {
                thread::sleep(Duration::from_millis(1));
            }
            if (0..6).all(|slot| {
                (terminal[slot].is_some() && (!connection_lost[slot] || absent_at_open[slot]))
                    || candidates[slot]
                        .as_ref()
                        .is_some_and(|candidate| candidate.final_submission)
            }) {
                break;
            }
        }
        let inputs: Vec<ActionInput> = observations
            .iter()
            .enumerate()
            .map(|(slot, observation)| {
                if let Some(candidate) = candidates[slot].take() {
                    let _ = candidate.raw.len();
                    ActionInput::Accepted(candidate.action)
                } else {
                    defaults[slot] += 1;
                    ActionInput::Default {
                        agent_id: observation.agent_id.clone(),
                        cause: terminal[slot].unwrap_or(DefaultCause::Timeout),
                    }
                }
            })
            .collect();
        for ((summary, observation), input) in
            thin_summaries.iter_mut().zip(&observations).zip(&inputs)
        {
            summary.observe_action(observation, input);
        }
        for (durations, duration) in decision_durations_ns.iter_mut().zip(candidate_durations_ns) {
            if let Some(duration) = duration {
                durations.push(duration);
            }
        }
        replay_inputs.push(inputs.clone());
        for (slot, cause) in terminal.iter().enumerate() {
            if cause.is_some() && !quarantined[slot] {
                if sessions[slot].reconnectable() {
                    // Remote recovery (draft1 section 10): drop a violating
                    // connection, never the slot; it defaults until it returns.
                    if !connection_lost[slot] {
                        sessions[slot].disconnect();
                    }
                } else {
                    sessions[slot].stop();
                    quarantined[slot] = true;
                }
            }
        }
        let committed_inputs = inputs.clone();
        let outcome = engine.commit_day(inputs)?;
        recorder.day(&observations, &committed_inputs, engine.latest_rejections())?;
        for (slot, session) in sessions.iter_mut().enumerate() {
            if !quarantined[slot] {
                let message = action_committed_message(
                    &plan.game_definition_id,
                    &observations[slot],
                    &committed_inputs[slot],
                    &engine.latest_rejections()[slot],
                    engine_sender_sequence,
                );
                engine_sender_sequence += 1;
                let _ = session.send_value(&message);
            }
        }
        if let Some(result) = outcome {
            recorder.finish(&result)?;
            for (slot, session) in sessions.iter_mut().enumerate() {
                if !quarantined[slot] {
                    let message = game_finished_message(
                        &plan.game_definition_id,
                        &observations[slot].agent_id,
                        &result,
                        engine_sender_sequence,
                    );
                    engine_sender_sequence += 1;
                    let _ = session.send_value(&message);
                }
            }
            sessions.iter_mut().for_each(|session| session.stop());
            // Training runs may skip the post-game replay check; the
            // game itself is unchanged and the result records the skip.
            let replay_verified = !training_skip_replay();
            if replay_verified {
                verify_replay(&plan.engine_plan, &replay_inputs, &result)?;
            }
            return finalize(
                plan,
                result,
                audit,
                defaults,
                audit_evidence,
                event_delivery,
                received_frames,
                thin_summaries,
                decision_durations_ns,
                replay_verified,
            );
        }
    }
}

/// Training records: with `TAC_SCM_TRAINING_RECORD=PATH`, a CBOR
/// sequence of one header, then per day one record per agent (the observation
/// the engine sent, the action it committed, how it was caused, and the
/// engine's command rejections), then the final scores. Engine semantics and
/// every other output are unchanged.
struct TrainingRecorder {
    out: Option<io::BufWriter<File>>,
}

impl TrainingRecorder {
    fn from_environment(game_definition_id: &str) -> Result<Self, RunnerError> {
        let Ok(path) = std::env::var("TAC_SCM_TRAINING_RECORD") else {
            return Ok(Self { out: None });
        };
        let mut recorder = Self {
            out: Some(io::BufWriter::new(
                File::create(path).map_err(RunnerError::Io)?,
            )),
        };
        recorder.write(&Value::Map(vec![
            ("type".into(), Value::Text("header".into())),
            (
                "schema".into(),
                Value::Text("tac-scm-training-record/1".into()),
            ),
            (
                "game_definition_id".into(),
                Value::Text(game_definition_id.into()),
            ),
        ]))?;
        Ok(recorder)
    }

    fn write(&mut self, value: &Value) -> Result<(), RunnerError> {
        if let Some(out) = self.out.as_mut() {
            out.write_all(&value.encode()).map_err(RunnerError::Io)?;
        }
        Ok(())
    }

    fn day(
        &mut self,
        observations: &[Observation],
        inputs: &[ActionInput],
        rejections: &[Vec<CommandRejection>],
    ) -> Result<(), RunnerError> {
        if self.out.is_none() {
            return Ok(());
        }
        for (slot, (observation, input)) in observations.iter().zip(inputs).enumerate() {
            let (action, cause) = match input {
                ActionInput::Accepted(action) => (action.value(), "agent-submission"),
                ActionInput::Default { cause, .. } => (
                    GameActionSet::empty(
                        &observation.agent_id,
                        &observation.id,
                        observation.day,
                        0,
                    )
                    .value(),
                    match cause {
                        DefaultCause::Missing => "missing",
                        DefaultCause::Timeout => "timeout",
                        DefaultCause::ProcessExit => "process-exit",
                        DefaultCause::ResourceLimit => "resource-limit",
                        DefaultCause::TransportError => "transport-error",
                    },
                ),
            };
            let rejected = rejections
                .get(slot)
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            Value::Map(vec![
                                ("command_id".into(), Value::Text(item.command_id.clone())),
                                ("code".into(), Value::Text(item.code.into())),
                                ("path".into(), Value::Text(item.field_path.clone())),
                            ])
                        })
                        .collect()
                })
                .unwrap_or_default();
            self.write(&Value::Map(vec![
                ("type".into(), Value::Text("day".into())),
                ("day".into(), Value::U64(u64::from(observation.day))),
                ("slot".into(), Value::U64(slot as u64)),
                ("agent_id".into(), Value::Text(observation.agent_id.clone())),
                ("observation".into(), observation.value()),
                ("action".into(), action),
                ("cause".into(), Value::Text(cause.into())),
                ("rejections".into(), Value::Array(rejected)),
            ]))?;
        }
        Ok(())
    }

    fn finish(&mut self, result: &GameResult) -> Result<(), RunnerError> {
        let scores = result
            .scores
            .iter()
            .map(|(agent_id, score)| {
                Value::Map(vec![
                    ("agent_id".into(), Value::Text(agent_id.clone())),
                    ("score_microcredits".into(), Value::I64(*score)),
                ])
            })
            .collect();
        self.write(&Value::Map(vec![
            ("type".into(), Value::Text("result".into())),
            ("scores".into(), Value::Array(scores)),
        ]))?;
        if let Some(out) = self.out.as_mut() {
            out.flush().map_err(RunnerError::Io)?;
        }
        Ok(())
    }
}

fn training_without_commitments() -> bool {
    std::env::var("TAC_SCM_TRAINING_NO_COMMITMENTS").as_deref() == Ok("1")
}

fn training_skip_replay() -> bool {
    // Without state commitments the replay check cannot match; it is skipped.
    if training_without_commitments() {
        return true;
    }
    std::env::var("TAC_SCM_TRAINING_SKIP_REPLAY").as_deref() == Ok("1")
}

fn verify_replay(
    plan: &GamePlan,
    inputs: &[Vec<ActionInput>],
    expected: &GameResult,
) -> Result<(), RunnerError> {
    let mut engine = if expected.events_captured {
        GameEngine::new_with_event_capture(plan.clone())?
    } else {
        GameEngine::new(plan.clone())?
    };
    let mut actual = None;
    for day_inputs in inputs {
        engine.open_day()?;
        actual = engine.commit_day(day_inputs.clone())?;
    }
    let actual = actual.ok_or(RunnerError::Plan("incomplete replay"))?;
    if actual.transcript_bytes != expected.transcript_bytes
        || actual.event_log_bytes != expected.event_log_bytes
        || actual.event_count != expected.event_count
        || actual.final_state_sha256 != expected.final_state_sha256
    {
        return Err(RunnerError::Plan("replay mismatch"));
    }
    Ok(())
}

fn audit_record(
    observation: &Observation,
    slot: usize,
    ordinal: u64,
    raw: Vec<u8>,
    disposition: &'static str,
    detail: &str,
) -> AuditRecord {
    AuditRecord {
        day: observation.day,
        slot: u8::try_from(slot).unwrap(),
        receipt_ordinal: ordinal,
        raw_bytes: raw,
        disposition,
        detail: detail.into(),
    }
}

fn receipt_ordinal(day: u32, slot: usize, local: u32) -> u64 {
    u64::from(day) * 6 * 17 + slot as u64 * 17 + u64::from(local)
}

fn finalize(
    plan: &FrozenGamePlan,
    result: GameResult,
    mut audit: Vec<AuditRecord>,
    default_counts: [u64; 6],
    audit_evidence: AuditEvidence,
    event_delivery: &'static str,
    received_frames: [u64; 6],
    thin_summaries: [ThinAgentSummary; 6],
    decision_durations_ns: [Vec<u64>; 6],
    replay_verified: bool,
) -> Result<RunnerOutput, RunnerError> {
    audit.sort_by_key(|record| (record.day, record.slot, record.receipt_ordinal));
    if audit_evidence == AuditEvidence::Anomalies {
        audit.retain(|record| record.disposition != "candidate" || record.detail != "valid");
    }
    let audit_record_count = audit.len();
    let audit_bytes = Value::Map(vec![
        (
            "schema_id".into(),
            Value::Text(
                if audit_evidence == AuditEvidence::Full {
                    AUDIT_SCHEMA_ID
                } else {
                    "tac-scm-protocol-anomalies/1"
                }
                .into(),
            ),
        ),
        ("plan_sha256".into(), Value::Bytes(plan.sha256().to_vec())),
        (
            "records".into(),
            Value::Array(audit.iter().map(AuditRecord::value).collect()),
        ),
    ])
    .encode();
    let audit_sha256 = sha256(&audit_bytes);
    let process_summary_bytes = Value::Map(vec![
        (
            "schema_id".into(),
            Value::Text("tac-scm-process-summary/2".into()),
        ),
        (
            "received_frames".into(),
            Value::Array(received_frames.into_iter().map(Value::U64).collect()),
        ),
        (
            "default_counts".into(),
            Value::Array(default_counts.into_iter().map(Value::U64).collect()),
        ),
        (
            "anomaly_record_count".into(),
            Value::U64(u64::try_from(audit_record_count).unwrap()),
        ),
        (
            "agent_summaries".into(),
            Value::Array(
                thin_summaries
                    .into_iter()
                    .map(ThinAgentSummary::value)
                    .collect(),
            ),
        ),
        ("event_delivery".into(), Value::Text(event_delivery.into())),
        ("event_count".into(), Value::U64(result.event_count)),
    ])
    .encode();
    let process_summary_sha256 = sha256(&process_summary_bytes);
    let runtime_summary_bytes = Value::Map(vec![
        (
            "schema_id".into(),
            Value::Text("tac-scm-runtime-summary/1".into()),
        ),
        (
            "measurement_boundary".into(),
            Value::Text("observation-send-to-authoritative-valid-candidate".into()),
        ),
        (
            "decision_samples".into(),
            Value::Array(
                decision_durations_ns
                    .iter()
                    .map(|values| Value::U64(u64::try_from(values.len()).unwrap()))
                    .collect(),
            ),
        ),
        (
            "decision_p95_ns".into(),
            Value::Array(
                decision_durations_ns
                    .iter()
                    .map(|values| Value::U64(nearest_rank(values, 95)))
                    .collect(),
            ),
        ),
        (
            "decision_max_ns".into(),
            Value::Array(
                decision_durations_ns
                    .iter()
                    .map(|values| Value::U64(values.iter().copied().max().unwrap_or(0)))
                    .collect(),
            ),
        ),
    ])
    .encode();
    let game_result_manifest_bytes = Value::Map(vec![
        (
            "schema_id".into(),
            Value::Text("tac-scm-game-result-manifest/2".into()),
        ),
        (
            "game_definition_id".into(),
            Value::Text(plan.game_definition_id.clone()),
        ),
        (
            "action_transcript_sha256".into(),
            Value::Bytes(result.transcript_sha256.to_vec()),
        ),
        (
            "event_log_sha256".into(),
            Value::Bytes(result.event_log_sha256.to_vec()),
        ),
        (
            "economic_summary_sha256".into(),
            Value::Bytes(result.economic_summary_sha256.to_vec()),
        ),
        (
            "process_summary_sha256".into(),
            Value::Bytes(process_summary_sha256.to_vec()),
        ),
        ("event_count".into(), Value::U64(result.event_count)),
        ("event_delivery".into(), Value::Text(event_delivery.into())),
        (
            "final_state_sha256".into(),
            Value::Bytes(result.final_state_sha256.to_vec()),
        ),
        ("termination".into(), Value::Text("completed".into())),
        (
            "scores".into(),
            Value::Array(
                result
                    .scores
                    .iter()
                    .map(|(agent, score)| {
                        Value::Map(vec![
                            ("agent_id".into(), Value::Text(agent.clone())),
                            ("cash_microcredits".into(), Value::I64(*score)),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "protocol_audit_sha256".into(),
            Value::Bytes(audit_sha256.to_vec()),
        ),
    ])
    .encode();
    let game_result_id = format!("result/{}", hex(&sha256(&game_result_manifest_bytes)));
    let record = result_record_value(
        plan,
        &result,
        &default_counts,
        audit_sha256,
        &game_result_id,
        event_delivery,
        process_summary_sha256,
        replay_verified,
    )?;
    validate_result_record_value(&record)?;
    let result_record_bytes = record.encode();
    Ok(RunnerOutput {
        result,
        plan_sha256: plan.sha256(),
        audit_bytes,
        audit_sha256,
        game_result_manifest_bytes,
        result_record_bytes,
        game_result_id,
        default_counts,
        event_delivery,
        full_audit: audit_evidence == AuditEvidence::Full,
        audit_record_count,
        process_summary_bytes,
        process_summary_sha256,
        runtime_summary_bytes,
    })
}

fn nearest_rank(values: &[u64], percentile: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut ordered = values.to_vec();
    ordered.sort_unstable();
    let index = (ordered.len() * percentile).div_ceil(100).saturating_sub(1);
    ordered[index]
}

fn validate_result_record_value(value: &Value) -> Result<(), RunnerError> {
    let Value::Map(map) = value else {
        return Err(RunnerError::Plan("result record map"));
    };
    let actual: BTreeSet<_> = map.iter().map(|(key, _)| key.as_str()).collect();
    let expected: BTreeSet<_> = RESULT_RECORD_FIELDS.iter().copied().collect();
    if actual != expected {
        return Err(RunnerError::Plan("result record closed fields"));
    }
    for field in [
        "ordered_agent_artifact_sha256",
        "scores_microcredits",
        "default_counts",
    ] {
        let Some(Value::Array(values)) = map
            .iter()
            .find_map(|(key, value)| (key == field).then_some(value))
        else {
            return Err(RunnerError::Plan("result record six-entry field"));
        };
        if values.len() != 6 {
            return Err(RunnerError::Plan("result record six-entry field"));
        }
    }
    Ok(())
}

fn result_record_value(
    plan: &FrozenGamePlan,
    result: &GameResult,
    defaults: &[u64; 6],
    audit_sha256: [u8; 32],
    game_result_id: &str,
    event_delivery: &str,
    process_summary_sha256: [u8; 32],
    replay_verified: bool,
) -> Result<Value, RunnerError> {
    Ok(Value::Map(vec![
        ("campaign_id".into(), Value::Text(plan.campaign_id.clone())),
        (
            "campaign_manifest_sha256".into(),
            Value::Bytes(plan.campaign_manifest_sha256.to_vec()),
        ),
        ("schedule_ordinal".into(), Value::U64(plan.schedule_ordinal)),
        ("block_index".into(), Value::U64(plan.block_index)),
        ("context_id".into(), Value::Text(plan.context_id.clone())),
        (
            "seat_rotation".into(),
            Value::U64(u64::from(plan.seat_rotation)),
        ),
        (
            "game_definition_id".into(),
            Value::Text(plan.game_definition_id.clone()),
        ),
        ("game_result_id".into(), Value::Text(game_result_id.into())),
        (
            "profile_id".into(),
            Value::Text(plan.engine_plan.profile_id.clone()),
        ),
        (
            "engine_artifact_sha256".into(),
            Value::Bytes(plan.engine_plan.engine_identity.to_vec()),
        ),
        (
            "runner_artifact_sha256".into(),
            Value::Bytes(runner_identity()?.to_vec()),
        ),
        (
            "ordered_agent_artifact_sha256".into(),
            Value::Array(
                plan.agents
                    .iter()
                    .map(|artifact| Value::Bytes(artifact.sha256.to_vec()))
                    .collect(),
            ),
        ),
        (
            "root_seed_commitment".into(),
            Value::Bytes(sha256(&plan.engine_plan.root_seed).to_vec()),
        ),
        ("termination".into(), Value::Text("completed".into())),
        (
            "scores_microcredits".into(),
            Value::Array(
                result
                    .scores
                    .iter()
                    .map(|(_, score)| Value::I64(*score))
                    .collect(),
            ),
        ),
        (
            "default_counts".into(),
            Value::Array(defaults.iter().copied().map(Value::U64).collect()),
        ),
        (
            "action_transcript_sha256".into(),
            Value::Bytes(result.transcript_sha256.to_vec()),
        ),
        (
            "economic_summary_sha256".into(),
            Value::Bytes(result.economic_summary_sha256.to_vec()),
        ),
        ("event_count".into(), Value::U64(result.event_count)),
        ("event_delivery".into(), Value::Text(event_delivery.into())),
        (
            "event_log_sha256".into(),
            Value::Bytes(result.event_log_sha256.to_vec()),
        ),
        (
            "final_state_sha256".into(),
            Value::Bytes(result.final_state_sha256.to_vec()),
        ),
        ("replay_verified".into(), Value::Bool(replay_verified)),
        (
            "protocol_audit_sha256".into(),
            Value::Bytes(audit_sha256.to_vec()),
        ),
        (
            "process_summary_sha256".into(),
            Value::Bytes(process_summary_sha256.to_vec()),
        ),
        ("infrastructure_attempts".into(), Value::Array(Vec::new())),
    ]))
}

fn runner_identity() -> Result<[u8; 32], RunnerError> {
    let path = std::env::current_exe()?;
    runner_identity_from(&path)
}

fn runner_identity_from(path: &Path) -> Result<[u8; 32], RunnerError> {
    let bytes = fs::read(path)?;
    Ok(sha256(&bytes))
}

pub fn identity_document() -> Result<String, RunnerError> {
    Ok(format!(
        concat!(
            "{{\"engine_cargo_version\":\"{}\",",
            "\"engine_semantic_id\":\"{}\",",
            "\"engine_semantic_sha256\":\"{}\",",
            "\"event_schema_id\":\"{}\",",
            "\"event_delivery_default\":\"off\",",
            "\"message_schema_id\":\"{}\",",
            "\"profile_id\":\"{}\",",
            "\"profile_selection\":\"TAC_SCM_PROFILE\",",
            "\"remote_session_protocol\":\"tac-scm-session/1\",",
            "\"run_evidence_contract\":\"tac-scm-run-evidence/1\",",
            "\"runner_cargo_version\":\"{}\",",
            "\"runner_executable_sha256\":\"{}\",",
            "\"selectable_profiles\":{{\"{}\":\"{}\",\"{}\":\"{}\"}}}}\n"
        ),
        ENGINE_PACKAGE_VERSION,
        ENGINE_SEMANTIC_ID,
        hex(&sha256(ENGINE_SEMANTIC_ID.as_bytes())),
        EVENT_SCHEMA_ID,
        MESSAGE_SCHEMA_ID,
        PROFILE_ID,
        env!("CARGO_PKG_VERSION"),
        hex(&runner_identity()?),
        PROFILE_ID,
        ENGINE_SEMANTIC_ID,
        DRAFT3_PROFILE_ID,
        DRAFT3_ENGINE_SEMANTIC_ID,
    ))
}

/// The normative profile for plans that `one`, `batch`, `planned`, and
/// `serve` materialize. `TAC_SCM_PROFILE` selects it explicitly and is frozen
/// into every plan through its profile id; unset means the default draft 2.
fn engine_plan_from_environment(root_seed: [u8; 32]) -> Result<GamePlan, RunnerError> {
    match std::env::var("TAC_SCM_PROFILE") {
        Err(std::env::VarError::NotPresent) => Ok(GamePlan::materialize(root_seed)?),
        Ok(profile) if profile == PROFILE_ID || profile == DRAFT3_PROFILE_ID => {
            Ok(GamePlan::materialize_profile(root_seed, &profile)?)
        }
        _ => Err(RunnerError::Plan("profile environment")),
    }
}

fn artifact_reference(id: &str, revision: u32) -> Value {
    Value::Map(vec![
        ("id".into(), Value::Text(id.into())),
        ("revision".into(), Value::U64(u64::from(revision))),
        (
            "sha256".into(),
            Value::Bytes(sha256(id.as_bytes()).to_vec()),
        ),
    ])
}

fn seed_manifest(plan: &GamePlan) -> Value {
    Value::Map(vec![
        (
            "root_seed_commitment".into(),
            Value::Bytes(sha256(&plan.root_seed).to_vec()),
        ),
        ("seed_reference".into(), Value::Text("runner-secret".into())),
        (
            "derivation_id".into(),
            Value::Text("hmac-sha256-named-stream/v1".into()),
        ),
        ("prng_id".into(), Value::Text("chacha20-rfc8439/v1".into())),
        (
            "sampler_ids".into(),
            Value::Array(vec![
                Value::Text("uniform-rejection/v1".into()),
                Value::Text("poisson-inversion-q448/v1".into()),
            ]),
        ),
        (
            "stream_registry".into(),
            Value::Array(
                plan.stream_patterns
                    .iter()
                    .map(|pattern| {
                        Value::Map(vec![
                            ("kind".into(), Value::Text("pattern".into())),
                            (
                                "segments".into(),
                                Value::Array(vec![Value::Text(pattern.clone())]),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

fn catalog_value() -> Value {
    let mut suppliers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for component in &COMPONENTS {
        for supplier in component.suppliers {
            suppliers.entry(supplier).or_default().push(component.id);
        }
    }
    Value::Map(vec![
        (
            "components".into(),
            Value::Array(
                COMPONENTS
                    .iter()
                    .map(|component| {
                        Value::Map(vec![
                            ("id".into(), Value::Text(component.id.into())),
                            (
                                "base_microcredits".into(),
                                Value::I64(component.base_microcredits),
                            ),
                            (
                                "suppliers".into(),
                                Value::Array(
                                    component
                                        .suppliers
                                        .iter()
                                        .map(|supplier| Value::Text((*supplier).into()))
                                        .collect(),
                                ),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "products".into(),
            Value::Array(
                PRODUCTS
                    .iter()
                    .map(|product| {
                        Value::Map(vec![
                            ("id".into(), Value::Text(product.id.into())),
                            (
                                "components".into(),
                                Value::Array(
                                    product
                                        .components
                                        .iter()
                                        .map(|component| Value::Text((*component).into()))
                                        .collect(),
                                ),
                            ),
                            ("cycles".into(), Value::U64(u64::from(product.cycles))),
                            ("segment".into(), Value::Text(product.segment.into())),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "suppliers".into(),
            Value::Array(
                suppliers
                    .into_iter()
                    .map(|(supplier, components)| {
                        Value::Map(vec![
                            ("id".into(), Value::Text(supplier.into())),
                            (
                                "components".into(),
                                Value::Array(
                                    components
                                        .into_iter()
                                        .map(|component| Value::Text(component.into()))
                                        .collect(),
                                ),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

fn semantic_configuration(plan: &GamePlan) -> Value {
    let profile = artifact_reference(&plan.profile_id, plan.profile_revision);
    let finance_rounding_id = if plan.historical_factors().financial_carry {
        "whole-credit-math-round-with-directed-decimal-storage/v1"
    } else {
        "round-half-even"
    };
    let modules = vec![
        artifact_reference("supplier-market-draft1", plan.supplier_module_revision),
        artifact_reference("component-contracts-draft2", plan.contracts_revision),
        artifact_reference("conformance-draft1", plan.conformance_revision),
    ];
    let seed = seed_manifest(plan);
    Value::Map(vec![
        ("profile".into(), profile),
        ("modules".into(), Value::Array(modules)),
        (
            "participants".into(),
            Value::Array(plan.participants.iter().cloned().map(Value::Text).collect()),
        ),
        (
            "calendar".into(),
            Value::Map(vec![
                ("first_day".into(), Value::U64(0)),
                ("last_day".into(), Value::U64(219)),
                ("days".into(), Value::U64(220)),
            ]),
        ),
        (
            "factory".into(),
            Value::Map(vec![
                (
                    "cycles_per_day".into(),
                    Value::U64(u64::from(FACTORY_CYCLES)),
                ),
                ("production_lines_max".into(), Value::U64(256)),
                ("delivery_lines_max".into(), Value::U64(4096)),
            ]),
        ),
        ("catalog".into(), catalog_value()),
        (
            "finance".into(),
            Value::Map(vec![
                (
                    "loan_rate_ppb".into(),
                    Value::U64(u64::from(plan.loan_rate_ppb)),
                ),
                (
                    "deposit_rate_ppb".into(),
                    Value::U64(u64::from(plan.deposit_rate_ppb)),
                ),
                (
                    "storage_rate_ppb".into(),
                    Value::U64(u64::from(plan.storage_rate_ppb)),
                ),
                (
                    "rounding_id".into(),
                    Value::Text(finance_rounding_id.into()),
                ),
            ]),
        ),
        (
            "customer".into(),
            Value::Map(vec![
                (
                    "low_initial_q_nano".into(),
                    Value::U64(plan.initial_means_nano[0]),
                ),
                (
                    "mid_initial_q_nano".into(),
                    Value::U64(plan.initial_means_nano[1]),
                ),
                (
                    "high_initial_q_nano".into(),
                    Value::U64(plan.initial_means_nano[2]),
                ),
                ("trend_min_ppb".into(), Value::U64(950_000_000)),
                ("trend_max_ppb".into(), Value::U64(1_050_000_000)),
                ("quantity_min".into(), Value::U64(1)),
                ("quantity_max".into(), Value::U64(20)),
                ("lead_min".into(), Value::U64(3)),
                ("lead_max".into(), Value::U64(12)),
                ("reserve_percent_min".into(), Value::U64(75)),
                ("reserve_percent_max".into(), Value::U64(125)),
                ("penalty_percent_min".into(), Value::U64(5)),
                ("penalty_percent_max".into(), Value::U64(15)),
                ("penalty_cap".into(), Value::U64(5)),
                ("report_period".into(), Value::U64(20)),
            ]),
        ),
        (
            "supplier".into(),
            Value::Map(vec![
                ("nominal_capacity".into(), Value::U64(550)),
                ("initial_capacity_min".into(), Value::U64(357)),
                ("initial_capacity_max".into(), Value::U64(743)),
                ("reservation_horizon".into(), Value::U64(20)),
                ("reservation_reduction_ppb".into(), Value::U64(5_000_000)),
                ("price_discount_ppb".into(), Value::U64(500_000_000)),
                ("single_source_apr_ppb".into(), Value::U64(750_000_000)),
                ("two_source_apr_ppb".into(), Value::U64(450_000_000)),
                ("allocation_exponent".into(), Value::U64(3)),
                ("down_payment_ppb".into(), Value::U64(100_000_000)),
            ]),
        ),
        ("randomness".into(), Value::Map(vec![("seed".into(), seed)])),
        (
            "arithmetic".into(),
            Value::Map(vec![
                ("money_scale".into(), Value::U64(1_000_000)),
                ("rate_scale".into(), Value::U64(1_000_000_000)),
                ("intermediate_bits".into(), Value::U64(128)),
                ("rounding_id".into(), Value::Text("round-half-even".into())),
                ("overflow".into(), Value::Text("abort-or-reject".into())),
            ]),
        ),
    ])
}

fn bootstrap_message(plan: &FrozenGamePlan, slot: usize) -> Value {
    let engine = &plan.engine_plan;
    let modules = vec![
        artifact_reference("supplier-market-draft1", engine.supplier_module_revision),
        artifact_reference("component-contracts-draft2", engine.contracts_revision),
        artifact_reference("conformance-draft1", engine.conformance_revision),
    ];
    let payload = Value::Map(vec![
        ("type".into(), Value::Text("game-bootstrap".into())),
        (
            "game_definition_id".into(),
            Value::Text(plan.game_definition_id.clone()),
        ),
        (
            "schema".into(),
            artifact_reference("contracts-draft2", engine.contracts_revision),
        ),
        (
            "profile".into(),
            artifact_reference(&engine.profile_id, engine.profile_revision),
        ),
        ("modules".into(), Value::Array(modules)),
        (
            "agent_id".into(),
            Value::Text(engine.participants[slot].clone()),
        ),
        (
            "agent_slot".into(),
            Value::U64(u64::try_from(slot).unwrap()),
        ),
        (
            "roster".into(),
            Value::Array(
                engine
                    .participants
                    .iter()
                    .cloned()
                    .map(Value::Text)
                    .collect(),
            ),
        ),
        (
            "semantic_configuration".into(),
            semantic_configuration(engine),
        ),
        ("catalog".into(), catalog_value()),
        ("latest_committed_day".into(), Value::Null),
    ]);
    envelope(
        &plan.game_definition_id,
        &format!("message/runner/bootstrap/{slot}"),
        "runner",
        "runner",
        &engine.participants[slot],
        None,
        "setup",
        u64::try_from(slot).unwrap(),
        payload,
    )
}

fn observation_message(game_id: &str, observation: &Observation, sequence: u64) -> Value {
    let payload = observation.value();
    envelope(
        game_id,
        &format!("message/engine/{sequence}"),
        "engine",
        "engine",
        &observation.agent_id,
        Some(observation.day),
        "observation",
        sequence,
        payload,
    )
}

fn action_receipt_message(
    game_id: &str,
    observation: &Observation,
    ordinal: u64,
    sender_sequence: u64,
    candidate: Option<&Candidate>,
    error: Option<&str>,
) -> Value {
    let action_digest = candidate.map(|item| sha256(&item.action.value().encode()));
    let payload = Value::Map(vec![
        ("type".into(), Value::Text("action-receipt".into())),
        ("day".into(), Value::U64(u64::from(observation.day))),
        (
            "revision".into(),
            candidate.map_or(Value::Null, |item| Value::U64(u64::from(item.revision))),
        ),
        (
            "action_digest".into(),
            action_digest.map_or(Value::Null, |digest| Value::Bytes(digest.to_vec())),
        ),
        ("receipt_ordinal".into(), Value::U64(ordinal)),
        (
            "candidate_valid".into(),
            Value::Bool(candidate.is_some() && error.is_none()),
        ),
        (
            "errors".into(),
            Value::Array(
                error
                    .into_iter()
                    .map(|detail| {
                        Value::Map(vec![
                            ("code".into(), Value::Text("schema-invalid".into())),
                            ("path".into(), Value::Text("payload".into())),
                            ("detail".into(), Value::Text(detail.into())),
                        ])
                    })
                    .collect(),
            ),
        ),
    ]);
    envelope(
        game_id,
        &format!("message/runner/receipt/{}/{ordinal}", observation.agent_id),
        "runner",
        "runner",
        &observation.agent_id,
        Some(observation.day),
        "action-window",
        sender_sequence,
        payload,
    )
}

fn action_committed_message(
    game_id: &str,
    observation: &Observation,
    input: &ActionInput,
    rejections: &[CommandRejection],
    sequence: u64,
) -> Value {
    let (action, cause) = match input {
        ActionInput::Accepted(action) => (action.clone(), "agent-submission"),
        ActionInput::Default { cause, .. } => (
            GameActionSet::empty(&observation.agent_id, &observation.id, observation.day, 0),
            match cause {
                DefaultCause::Missing => "missing",
                DefaultCause::Timeout => "timeout",
                DefaultCause::ProcessExit => "process-exit",
                DefaultCause::ResourceLimit => "resource-limit",
                DefaultCause::TransportError => "transport-error",
            },
        ),
    };
    let command_ids: Vec<&str> = action
        .customer_offers
        .iter()
        .map(|command| command.command_id.as_str())
        .chain(
            action
                .supplier_rfqs
                .iter()
                .map(|command| command.command_id.as_str()),
        )
        .chain(
            action
                .supplier_orders
                .iter()
                .map(|command| command.command_id.as_str()),
        )
        .chain(
            action
                .production_schedule
                .iter()
                .map(|line| line.line_id.as_str()),
        )
        .chain(
            action
                .delivery_schedule
                .iter()
                .map(|line| line.line_id.as_str()),
        )
        .collect();
    let command_results = command_ids
        .into_iter()
        .map(|command_id| {
            let rejection = rejections.iter().find(|item| item.command_id == command_id);
            let mut fields = vec![
                ("command_id".into(), Value::Text(command_id.into())),
                ("accepted".into(), Value::Bool(rejection.is_none())),
            ];
            if let Some(rejection) = rejection {
                fields.push((
                    "rejection".into(),
                    Value::Map(vec![
                        ("code".into(), Value::Text(rejection.code.into())),
                        ("path".into(), Value::Text(rejection.field_path.clone())),
                        ("detail".into(), Value::Text(rejection.detail.into())),
                    ]),
                ));
            }
            Value::Map(fields)
        })
        .collect();
    let payload = Value::Map(vec![
        ("type".into(), Value::Text("action-committed".into())),
        ("day".into(), Value::U64(u64::from(observation.day))),
        (
            "action_digest".into(),
            Value::Bytes(sha256(&action.value().encode()).to_vec()),
        ),
        ("cause".into(), Value::Text(cause.into())),
        ("command_results".into(), Value::Array(command_results)),
    ]);
    envelope(
        game_id,
        &format!("message/engine/{sequence}"),
        "engine",
        "engine",
        &observation.agent_id,
        Some(observation.day),
        "action-commit",
        sequence,
        payload,
    )
}

fn game_finished_message(
    game_id: &str,
    agent_id: &str,
    result: &GameResult,
    sequence: u64,
) -> Value {
    let payload = Value::Map(vec![
        ("type".into(), Value::Text("game-finished".into())),
        ("status".into(), Value::Text("completed".into())),
        (
            "scores".into(),
            Value::Array(
                result
                    .scores
                    .iter()
                    .map(|(agent, cash)| {
                        Value::Map(vec![
                            ("agent_id".into(), Value::Text(agent.clone())),
                            ("cash_microcredits".into(), Value::I64(*cash)),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "final_state_commitment".into(),
            Value::Bytes(result.final_state_sha256.to_vec()),
        ),
    ]);
    envelope(
        game_id,
        &format!("message/engine/{sequence}"),
        "engine",
        "engine",
        agent_id,
        None,
        "terminated",
        sequence,
        payload,
    )
}

#[allow(clippy::too_many_arguments)]
fn envelope(
    game_id: &str,
    message_id: &str,
    sender_role: &str,
    sender_id: &str,
    recipient: &str,
    game_day: Option<u32>,
    phase: &str,
    sender_sequence: u64,
    payload: Value,
) -> Value {
    let digest = sha256(&payload.encode());
    let mut entries = vec![
        ("schema_id".into(), Value::Text(MESSAGE_SCHEMA_ID.into())),
        (
            "schema_version".into(),
            Value::Array(vec![Value::U64(2), Value::U64(0), Value::U64(0)]),
        ),
        ("game_id".into(), Value::Text(game_id.into())),
        ("message_id".into(), Value::Text(message_id.into())),
        ("sender_role".into(), Value::Text(sender_role.into())),
        ("sender_id".into(), Value::Text(sender_id.into())),
        ("recipient".into(), Value::Text(recipient.into())),
        ("phase".into(), Value::Text(phase.into())),
        ("sender_sequence".into(), Value::U64(sender_sequence)),
        ("payload_digest".into(), Value::Bytes(digest.to_vec())),
        ("payload".into(), payload),
    ];
    if let Some(day) = game_day {
        entries.push(("game_day".into(), Value::U64(u64::from(day))));
    }
    Value::Map(entries)
}

fn parse_action_message(
    raw: &[u8],
    game_id: &str,
    observation: &Observation,
) -> Result<Candidate, String> {
    let value = decode_canonical(raw)?;
    let message = as_map(&value)?;
    closed_keys_with_optional(
        message,
        &[
            "schema_id",
            "schema_version",
            "game_id",
            "message_id",
            "sender_role",
            "sender_id",
            "recipient",
            "game_day",
            "phase",
            "sender_sequence",
            "payload_digest",
            "payload",
        ],
        &["causation_id", "final_submission"],
    )?;
    expect_text(message, "schema_id", MESSAGE_SCHEMA_ID)?;
    if get(message, "schema_version")?
        != &Value::Array(vec![Value::U64(2), Value::U64(0), Value::U64(0)])
    {
        return Err("schema-version".into());
    }
    expect_text(message, "game_id", game_id)?;
    expect_text(message, "sender_role", "manufacturer")?;
    expect_text(message, "sender_id", &observation.agent_id)?;
    expect_text(message, "recipient", "runner")?;
    expect_text(message, "phase", "action-window")?;
    if get_u64(message, "game_day")? != u64::from(observation.day) {
        return Err("wrong-day".into());
    }
    let message_id = get_text(message, "message_id")?.to_owned();
    let payload = get(message, "payload")?;
    let digest = get_bytes(message, "payload_digest")?;
    if digest != sha256(&payload.encode()) {
        return Err("payload-digest".into());
    }
    let action = parse_action(payload)?;
    if action.agent_id != observation.agent_id {
        return Err("wrong-agent".into());
    }
    if action.day != observation.day {
        return Err("wrong-day".into());
    }
    if action.observation_id != observation.id {
        return Err("stale-observation".into());
    }
    if action.revision == 0 {
        return Err("bad-revision".into());
    }
    // After the late-message checks, so a stale action is still ignored
    // rather than defaulted.
    check_action_structure(&action)?;
    Ok(Candidate {
        revision: action.revision,
        action,
        final_submission: get_optional_bool(message, "final_submission")?.unwrap_or(false),
        message_id,
        raw: raw.to_vec(),
    })
}

fn parse_action(value: &Value) -> Result<GameActionSet, String> {
    let map = as_map(value)?;
    closed_keys(
        map,
        &[
            "type",
            "agent_id",
            "observation_id",
            "day",
            "revision",
            "customer_offers",
            "supplier_rfqs",
            "supplier_orders",
            "production_schedule",
            "delivery_schedule",
        ],
    )?;
    expect_text(map, "type", "day-action-set")?;
    let customer_offers = get_array(map, "customer_offers")?
        .iter()
        .map(|item| {
            let item = as_map(item)?;
            closed_keys(item, &["command_id", "rfq_id", "unit_price"])?;
            Ok(CustomerOffer {
                command_id: get_text(item, "command_id")?.into(),
                rfq_id: get_text(item, "rfq_id")?.into(),
                unit_price: get_i64(item, "unit_price")?,
            })
        })
        .collect::<Result<_, String>>()?;
    let supplier_rfqs = get_array(map, "supplier_rfqs")?
        .iter()
        .map(|item| {
            let item = as_map(item)?;
            closed_keys(
                item,
                &[
                    "command_id",
                    "rfq_id",
                    "supplier_id",
                    "component_id",
                    "quantity",
                    "due_day",
                    "max_unit_price",
                ],
            )?;
            Ok(SupplierRfqCommand {
                command_id: get_text(item, "command_id")?.into(),
                rfq_id: get_text(item, "rfq_id")?.into(),
                supplier_id: get_text(item, "supplier_id")?.into(),
                component_id: get_text(item, "component_id")?.into(),
                quantity: get_u32(item, "quantity")?,
                due_day: get_u32(item, "due_day")?,
                max_unit_price: get_optional_i64(item, "max_unit_price")?,
            })
        })
        .collect::<Result<_, String>>()?;
    let supplier_orders = get_array(map, "supplier_orders")?
        .iter()
        .map(|item| {
            let item = as_map(item)?;
            closed_keys(item, &["command_id", "offer_id"])?;
            Ok(SupplierOrderCommand {
                command_id: get_text(item, "command_id")?.into(),
                offer_id: get_text(item, "offer_id")?.into(),
            })
        })
        .collect::<Result<_, String>>()?;
    let production_schedule = get_array(map, "production_schedule")?
        .iter()
        .map(|item| {
            let item = as_map(item)?;
            closed_keys(item, &["line_id", "priority", "product_id", "quantity"])?;
            Ok(ProductionLine {
                line_id: get_text(item, "line_id")?.into(),
                priority: get_u32(item, "priority")?,
                product_id: get_text(item, "product_id")?.into(),
                quantity: get_u32(item, "quantity")?,
            })
        })
        .collect::<Result<_, String>>()?;
    let delivery_schedule = get_array(map, "delivery_schedule")?
        .iter()
        .map(|item| {
            let item = as_map(item)?;
            closed_keys(
                item,
                &["line_id", "priority", "customer_order_id", "quantity"],
            )?;
            Ok(DeliveryLine {
                line_id: get_text(item, "line_id")?.into(),
                priority: get_u32(item, "priority")?,
                customer_order_id: get_text(item, "customer_order_id")?.into(),
                quantity: get_u32(item, "quantity")?,
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(GameActionSet {
        agent_id: get_text(map, "agent_id")?.into(),
        observation_id: get_text(map, "observation_id")?.into(),
        day: get_u32(map, "day")?,
        revision: get_u32(map, "revision")?,
        customer_offers,
        supplier_rfqs,
        supplier_orders,
        production_schedule,
        delivery_schedule,
    })
}

/// Production lines one action may carry before the engine aborts the game.
const MAX_PRODUCTION_LINES: usize = 256;
/// Delivery lines one action may carry before the engine aborts the game.
const MAX_DELIVERY_LINES: usize = 4096;

/// Structural checks that the engine makes only as game-fatal errors.
///
/// The engine's `validate_inputs` (engine-core `game.rs`, the id and
/// cardinality block) returns a `GameError` for an action set with an id that
/// fails the id pattern, two equal ids anywhere among its command ids and
/// line ids (one namespace over all five lists), more than 256 production
/// lines or more than 4,096 delivery lines; `commit_day` then fails and the
/// whole game aborts without a score. Two eligible supplier RFQs of one
/// action with the same `rfq_id` on one supplier line (same `supplier_id`
/// and `component_id`) share an engine RFQ id (it hashes agent, supplier and
/// client `rfq_id` only) and abort the game in the allocation permutation
/// (`DuplicateCandidate`).
///
/// The runner checks them here so that one entrant's bug costs that entrant
/// the ordinary malformed-message path (`schema-invalid` receipt,
/// `transport-error` default for the day) instead of aborting the game of
/// the other five. The first four checks are exactly the engine's; the
/// engine itself is unchanged, so its identity and replay are too. The
/// supplier check, `(supplier_id, component_id, rfq_id)` distinct, is the
/// narrowest static check covering every fatal case: whether both RFQs are
/// eligible depends on the posted price, which the runner cannot know, so a
/// same-line duplicate is rejected even when at most one would be eligible.
/// Reuse for another component of the same supplier is not fatal and stays
/// accepted (the engine merges the two into one alternative group).
fn check_action_structure(action: &GameActionSet) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for id in action
        .customer_offers
        .iter()
        .map(|x| &x.command_id)
        .chain(action.supplier_rfqs.iter().map(|x| &x.command_id))
        .chain(action.supplier_orders.iter().map(|x| &x.command_id))
        .chain(action.production_schedule.iter().map(|x| &x.line_id))
        .chain(action.delivery_schedule.iter().map(|x| &x.line_id))
    {
        if !valid_engine_id(id) {
            return Err("schema-invalid: invalid-id".into());
        }
        if !ids.insert(id) {
            return Err("schema-invalid: duplicate-id".into());
        }
    }
    if action.production_schedule.len() > MAX_PRODUCTION_LINES {
        return Err("schema-invalid: production-line-limit".into());
    }
    if action.delivery_schedule.len() > MAX_DELIVERY_LINES {
        return Err("schema-invalid: delivery-line-limit".into());
    }
    let mut supplier_rfqs = BTreeSet::new();
    for rfq in &action.supplier_rfqs {
        if !supplier_rfqs.insert((&rfq.supplier_id, &rfq.component_id, &rfq.rfq_id)) {
            return Err("schema-invalid: duplicate-supplier-rfq-id".into());
        }
    }
    Ok(())
}

/// The engine's command and line id pattern `[a-z0-9][a-z0-9._-]{0,31}`,
/// written identically to its private `valid_id`.
fn valid_engine_id(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && value.bytes().skip(1).all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// Actions that break one structural rule each, sent by the fixture agent
/// (conformance tests). `None` for any other mode.
fn fixture_structural_violation(
    mode: &str,
    agent_id: &str,
    observation_id: &str,
    day: u32,
) -> Option<GameActionSet> {
    let mut action = GameActionSet::empty(agent_id, observation_id, day, 1);
    let rfq = |command_id: &str, rfq_id: &str, supplier_id: &str| SupplierRfqCommand {
        command_id: command_id.into(),
        rfq_id: rfq_id.into(),
        supplier_id: supplier_id.into(),
        component_id: "board-imd".into(),
        quantity: 10,
        due_day: day + 5,
        max_unit_price: None,
    };
    let production = |index: usize| ProductionLine {
        line_id: format!("pl-{index}"),
        priority: 0,
        product_id: "p01".into(),
        quantity: 1,
    };
    let delivery = |index: usize| DeliveryLine {
        line_id: format!("dl-{index}"),
        priority: 0,
        customer_order_id: "no-such-order".into(),
        quantity: 1,
    };
    match mode {
        "invalid-id" => action.production_schedule.push(ProductionLine {
            line_id: "Line-1".into(),
            ..production(0)
        }),
        "shared-id" => {
            action.supplier_rfqs.push(rfq("x-1", "r-1", "basus"));
            action.production_schedule.push(ProductionLine {
                line_id: "x-1".into(),
                ..production(0)
            });
        }
        "production-lines" => {
            action.production_schedule = (0..=MAX_PRODUCTION_LINES).map(production).collect();
        }
        "delivery-lines" => {
            action.delivery_schedule = (0..=MAX_DELIVERY_LINES).map(delivery).collect();
        }
        "supplier-rfq-reuse" => {
            action.supplier_rfqs.push(rfq("s-1", "r-1", "basus"));
            action.supplier_rfqs.push(rfq("s-2", "r-1", "basus"));
        }
        // The largest structurally valid action of the same kinds.
        "structural-limits" => {
            action.production_schedule = (0..MAX_PRODUCTION_LINES).map(production).collect();
            action.delivery_schedule = (0..MAX_DELIVERY_LINES).map(delivery).collect();
            action.supplier_rfqs.push(rfq("s-1", "r-1", "basus"));
            action.supplier_rfqs.push(rfq("s-2", "r-1", "macrostar"));
            // Same supplier and rfq_id, another component: accepted.
            action.supplier_rfqs.push(SupplierRfqCommand {
                component_id: "board-pintel".into(),
                ..rfq("s-3", "r-1", "basus")
            });
        }
        _ => return None,
    }
    Some(action)
}

type Map = Vec<(String, Value)>;

fn as_map(value: &Value) -> Result<&Map, String> {
    if let Value::Map(map) = value {
        Ok(map)
    } else {
        Err("schema-invalid: expected map".into())
    }
}
fn get<'a>(map: &'a Map, key: &str) -> Result<&'a Value, String> {
    map.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
        .ok_or_else(|| format!("schema-invalid: missing {key}"))
}
fn get_text<'a>(map: &'a Map, key: &str) -> Result<&'a str, String> {
    if let Value::Text(value) = get(map, key)? {
        Ok(value)
    } else {
        Err(format!("schema-invalid: {key}"))
    }
}
fn expect_text(map: &Map, key: &str, expected: &str) -> Result<(), String> {
    if get_text(map, key)? == expected {
        Ok(())
    } else {
        Err(format!("wrong-{key}"))
    }
}
fn get_u64(map: &Map, key: &str) -> Result<u64, String> {
    match get(map, key)? {
        Value::U64(value) => Ok(*value),
        Value::I64(value) if *value >= 0 => Ok(*value as u64),
        _ => Err(format!("schema-invalid: {key}")),
    }
}
fn get_u32(map: &Map, key: &str) -> Result<u32, String> {
    u32::try_from(get_u64(map, key)?).map_err(|_| format!("out-of-range: {key}"))
}
fn get_i64(map: &Map, key: &str) -> Result<i64, String> {
    match get(map, key)? {
        Value::I64(value) => Ok(*value),
        Value::U64(value) => i64::try_from(*value).map_err(|_| format!("out-of-range: {key}")),
        _ => Err(format!("schema-invalid: {key}")),
    }
}
fn get_optional_i64(map: &Map, key: &str) -> Result<Option<i64>, String> {
    match get(map, key)? {
        Value::Null => Ok(None),
        _ => get_i64(map, key).map(Some),
    }
}
fn get_optional_bool(map: &Map, key: &str) -> Result<Option<bool>, String> {
    match map.iter().find(|(name, _)| name == key) {
        None => Ok(None),
        Some((_, Value::Bool(value))) => Ok(Some(*value)),
        Some(_) => Err(format!("schema-invalid: {key}")),
    }
}
fn get_bytes<'a>(map: &'a Map, key: &str) -> Result<&'a [u8], String> {
    if let Value::Bytes(value) = get(map, key)? {
        Ok(value)
    } else {
        Err(format!("schema-invalid: {key}"))
    }
}
fn get_array<'a>(map: &'a Map, key: &str) -> Result<&'a [Value], String> {
    if let Value::Array(value) = get(map, key)? {
        Ok(value)
    } else {
        Err(format!("schema-invalid: {key}"))
    }
}
fn closed_keys(map: &Map, required: &[&str]) -> Result<(), String> {
    if map.len() != required.len() {
        return Err("schema-invalid: unknown-or-missing-field".into());
    }
    let actual: BTreeSet<_> = map.iter().map(|(key, _)| key.as_str()).collect();
    let expected: BTreeSet<_> = required.iter().copied().collect();
    if actual == expected {
        Ok(())
    } else {
        Err("schema-invalid: unknown-or-missing-field".into())
    }
}

fn closed_keys_with_optional(
    map: &Map,
    required: &[&str],
    optional: &[&str],
) -> Result<(), String> {
    let actual: BTreeSet<_> = map.iter().map(|(key, _)| key.as_str()).collect();
    let required: BTreeSet<_> = required.iter().copied().collect();
    let allowed: BTreeSet<_> = required
        .iter()
        .copied()
        .chain(optional.iter().copied())
        .collect();
    if required.is_subset(&actual) && actual.is_subset(&allowed) {
        Ok(())
    } else {
        Err("schema-invalid: unknown-or-missing-field".into())
    }
}

fn decode_canonical(bytes: &[u8]) -> Result<Value, String> {
    let mut decoder = Decoder { bytes, offset: 0 };
    let value = decoder.value(0)?;
    if decoder.offset != bytes.len() || value.encode() != bytes {
        return Err("schema-invalid: non-canonical-cbor".into());
    }
    Ok(value)
}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Decoder<'_> {
    fn byte(&mut self) -> Result<u8, String> {
        let value = *self
            .bytes
            .get(self.offset)
            .ok_or_else(|| "schema-invalid: truncated-cbor".to_owned())?;
        self.offset += 1;
        Ok(value)
    }
    fn take(&mut self, length: usize) -> Result<&[u8], String> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| "schema-invalid: cbor-length".to_owned())?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| "schema-invalid: truncated-cbor".to_owned())?;
        self.offset = end;
        Ok(value)
    }
    fn argument(&mut self, add: u8) -> Result<u64, String> {
        match add {
            value @ 0..=23 => Ok(u64::from(value)),
            24 => Ok(u64::from(self.byte()?)),
            25 => Ok(u64::from(u16::from_be_bytes(
                self.take(2)?.try_into().unwrap(),
            ))),
            26 => Ok(u64::from(u32::from_be_bytes(
                self.take(4)?.try_into().unwrap(),
            ))),
            27 => Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap())),
            _ => Err("schema-invalid: indefinite-or-reserved-cbor".into()),
        }
    }
    fn value(&mut self, depth: u8) -> Result<Value, String> {
        if depth > 48 {
            return Err("schema-invalid: cbor-depth".into());
        }
        let initial = self.byte()?;
        let major = initial >> 5;
        let add = initial & 31;
        match major {
            0 => Ok(Value::U64(self.argument(add)?)),
            1 => {
                let value = self.argument(add)?;
                let signed = -1_i128 - i128::from(value);
                Ok(Value::I64(
                    i64::try_from(signed).map_err(|_| "integer-range")?,
                ))
            }
            2 => {
                let length = usize::try_from(self.argument(add)?).map_err(|_| "cbor-length")?;
                Ok(Value::Bytes(self.take(length)?.to_vec()))
            }
            3 => {
                let length = usize::try_from(self.argument(add)?).map_err(|_| "cbor-length")?;
                let text =
                    std::str::from_utf8(self.take(length)?).map_err(|_| "schema-invalid: utf8")?;
                Ok(Value::Text(text.into()))
            }
            4 => {
                let length = usize::try_from(self.argument(add)?).map_err(|_| "cbor-length")?;
                let mut values = Vec::with_capacity(length.min(4096));
                for _ in 0..length {
                    values.push(self.value(depth + 1)?);
                }
                Ok(Value::Array(values))
            }
            5 => {
                let length = usize::try_from(self.argument(add)?).map_err(|_| "cbor-length")?;
                let mut entries = Vec::with_capacity(length.min(4096));
                let mut seen = BTreeSet::new();
                for _ in 0..length {
                    let Value::Text(key) = self.value(depth + 1)? else {
                        return Err("schema-invalid: non-text-map-key".into());
                    };
                    if !seen.insert(key.clone()) {
                        return Err("schema-invalid: duplicate-map-key".into());
                    }
                    entries.push((key, self.value(depth + 1)?));
                }
                Ok(Value::Map(entries))
            }
            7 if add == 20 => Ok(Value::Bool(false)),
            7 if add == 21 => Ok(Value::Bool(true)),
            7 if add == 22 => Ok(Value::Null),
            _ => Err("schema-invalid: unsupported-cbor".into()),
        }
    }
}

/// Entry point for the bundled protocol fixture agent used by conformance
/// tests. It is not a benchmark entrant.
pub fn run_fixture_agent(mode: &str, delay_ms: u64) -> Result<(), RunnerError> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut last_engine_sequence = None;
    let mut last_runner_sequence = None;
    loop {
        let raw = match read_frame(&mut input, MAX_FRAME_BYTES) {
            Ok(Some(raw)) => raw,
            Ok(None) => return Ok(()),
            Err(error) => return Err(error),
        };
        if mode == "exit" {
            return Ok(());
        }
        if mode == "timeout" {
            thread::sleep(Duration::from_secs(60));
            continue;
        }
        if delay_ms > 0 && mode != "recover" {
            thread::sleep(Duration::from_millis(delay_ms));
        }
        if mode == "malformed" {
            write_frame(&mut output, &[0xff])?;
            continue;
        }
        if mode == "oversize" {
            output.write_all(&u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes())?;
            output.flush()?;
            continue;
        }
        let request = decode_canonical(&raw).map_err(RunnerError::Protocol)?;
        let request = as_map(&request).map_err(RunnerError::Protocol)?;
        let sender_role = get_text(request, "sender_role").map_err(RunnerError::Protocol)?;
        if mode == "sequence-check" && sender_role == "engine" {
            let sequence = get_u64(request, "sender_sequence").map_err(RunnerError::Protocol)?;
            let message_id = get_text(request, "message_id").map_err(RunnerError::Protocol)?;
            let recipient = get_text(request, "recipient").map_err(RunnerError::Protocol)?;
            let slot = recipient
                .strip_prefix("agent-")
                .and_then(|suffix| suffix.as_bytes().first().copied())
                .and_then(|letter| letter.checked_sub(b'a'))
                .map(u64::from)
                .filter(|slot| *slot < 6)
                .ok_or_else(|| RunnerError::Protocol("engine recipient identity differs".into()))?;
            let expected = last_engine_sequence.map_or(slot, |previous| previous + 6);
            if message_id != format!("message/engine/{sequence}") || sequence != expected {
                return Err(RunnerError::Protocol(
                    "engine message identity or sequence is not monotone".into(),
                ));
            }
            last_engine_sequence = Some(sequence);
        } else if mode == "sequence-check" && sender_role == "runner" {
            let sequence = get_u64(request, "sender_sequence").map_err(RunnerError::Protocol)?;
            let recipient = get_text(request, "recipient").map_err(RunnerError::Protocol)?;
            let slot = recipient
                .strip_prefix("agent-")
                .and_then(|suffix| suffix.as_bytes().first().copied())
                .and_then(|letter| letter.checked_sub(b'a'))
                .map(u64::from)
                .filter(|slot| *slot < 6)
                .ok_or_else(|| RunnerError::Protocol("runner recipient identity differs".into()))?;
            if last_runner_sequence.map_or(sequence != slot, |previous| sequence <= previous) {
                return Err(RunnerError::Protocol(
                    "runner sender sequence is not monotone".into(),
                ));
            }
            last_runner_sequence = Some(sequence);
        }
        let game_id = get_text(request, "game_id")
            .map_err(RunnerError::Protocol)?
            .to_owned();
        let agent_id = get_text(request, "recipient")
            .map_err(RunnerError::Protocol)?
            .to_owned();
        let payload = as_map(get(request, "payload").map_err(RunnerError::Protocol)?)
            .map_err(RunnerError::Protocol)?;
        let payload_type = get_text(payload, "type").map_err(RunnerError::Protocol)?;
        if payload_type == "game-finished" {
            return Ok(());
        }
        if payload_type != "day-observation" {
            continue;
        }
        let day = get_u32(payload, "day").map_err(RunnerError::Protocol)?;
        if mode == "recover" && day == 0 && delay_ms > 0 {
            thread::sleep(Duration::from_millis(delay_ms));
        }
        let observation_id = get_text(payload, "observation_id")
            .map_err(RunnerError::Protocol)?
            .to_owned();
        let response_agent = if mode == "wrong-identity" {
            "wrong-agent"
        } else {
            &agent_id
        };
        let action = fixture_structural_violation(mode, response_agent, &observation_id, day)
            .unwrap_or_else(|| GameActionSet::empty(response_agent, &observation_id, day, 1))
            .value();
        let message_id = format!("message/{agent_id}/{day}");
        let mut response = envelope(
            &game_id,
            &message_id,
            "manufacturer",
            response_agent,
            "runner",
            Some(day),
            "action-window",
            u64::from(day),
            action,
        );
        if mode == "old-schema" {
            let Value::Map(entries) = &mut response else {
                unreachable!("envelope is a map")
            };
            let schema_id = entries
                .iter_mut()
                .find_map(|(key, value)| (key == "schema_id").then_some(value))
                .expect("envelope has schema id");
            *schema_id = Value::Text("tac-scm-message/1".into());
            let version = entries
                .iter_mut()
                .find_map(|(key, value)| (key == "schema_version").then_some(value))
                .expect("envelope has schema version");
            *version = Value::Array(vec![Value::U64(1), Value::U64(0), Value::U64(0)]);
        }
        if mode != "duplicate" {
            let Value::Map(entries) = &mut response else {
                unreachable!("envelope is a map")
            };
            entries.push(("final_submission".into(), Value::Bool(true)));
        }
        write_frame(&mut output, &response.encode())?;
        if mode == "duplicate" {
            let conflicting = envelope(
                &game_id,
                &message_id,
                "manufacturer",
                response_agent,
                "runner",
                Some(day),
                "action-window",
                u64::from(day) + 1,
                GameActionSet::empty(response_agent, &observation_id, day, 2).value(),
            )
            .encode();
            write_frame(&mut output, &conflicting)?;
        }
    }
}

fn read_frame(reader: &mut impl Read, limit: usize) -> Result<Option<Vec<u8>>, RunnerError> {
    let mut prefix = [0_u8; 4];
    let mut first = [0_u8; 1];
    match reader.read(&mut first) {
        Ok(0) => return Ok(None),
        Ok(1) => prefix[0] = first[0],
        Ok(_) => unreachable!(),
        Err(error) => return Err(error.into()),
    }
    reader.read_exact(&mut prefix[1..])?;
    let length = usize::try_from(u32::from_be_bytes(prefix)).unwrap();
    if length > limit {
        return Err(RunnerError::Limit("frame"));
    }
    let mut bytes = vec![0_u8; length];
    reader.read_exact(&mut bytes)?;
    Ok(Some(bytes))
}

fn write_frame(writer: &mut impl Write, bytes: &[u8]) -> Result<(), RunnerError> {
    let length = u32::try_from(bytes.len()).map_err(|_| RunnerError::Limit("frame"))?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(bytes)?;
    writer.flush()?;
    Ok(())
}

/// Runs the intentionally small CLI. Batch plans are expanded before any game
/// starts and never depend on preceding outcomes.
pub fn cli(arguments: &[String]) -> Result<(), RunnerError> {
    match arguments.first().map(String::as_str) {
        Some("fixture-agent") => {
            let mode = arguments.get(1).map_or("normal", String::as_str);
            let delay = arguments
                .get(2)
                .map_or(Ok(0), |value| value.parse::<u64>())
                .map_err(|_| RunnerError::Plan("delay"))?;
            run_fixture_agent(mode, delay)
        }
        Some("identity") => {
            print!("{}", identity_document()?);
            Ok(())
        }
        Some("serve") => run_cli_remote(arguments),
        Some("one") => run_cli_games(arguments, 1),
        Some("planned") => run_cli_planned(arguments),
        Some("planned-research") => run_cli_planned_research(arguments),
        Some("batch") => {
            let count = arguments
                .get(2)
                .ok_or(RunnerError::Plan("batch count"))?
                .parse::<u64>()
                .map_err(|_| RunnerError::Plan("batch count"))?;
            run_cli_games(arguments, count)
        }
        _ => Err(RunnerError::Plan(
            "usage: tac-scm-game-runner identity | serve BIND OUTPUT ROOT_SEED_HEX | one OUTPUT AGENT... | batch OUTPUT COUNT AGENT... | planned OUTPUT ROOT_SEED_HEX CAMPAIGN_ID CAMPAIGN_SHA256 BLOCK CONTEXT ROTATION ORDINAL AGENT... | planned-research OUTPUT ROOT_SEED_HEX PROFILE_ID CAMPAIGN_ID CAMPAIGN_SHA256 BLOCK CONTEXT ROTATION ORDINAL AGENT...",
        )),
    }
}

fn run_cli_remote(arguments: &[String]) -> Result<(), RunnerError> {
    let bind = arguments
        .get(1)
        .ok_or(RunnerError::Plan("remote bind"))?
        .parse()
        .map_err(|_| RunnerError::Plan("remote bind"))?;
    let output = PathBuf::from(arguments.get(2).ok_or(RunnerError::Plan("remote output"))?);
    let root_seed = parse_hex_digest(
        arguments
            .get(3)
            .ok_or(RunnerError::Plan("remote root seed"))?,
    )?;
    if arguments.len() != 4 {
        return Err(RunnerError::Plan(
            "serve requires BIND OUTPUT ROOT_SEED_HEX",
        ));
    }
    let token_value = std::env::var("TAC_SCM_REMOTE_AGENT_TOKENS")
        .map_err(|_| RunnerError::Plan("remote agent token environment"))?;
    let tokens: Vec<_> = token_value.split(',').map(str::to_owned).collect();
    let agent_tokens: [String; 6] = tokens
        .try_into()
        .map_err(|_| RunnerError::Plan("exactly six remote agent tokens required"))?;
    let spectator_token = std::env::var("TAC_SCM_REMOTE_SPECTATOR_TOKEN")
        .map_err(|_| RunnerError::Plan("remote spectator token environment"))?;
    let mut config = remote::RemoteServerConfig::loopback(
        bind,
        output,
        engine_plan_from_environment(root_seed)?,
        agent_tokens,
        spectator_token,
    );
    config.policy = operational_policy_from_environment()?;
    config.tls_terminated = std::env::var("TAC_SCM_TLS_TERMINATED").as_deref() == Ok("1");
    if let Some(value) = std::env::var_os("TAC_SCM_REGISTRATION_TIMEOUT_MS") {
        config.registration_timeout = Duration::from_millis(
            value
                .to_str()
                .ok_or(RunnerError::Plan("registration timeout environment"))?
                .parse()
                .map_err(|_| RunnerError::Plan("registration timeout environment"))?,
        );
    }
    let server = remote::RemoteMatchServer::bind(config)?;
    println!(
        "{{\"status\":\"listening\",\"session_schema\":\"{}\",\"address\":\"{}\"}}",
        remote::SESSION_SCHEMA_ID,
        server.local_addr()
    );
    io::stdout().flush()?;
    let report = server.run()?;
    println!(
        "{{\"status\":\"complete\",\"game_definition_id\":\"{}\",\"game_result_id\":\"{}\",\"event_delivery\":\"{}\",\"spectator_queue_peak_bytes\":{}}}",
        report.plan.game_definition_id,
        report.output.game_result_id,
        report.output.event_delivery,
        report.spectator_queue_peak_bytes,
    );
    Ok(())
}

fn run_cli_planned(arguments: &[String]) -> Result<(), RunnerError> {
    let output = PathBuf::from(arguments.get(1).ok_or(RunnerError::Plan("output"))?);
    let root_seed = parse_hex_digest(arguments.get(2).ok_or(RunnerError::Plan("root seed"))?)?;
    let campaign_id = arguments.get(3).ok_or(RunnerError::Plan("campaign id"))?;
    let campaign_manifest_sha256 = parse_hex_digest(
        arguments
            .get(4)
            .ok_or(RunnerError::Plan("campaign manifest"))?,
    )?;
    let block_index = parse_cli_number(arguments.get(5), "block index")?;
    let context_id = arguments.get(6).ok_or(RunnerError::Plan("context id"))?;
    let seat_rotation = u32::try_from(parse_cli_number(arguments.get(7), "seat rotation")?)
        .map_err(|_| RunnerError::Plan("seat rotation"))?;
    if seat_rotation >= 6 {
        return Err(RunnerError::Plan("seat rotation"));
    }
    let schedule_ordinal = parse_cli_number(arguments.get(8), "schedule ordinal")?;
    let paths = arguments.get(9..).ok_or(RunnerError::Plan("agents"))?;
    if paths.len() != 6 {
        return Err(RunnerError::Plan("exactly six agent executables required"));
    }
    let agents = inspect_campaign_agent_specs(paths, campaign_id, block_index)?;
    let plan = FrozenGamePlan::freeze_scheduled(
        engine_plan_from_environment(root_seed)?,
        agents,
        operational_policy_from_environment()?,
        campaign_id,
        campaign_manifest_sha256,
        schedule_ordinal,
        block_index,
        context_id,
        seat_rotation,
    )?;
    if output.join("identities.txt").is_file() {
        return Ok(());
    }
    let evidence = evidence_policy_from_environment()?;
    let result = run_game_with_evidence(&plan, evidence)?;
    if evidence == EvidencePolicy::FULL {
        result.write_to(&plan, &output)
    } else {
        result.write_summary_to(&plan, &output)
    }
}

fn run_cli_planned_research(arguments: &[String]) -> Result<(), RunnerError> {
    let output = PathBuf::from(arguments.get(1).ok_or(RunnerError::Plan("output"))?);
    let root_seed = parse_hex_digest(arguments.get(2).ok_or(RunnerError::Plan("root seed"))?)?;
    let profile = HistoricalProfile::from_id(
        arguments
            .get(3)
            .ok_or(RunnerError::Plan("research profile id"))?,
    )
    .ok_or(RunnerError::Plan("research profile id"))?;
    let campaign_id = arguments.get(4).ok_or(RunnerError::Plan("campaign id"))?;
    let campaign_manifest_sha256 = parse_hex_digest(
        arguments
            .get(5)
            .ok_or(RunnerError::Plan("campaign manifest"))?,
    )?;
    let block_index = parse_cli_number(arguments.get(6), "block index")?;
    let context_id = arguments.get(7).ok_or(RunnerError::Plan("context id"))?;
    let seat_rotation = u32::try_from(parse_cli_number(arguments.get(8), "seat rotation")?)
        .map_err(|_| RunnerError::Plan("seat rotation"))?;
    if seat_rotation >= 6 {
        return Err(RunnerError::Plan("seat rotation"));
    }
    let schedule_ordinal = parse_cli_number(arguments.get(9), "schedule ordinal")?;
    let paths = arguments.get(10..).ok_or(RunnerError::Plan("agents"))?;
    if paths.len() != 6 {
        return Err(RunnerError::Plan("exactly six agent executables required"));
    }
    let agents = inspect_campaign_agent_specs(paths, campaign_id, block_index)?;
    let plan = FrozenGamePlan::freeze_scheduled(
        GamePlan::materialize_research(root_seed, profile)?,
        agents,
        operational_policy_from_environment()?,
        campaign_id,
        campaign_manifest_sha256,
        schedule_ordinal,
        block_index,
        context_id,
        seat_rotation,
    )?;
    if output.join("identities.txt").is_file() {
        return Ok(());
    }
    let evidence = evidence_policy_from_environment()?;
    let result = run_game_with_evidence(&plan, evidence)?;
    if evidence == EvidencePolicy::FULL {
        result.write_to(&plan, &output)
    } else {
        result.write_summary_to(&plan, &output)
    }
}

fn parse_cli_number(value: Option<&String>, field: &'static str) -> Result<u64, RunnerError> {
    value
        .ok_or(RunnerError::Plan(field))?
        .parse()
        .map_err(|_| RunnerError::Plan(field))
}

fn parse_hex_digest(value: &str) -> Result<[u8; 32], RunnerError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(RunnerError::Plan("hex digest"));
    }
    let mut digest = [0_u8; 32];
    for (index, output) in digest.iter_mut().enumerate() {
        let start = index * 2;
        *output = u8::from_str_radix(&value[start..start + 2], 16)
            .map_err(|_| RunnerError::Plan("hex digest"))?;
    }
    Ok(digest)
}

fn inspect_agent_paths(paths: &[String]) -> Result<[AgentArtifact; 6], RunnerError> {
    let mut inspected = Vec::new();
    for (slot, path) in paths.iter().enumerate() {
        inspected.push(AgentArtifact::inspect(
            format!("agent-{}", char::from(b'a' + u8::try_from(slot).unwrap())),
            path,
            Vec::new(),
        )?);
    }
    Ok(inspected.try_into().unwrap())
}

fn inspect_campaign_agent_specs(
    specs: &[String],
    campaign_id: &str,
    block_index: u64,
) -> Result<[AgentArtifact; 6], RunnerError> {
    let mut inspected = Vec::new();
    for (slot, spec) in specs.iter().enumerate() {
        let (instance_id, path) = spec
            .split_once('=')
            .ok_or(RunnerError::Plan("campaign agent INSTANCE=PATH"))?;
        if instance_id.is_empty() || path.is_empty() {
            return Err(RunnerError::Plan("campaign agent INSTANCE=PATH"));
        }
        let mut artifact = AgentArtifact::inspect(
            format!("agent-{}", char::from(b'a' + u8::try_from(slot).unwrap())),
            path,
            Vec::new(),
        )?;
        artifact.arguments = vec![
            "--campaign-id".into(),
            campaign_id.into(),
            "--artifact-digest".into(),
            hex(&artifact.sha256),
            "--instance-id".into(),
            instance_id.into(),
            "--block-index".into(),
            block_index.to_string(),
        ];
        inspected.push(artifact);
    }
    Ok(inspected.try_into().unwrap())
}

fn operational_policy_from_environment() -> Result<OperationalPolicy, RunnerError> {
    let mut policy = OperationalPolicy::default();
    if let Some(value) = std::env::var_os("TAC_SCM_ACTION_DEADLINE_MS") {
        policy.action_deadline = Duration::from_millis(
            value
                .to_str()
                .ok_or(RunnerError::Plan("action deadline environment"))?
                .parse()
                .map_err(|_| RunnerError::Plan("action deadline environment"))?,
        );
    }
    if let Some(value) = std::env::var_os("TAC_SCM_STARTUP_GRACE_MS") {
        policy.startup_grace = Duration::from_millis(
            value
                .to_str()
                .ok_or(RunnerError::Plan("startup grace environment"))?
                .parse()
                .map_err(|_| RunnerError::Plan("startup grace environment"))?,
        );
    }
    if let Some(value) = std::env::var_os("TAC_SCM_SHUTDOWN_GRACE_MS") {
        let milliseconds: u64 = value
            .to_str()
            .ok_or(RunnerError::Plan("shutdown grace environment"))?
            .parse()
            .map_err(|_| RunnerError::Plan("shutdown grace environment"))?;
        if !(1..=15_000).contains(&milliseconds) {
            return Err(RunnerError::Plan("shutdown grace environment"));
        }
        policy.shutdown_grace = Duration::from_millis(milliseconds);
    }
    Ok(policy)
}

fn evidence_policy_from_environment() -> Result<EvidencePolicy, RunnerError> {
    let evidence = std::env::var("TAC_SCM_EVIDENCE");
    if evidence.is_err() && std::env::var("TAC_SCM_RETENTION").as_deref() == Ok("full") {
        return Err(RunnerError::Plan(
            "deprecated full retention cannot enable evidence implicitly",
        ));
    }
    match evidence.as_deref() {
        Ok("full") => Ok(EvidencePolicy::FULL),
        Ok("thin") | Err(std::env::VarError::NotPresent) => Ok(EvidencePolicy::default()),
        _ => Err(RunnerError::Plan("evidence environment")),
    }
}

fn run_cli_games(arguments: &[String], count: u64) -> Result<(), RunnerError> {
    let output = PathBuf::from(arguments.get(1).ok_or(RunnerError::Plan("output"))?);
    let agent_start = if arguments.first().is_some_and(|value| value == "batch") {
        3
    } else {
        2
    };
    let paths = arguments
        .get(agent_start..)
        .ok_or(RunnerError::Plan("agents"))?;
    if paths.len() != 6 {
        return Err(RunnerError::Plan("exactly six agent executables required"));
    }
    let agents = inspect_agent_paths(paths)?;
    let policy = operational_policy_from_environment()?;
    let evidence = evidence_policy_from_environment()?;
    let seeds: Vec<_> = (0..count)
        .map(|ordinal| sha256(&ordinal.to_be_bytes()))
        .collect();
    let plans = seeds
        .into_iter()
        .enumerate()
        .map(|(ordinal, seed)| {
            FrozenGamePlan::freeze(
                engine_plan_from_environment(seed)?,
                agents.clone(),
                policy.clone(),
                "manual-batch-v2",
                u64::try_from(ordinal).unwrap(),
            )
        })
        .collect::<Result<Vec<_>, RunnerError>>()?;
    for (ordinal, plan) in plans.into_iter().enumerate() {
        let game_output = output.join(format!("game-{ordinal:06}"));
        if game_output.join("identities.txt").is_file() {
            continue;
        }
        let result = run_game_with_evidence(&plan, evidence)?;
        if evidence == EvidencePolicy::FULL {
            result.write_to(&plan, &game_output)?;
        } else {
            result.write_summary_to(&plan, &game_output)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::path::PathBuf;

    use super::{
        AgentArtifact, FrozenGamePlan, OperationalPolicy, bootstrap_message,
        check_action_structure, decode_canonical, fixture_structural_violation, receipt_ordinal,
        runner_identity_from, semantic_configuration, valid_engine_id,
    };
    use tac_scm_engine_core::cbor::Value;
    use tac_scm_engine_core::game::{
        ActionInput, DefaultCause, GameActionSet, GameEngine, GamePlan,
    };
    use tac_scm_engine_core::historical::HistoricalProfile;

    #[test]
    fn decoder_accepts_only_canonical_closed_values() {
        let value = Value::Map(vec![
            ("day".into(), Value::U64(4)),
            ("agent".into(), Value::Text("agent-a".into())),
        ]);
        assert_eq!(decode_canonical(&value.encode()).unwrap(), value);
        assert!(decode_canonical(&[0x18, 0x01]).is_err());
        assert!(decode_canonical(&[0xbf, 0xff]).is_err());
    }

    /// Commits `action` for slot 0 (the other five default) on day 0 of a
    /// fresh engine and reports whether the engine failed the day.
    fn engine_fails_day_zero(build: impl Fn(&str, &str, u32) -> GameActionSet) -> bool {
        let mut engine = GameEngine::new(GamePlan::materialize([0x42; 32]).unwrap()).unwrap();
        let observations = engine.open_day().unwrap().to_vec();
        let first = &observations[0];
        let mut inputs = vec![ActionInput::Accepted(build(
            &first.agent_id,
            &first.id,
            first.day,
        ))];
        inputs.extend(
            observations[1..]
                .iter()
                .map(|observation| ActionInput::Default {
                    agent_id: observation.agent_id.clone(),
                    cause: DefaultCause::Timeout,
                }),
        );
        engine.commit_day(inputs).is_err()
    }

    #[test]
    fn runner_structural_checks_reject_exactly_the_engines_fatal_actions() {
        for (mode, fatal) in [
            ("invalid-id", true),
            ("shared-id", true),
            ("production-lines", true),
            ("delivery-lines", true),
            ("supplier-rfq-reuse", true),
            ("structural-limits", false),
        ] {
            let build = |agent: &str, observation: &str, day: u32| {
                fixture_structural_violation(mode, agent, observation, day).unwrap()
            };
            let probe = build("agent-a", "observation", 0);
            assert_eq!(
                check_action_structure(&probe).is_err(),
                fatal,
                "runner: {mode}"
            );
            assert_eq!(engine_fails_day_zero(build), fatal, "engine: {mode}");
        }
    }

    #[test]
    fn supplier_rfq_reuse_check_is_stricter_only_for_ineligible_same_line_duplicates() {
        let reuse = |second_component: &'static str, second_quantity: u32| {
            move |agent: &str, observation: &str, day: u32| {
                let mut action =
                    fixture_structural_violation("supplier-rfq-reuse", agent, observation, day)
                        .unwrap();
                action.supplier_rfqs[1].component_id = second_component.into();
                action.supplier_rfqs[1].quantity = second_quantity;
                action
            }
        };
        // Another component of the same supplier: accepted by both (the
        // engine merges the two into one alternative group).
        let other_component = reuse("board-pintel", 10);
        assert!(check_action_structure(&other_component("agent-a", "observation", 0)).is_ok());
        assert!(!engine_fails_day_zero(other_component));
        // Same line, one of the two ineligible (a probe): the engine would
        // not abort, but eligibility depends on the posted price in general,
        // so the runner rejects every same-line duplicate.
        let probe = reuse("board-imd", 0);
        assert!(check_action_structure(&probe("agent-a", "observation", 0)).is_err());
        assert!(!engine_fails_day_zero(probe));
    }

    #[test]
    fn engine_id_pattern_matches_its_boundaries() {
        assert!(valid_engine_id("a"));
        assert!(valid_engine_id("0a._-"));
        assert!(valid_engine_id(&"a".repeat(32)));
        assert!(!valid_engine_id(&"a".repeat(33)));
        assert!(!valid_engine_id(""));
        assert!(!valid_engine_id(".a"));
        assert!(!valid_engine_id("-a"));
        assert!(!valid_engine_id("aB"));
        assert!(!valid_engine_id("a/b"));
        assert!(!valid_engine_id("a\u{e9}"));
    }

    #[test]
    fn receipt_ordinals_are_timing_independent() {
        assert_eq!(receipt_ordinal(3, 2, 1), 341);
        assert!(receipt_ordinal(3, 2, 2) < receipt_ordinal(3, 3, 1));
    }

    #[test]
    fn runner_identity_fails_closed_when_executable_bytes_are_unavailable() {
        assert!(runner_identity_from(Path::new("missing-runner-artifact")).is_err());
    }

    #[test]
    fn successor_plan_and_bootstrap_name_successor_contracts() {
        let engine_plan = GamePlan::materialize([0x24; 32]).unwrap();
        let configuration = semantic_configuration(&engine_plan).encode();
        assert!(
            configuration
                .windows(b"component-contracts-draft2".len())
                .any(|value| { value == b"component-contracts-draft2" })
        );
        assert!(
            !configuration
                .windows(b"component-contracts-draft1".len())
                .any(|value| { value == b"component-contracts-draft1" })
        );

        let agents = std::array::from_fn(|slot| AgentArtifact {
            agent_id: engine_plan.participants[slot].clone(),
            executable: PathBuf::from("unused-test-artifact"),
            arguments: Vec::new(),
            sha256: [u8::try_from(slot).unwrap(); 32],
            byte_length: 0,
        });
        let frozen = FrozenGamePlan::freeze(
            engine_plan,
            agents,
            OperationalPolicy::default(),
            "successor-contract-test",
            0,
        )
        .unwrap();
        assert_eq!(
            frozen.policy.shutdown_grace,
            std::time::Duration::from_millis(100)
        );
        assert!(
            frozen
                .canonical_bytes
                .windows(b"shutdown_grace_ms".len())
                .any(|value| value == b"shutdown_grace_ms")
        );
        assert!(
            frozen
                .canonical_bytes
                .windows(b"close-stdin-bounded-grace-then-kill/v2".len())
                .any(|value| value == b"close-stdin-bounded-grace-then-kill/v2")
        );
        let bootstrap = bootstrap_message(&frozen, 0).encode();
        assert!(
            bootstrap
                .windows(b"contracts-draft2".len())
                .any(|value| { value == b"contracts-draft2" })
        );
        assert!(
            !bootstrap
                .windows(b"contracts-draft1".len())
                .any(|value| { value == b"contracts-draft1" })
        );
    }

    #[test]
    fn research_plan_and_bootstrap_bind_the_explicit_historical_profile() {
        let engine_plan =
            GamePlan::materialize_research([0x25; 32], HistoricalProfile::FinancialCarry).unwrap();
        let configuration = semantic_configuration(&engine_plan).encode();
        assert!(
            configuration
                .windows(b"whole-credit-math-round-with-directed-decimal-storage/v1".len())
                .any(|value| {
                    value == b"whole-credit-math-round-with-directed-decimal-storage/v1"
                })
        );

        let agents = std::array::from_fn(|slot| AgentArtifact {
            agent_id: engine_plan.participants[slot].clone(),
            executable: PathBuf::from("unused-test-artifact"),
            arguments: Vec::new(),
            sha256: [u8::try_from(slot).unwrap(); 32],
            byte_length: 0,
        });
        let frozen = FrozenGamePlan::freeze(
            engine_plan,
            agents,
            OperationalPolicy::default(),
            "historical-profile-test",
            0,
        )
        .unwrap();
        let bootstrap = bootstrap_message(&frozen, 0).encode();
        assert!(
            bootstrap
                .windows(HistoricalProfile::FinancialCarry.id().len())
                .any(|value| value == HistoricalProfile::FinancialCarry.id().as_bytes())
        );
    }
}
