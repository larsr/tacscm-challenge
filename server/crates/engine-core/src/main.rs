use std::hint::black_box;
use std::time::Instant;

use tac_scm_engine_core::crypto::hex;
use tac_scm_engine_core::game::{GamePlan, run_default_game, verify_default_replay};
use tac_scm_engine_core::{SliceConfig, fixture_actions, run_slice, verify_replay};

const OPENING_CASH: [i64; 6] = [-1_000_000_000; 6];

fn main() {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        Some("--slice") => demonstrate_slice(),
        Some("--benchmark") => {
            let iterations = arguments.next().map_or(100_000, |value| {
                value.parse().expect("positive iteration count")
            });
            benchmark(iterations);
        }
        Some("--full-game") | None => demonstrate_full_game(),
        Some(other) => panic!("unknown argument: {other}"),
    }
}

fn demonstrate_full_game() {
    let plan = GamePlan::materialize([7_u8; 32]).expect("materialized game plan");
    let started = Instant::now();
    let output = run_default_game(&plan).expect("complete default game");
    verify_default_replay(&plan, &output).expect("complete game replay");
    println!(
        "{{\"status\":\"ok\",\"profile\":\"{}\",\"days\":220,\"agents\":6,\"elapsed_ms\":{},\"winner\":\"{}\",\"score_microcredits\":{},\"transcript_sha256\":\"{}\",\"event_delivery\":\"off\",\"event_count\":{},\"state_sha256\":\"{}\",\"engine_semantic_replay\":true}}",
        plan.profile_id,
        started.elapsed().as_millis(),
        output.scores[0].0,
        output.scores[0].1,
        hex(&output.transcript_sha256),
        output.event_count,
        hex(&output.final_state_sha256)
    );
}

fn demonstrate_slice() {
    let config = SliceConfig::fixture();
    let actions = fixture_actions(&config);
    let output = run_slice(&config, OPENING_CASH, &actions).expect("fixture transition");
    verify_replay(&config, OPENING_CASH, &actions, &output).expect("fixture replay");
    println!(
        "{{\"status\":\"ok\",\"agents\":6,\"events\":{},\"winner\":\"{}\",\"transcript_sha256\":\"{}\",\"event_log_sha256\":\"{}\",\"state_sha256\":\"{}\",\"replay\":true}}",
        output.events.len(),
        output
            .state
            .customer_order
            .as_ref()
            .expect("fixture order")
            .agent_id,
        hex(&output.transcript_sha256),
        hex(&output.event_log_sha256),
        hex(&output.final_state_sha256)
    );
}

fn benchmark(iterations: u64) {
    assert!(iterations > 0, "iteration count must be positive");
    let config = SliceConfig::fixture();
    let actions = fixture_actions(&config);
    let started = Instant::now();
    let mut checksum = 0_u8;
    for _ in 0..iterations {
        let output = run_slice(
            black_box(&config),
            black_box(OPENING_CASH),
            black_box(&actions),
        )
        .expect("fixture transition");
        checksum = checksum.wrapping_add(black_box(output.event_log_sha256[0]));
    }
    let elapsed = started.elapsed();
    let elapsed_ns = u64::try_from(elapsed.as_nanos()).expect("benchmark duration fits u64");
    println!(
        "{{\"runtime\":\"rust\",\"iterations\":{iterations},\"elapsed_ns\":{elapsed_ns},\"ns_per_transition\":{},\"checksum\":{checksum}}}",
        elapsed_ns / iterations
    );
}
