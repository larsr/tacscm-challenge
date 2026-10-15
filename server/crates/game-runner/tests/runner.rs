use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use tac_scm_engine_core::AGENT_IDS;
use tac_scm_engine_core::conformance::validate_event_log;
use tac_scm_engine_core::crypto::{hex, sha256};
use tac_scm_engine_core::game::GamePlan;
use tac_scm_game_runner::{
    AgentArtifact, EvidencePolicy, FrozenGamePlan, OperationalPolicy, run_game,
    run_game_with_event_stream, run_game_with_evidence,
};

static RUNNER_TEST_LOCK: Mutex<()> = Mutex::new(());

fn executable() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tac-scm-game-runner"))
}

/// The Python baseline executables built from `sdk/` by
/// `server/scripts/build-test-baselines` (run it before `cargo test`).
fn baseline(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../baselines")
        .join(name);
    assert!(
        path.is_file(),
        "missing {}: run `python3 scripts/build-test-baselines` in server/ first",
        path.display()
    );
    path
}

fn frozen(modes: [(&str, u64); 6], deadline_ms: u64) -> FrozenGamePlan {
    let agents = std::array::from_fn(|slot| {
        AgentArtifact::inspect(
            AGENT_IDS[slot],
            executable(),
            vec![
                "fixture-agent".into(),
                modes[slot].0.into(),
                modes[slot].1.to_string(),
            ],
        )
        .unwrap()
    });
    FrozenGamePlan::freeze(
        GamePlan::materialize([0x42; 32]).unwrap(),
        agents,
        OperationalPolicy {
            action_deadline: Duration::from_millis(deadline_ms),
            ..OperationalPolicy::default()
        },
        "runner-conformance-v1",
        0,
    )
    .unwrap()
}

#[test]
fn identity_command_binds_aligned_cargo_semantic_and_actual_runner_bytes() {
    let binary = executable();
    let output = Command::new(&binary).arg("identity").output().unwrap();
    assert!(output.status.success());
    let document = String::from_utf8(output.stdout).unwrap();
    assert!(document.contains("\"engine_cargo_version\":\"0.5.1\""));
    assert!(document.contains("\"engine_semantic_id\":\"tac-scm-engine-core/0.4.1\""));
    assert!(document.contains("\"profile_selection\":\"TAC_SCM_PROFILE\""));
    assert!(document.contains(concat!(
        "\"selectable_profiles\":{\"tac-scm-2026-draft2\":\"tac-scm-engine-core/0.4.1\",",
        "\"tac-scm-2026-draft3\":\"tac-scm-engine-core/0.5.1\"}"
    )));
    assert!(document.contains("\"event_schema_id\":\"tac-scm-event/3\""));
    assert!(document.contains("\"event_delivery_default\":\"off\""));
    assert!(document.contains("\"message_schema_id\":\"tac-scm-message/2\""));
    assert!(document.contains("\"profile_id\":\"tac-scm-2026-draft2\""));
    assert!(document.contains("\"runner_cargo_version\":\"0.5.1\""));
    assert!(document.contains("\"run_evidence_contract\":\"tac-scm-run-evidence/1\""));
    assert!(document.contains(&format!(
        "\"runner_executable_sha256\":\"{}\"",
        hex(&sha256(&std::fs::read(binary).unwrap()))
    )));
}

#[test]
fn frozen_plan_is_authoritative_not_response_timing() {
    let _guard = RUNNER_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let plan = frozen(
        [
            ("normal", 5),
            ("normal", 0),
            ("normal", 4),
            ("normal", 1),
            ("normal", 3),
            ("normal", 2),
        ],
        50,
    );
    let first = run_game(&plan).unwrap();
    let second = run_game(&plan).unwrap();
    assert_eq!(first.plan_sha256, second.plan_sha256);
    assert_eq!(
        first.result.transcript_sha256,
        second.result.transcript_sha256
    );
    assert_eq!(
        first.result.event_log_sha256,
        second.result.event_log_sha256
    );
    assert_eq!(
        first.result.final_state_sha256,
        second.result.final_state_sha256
    );
    assert_eq!(first.default_counts, [0; 6]);
    assert_eq!(first.event_delivery, "off");
    assert!(first.result.event_log_bytes.is_empty());
    assert_eq!(first.audit_record_count, 0);
    assert_eq!(first.result_record_bytes, second.result_record_bytes);

    let output = std::env::temp_dir().join(format!(
        "tac-scm-thin-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    first.write_summary_to(&plan, &output).unwrap();
    let files: std::collections::BTreeSet<_> = std::fs::read_dir(&output)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(
        files,
        [
            "action-transcript.cbor",
            "economic-summary.cbor",
            "game-plan.cbor",
            "game-result.cbor",
            "identities.txt",
            "process-summary.cbor",
            "result-record.cbor",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
    std::fs::remove_dir_all(output).unwrap();
}

#[test]
fn thin_capture_and_stream_modes_preserve_runner_semantics() {
    let _guard = RUNNER_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let plan = frozen([("normal", 0); 6], 50);
    let thin = run_game(&plan).unwrap();
    let full = run_game_with_evidence(&plan, EvidencePolicy::FULL).unwrap();
    assert_eq!(thin.event_delivery, "off");
    assert_eq!(full.event_delivery, "capture");
    assert!(!thin.result.events_captured);
    assert!(full.result.events_captured);
    assert_eq!(thin.result.event_count, full.result.event_count);
    assert_eq!(thin.result.transcript_bytes, full.result.transcript_bytes);
    assert_eq!(
        thin.result.economic_summary_bytes,
        full.result.economic_summary_bytes
    );
    assert_eq!(thin.result.daily_commitments, full.result.daily_commitments);
    assert_eq!(
        thin.result.final_state_sha256,
        full.result.final_state_sha256
    );
    assert_eq!(thin.result.scores, full.result.scores);

    let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&bytes);
    let streamed = run_game_with_event_stream(&plan, move |event| {
        sink.lock().unwrap().extend_from_slice(event);
    })
    .unwrap();
    assert_eq!(streamed.event_delivery, "stream");
    assert_eq!(streamed.result.event_count, full.result.event_count);
    assert_eq!(
        streamed.result.event_log_sha256,
        full.result.event_log_sha256
    );
    assert_eq!(
        streamed.result.transcript_bytes,
        full.result.transcript_bytes
    );
    assert_eq!(
        streamed.result.economic_summary_bytes,
        full.result.economic_summary_bytes
    );
    assert_eq!(
        streamed.result.daily_commitments,
        full.result.daily_commitments
    );
    assert_eq!(
        streamed.result.final_state_sha256,
        full.result.final_state_sha256
    );
    assert_eq!(streamed.result.scores, full.result.scores);
    assert_eq!(*bytes.lock().unwrap(), full.result.event_log_bytes);
}

#[test]
fn malformed_oversize_identity_timeout_and_exit_default_independently() {
    let _guard = RUNNER_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let plan = frozen(
        [
            ("normal", 0),
            ("malformed", 0),
            ("oversize", 0),
            ("wrong-identity", 0),
            ("timeout", 0),
            ("exit", 0),
        ],
        50,
    );
    let output = run_game_with_evidence(&plan, EvidencePolicy::FULL).unwrap();
    assert_eq!(output.default_counts[0], 0);
    assert!(output.default_counts[1..].iter().all(|count| *count == 220));
    assert!(!output.audit_bytes.is_empty());
    assert_eq!(output.result.scores.len(), 6);
}

/// Every fixture here marks its action final, so the window closes as soon as
/// all six have answered; the long deadline only keeps a loaded machine from
/// turning a slow normal fixture into a timeout default.
const STRUCTURAL_TEST_DEADLINE_MS: u64 = 5000;

/// An action the engine would treat as a game-fatal structural error
/// takes the malformed-message path instead. The violating slot gets a
/// `schema-invalid` receipt and a `transport-error` default on day 0; as a
/// local process it is then stopped and defaults on every later day
/// (`process-exit`), while the game completes and all six are scored.
fn structural_violation_defaults_the_slot_and_the_game_completes(mode: &str, detail: &str) {
    let _guard = RUNNER_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let plan = frozen(
        [
            (mode, 0),
            ("normal", 0),
            ("normal", 0),
            ("normal", 0),
            ("normal", 0),
            ("normal", 0),
        ],
        STRUCTURAL_TEST_DEADLINE_MS,
    );
    let output = run_game_with_evidence(&plan, EvidencePolicy::FULL).unwrap();
    assert_eq!(output.default_counts[0], 220, "{mode}");
    assert_eq!(&output.default_counts[1..], &[0; 5], "{mode}");
    assert_eq!(output.result.scores.len(), 6, "{mode}");
    let contains = |needle: &[u8]| {
        output
            .audit_bytes
            .windows(needle.len())
            .any(|window| window == needle)
    };
    assert!(contains(detail.as_bytes()), "{mode}");
    assert!(contains(b"default"), "{mode}");
}

#[test]
fn invalid_command_or_line_id_defaults_instead_of_aborting() {
    structural_violation_defaults_the_slot_and_the_game_completes(
        "invalid-id",
        "schema-invalid: invalid-id",
    );
}

#[test]
fn id_shared_between_command_and_line_defaults_instead_of_aborting() {
    structural_violation_defaults_the_slot_and_the_game_completes(
        "shared-id",
        "schema-invalid: duplicate-id",
    );
}

#[test]
fn too_many_production_lines_default_instead_of_aborting() {
    structural_violation_defaults_the_slot_and_the_game_completes(
        "production-lines",
        "schema-invalid: production-line-limit",
    );
}

#[test]
fn too_many_delivery_lines_default_instead_of_aborting() {
    structural_violation_defaults_the_slot_and_the_game_completes(
        "delivery-lines",
        "schema-invalid: delivery-line-limit",
    );
}

#[test]
fn supplier_rfq_id_reused_on_one_supplier_line_defaults_instead_of_aborting() {
    structural_violation_defaults_the_slot_and_the_game_completes(
        "supplier-rfq-reuse",
        "schema-invalid: duplicate-supplier-rfq-id",
    );
}

#[test]
fn actions_at_the_structural_limits_are_accepted_every_day() {
    let _guard = RUNNER_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let plan = frozen(
        [
            ("structural-limits", 0),
            ("normal", 0),
            ("normal", 0),
            ("normal", 0),
            ("normal", 0),
            ("normal", 0),
        ],
        STRUCTURAL_TEST_DEADLINE_MS,
    );
    let output = run_game(&plan).unwrap();
    assert_eq!(output.default_counts, [0; 6]);
    assert_eq!(output.result.scores.len(), 6);
}

#[test]
fn conflicting_duplicate_defaults_without_replacing_authority() {
    let _guard = RUNNER_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let plan = frozen(
        [
            ("duplicate", 0),
            ("normal", 2),
            ("normal", 2),
            ("normal", 2),
            ("normal", 2),
            ("normal", 2),
        ],
        50,
    );
    let output = run_game(&plan).unwrap();
    assert_eq!(output.default_counts[0], 220);
    assert_eq!(&output.default_counts[1..], &[0; 5]);
}

#[test]
fn a_late_process_can_recover_without_permanent_slot_disablement() {
    let _guard = RUNNER_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let plan = frozen(
        [
            ("recover", 100),
            ("normal", 0),
            ("normal", 0),
            ("normal", 0),
            ("normal", 0),
            ("normal", 0),
        ],
        50,
    );
    let output = run_game(&plan).unwrap();
    assert!(output.default_counts[0] > 0);
    assert!(output.default_counts[0] < 220);
    assert_eq!(&output.default_counts[1..], &[0; 5]);
}

#[test]
fn mixed_frozen_python_baselines_complete_all_lifecycle_days() {
    let _guard = RUNNER_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let names = [
        "baseline-random-v2",
        "baseline-heuristic-v2",
        "baseline-optimizer-v2",
        "baseline-random-v2",
        "baseline-heuristic-v2",
        "baseline-optimizer-v2",
    ];
    let agents = std::array::from_fn(|slot| {
        AgentArtifact::inspect(AGENT_IDS[slot], baseline(names[slot]), Vec::new()).unwrap()
    });
    let plan = FrozenGamePlan::freeze(
        GamePlan::materialize([0x51; 32]).unwrap(),
        agents,
        OperationalPolicy {
            action_deadline: Duration::from_secs(15),
            ..OperationalPolicy::default()
        },
        "baseline-runner-smoke-v2",
        0,
    )
    .unwrap();
    let output = run_game_with_evidence(&plan, EvidencePolicy::FULL).unwrap();
    assert_eq!(output.default_counts, [0; 6]);
    assert_eq!(output.result.scores.len(), 6);
    assert!(output.result_record_bytes.len() > 100);
    let conformance = validate_event_log(&output.result.event_log_bytes).unwrap();
    assert_eq!(conformance.entity_types.len(), 13);
    let expected_economic_kinds = [
        "bank-interest-assessment",
        "command-rejection",
        "customer-award",
        "customer-cancellation",
        "customer-demand",
        "customer-payment",
        "customer-penalty",
        "factory-delivery",
        "factory-inventory",
        "factory-production",
        "storage-assessment",
        "supplier-allocation",
        "supplier-inventory",
        "supplier-price",
        "supplier-production",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(
        conformance.economic_kinds, expected_economic_kinds,
        "the frozen baselines should exercise every ordinary lifecycle kind; the final-shortfall supplier-refund branch has a focused engine test"
    );
    assert!(conformance.ledger_transaction_count > 0);
}

#[test]
fn engine_and_runner_messages_have_monotone_sequences_for_every_recipient() {
    let _guard = RUNNER_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let plan = frozen([("sequence-check", 0); 6], 50);
    let output = run_game(&plan).unwrap();
    assert_eq!(output.default_counts, [0; 6]);
}

#[test]
fn predecessor_message_schema_is_rejected_fail_closed() {
    let _guard = RUNNER_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let plan = frozen(
        [
            ("old-schema", 0),
            ("normal", 0),
            ("normal", 0),
            ("normal", 0),
            ("normal", 0),
            ("normal", 0),
        ],
        50,
    );
    let output = run_game(&plan).unwrap();
    assert_eq!(output.default_counts[0], 220);
    assert_eq!(&output.default_counts[1..], &[0; 5]);
}

#[test]
fn profile_environment_selects_draft3_explicitly_for_the_one_command() {
    let _guard = RUNNER_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let names = [
        "baseline-random-v2",
        "baseline-heuristic-v2",
        "baseline-optimizer-v2",
        "baseline-random-v2",
        "baseline-heuristic-v2",
        "baseline-optimizer-v2",
    ];
    let root = std::env::temp_dir().join(format!("tac-scm-profile-env-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let run = |profile: Option<&str>, output: &PathBuf| {
        let mut command = Command::new(executable());
        command
            .arg("one")
            .arg(output)
            .args(names.iter().map(|name| baseline(name)))
            .env("TAC_SCM_ACTION_DEADLINE_MS", "15000")
            .env_remove("TAC_SCM_EVIDENCE")
            .env_remove("TAC_SCM_TRAINING_SKIP_REPLAY")
            .env_remove("TAC_SCM_TRAINING_NO_COMMITMENTS");
        match profile {
            Some(profile) => command.env("TAC_SCM_PROFILE", profile),
            None => command.env_remove("TAC_SCM_PROFILE"),
        };
        command.output().unwrap()
    };
    let rejected = run(Some("tac-scm-2026-draft9"), &root.join("unknown"));
    assert!(!rejected.status.success());
    assert!(!root.join("unknown").exists());
    let output = root.join("draft3");
    let completed = run(Some("tac-scm-2026-draft3"), &output);
    assert!(
        completed.status.success(),
        "{}",
        String::from_utf8_lossy(&completed.stderr)
    );
    let game = output.join("game-000000");
    let plan = std::fs::read(game.join("game-plan.cbor")).unwrap();
    assert!(
        plan.windows(b"tac-scm-2026-draft3".len())
            .any(|window| window == b"tac-scm-2026-draft3")
    );
    let identities = std::fs::read_to_string(game.join("identities.txt")).unwrap();
    assert!(identities.contains("game_result_id=result/"));
    std::fs::remove_dir_all(&root).unwrap();
}
