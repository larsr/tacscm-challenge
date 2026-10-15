//! Materialize only the public exogenous game-plan summary for one root.

use tac_scm_engine_core::crypto::hex;
use tac_scm_engine_core::game::GamePlan;
use tac_scm_engine_core::historical::HistoricalProfile;

fn main() {
    let root = std::env::args().nth(1).unwrap_or_else(|| {
        panic!("usage: materialize-plan-summary ROOT_SEED_HEX [RESEARCH_PROFILE_ID]")
    });
    let seed = parse_digest(&root);
    let profile = std::env::args().nth(2);
    let plan = profile
        .map_or_else(
            || GamePlan::materialize(seed),
            |profile_id| {
                let profile = HistoricalProfile::from_id(&profile_id)
                    .unwrap_or_else(|| panic!("unknown research profile: {profile_id}"));
                GamePlan::materialize_research(seed, profile)
            },
        )
        .expect("valid materialized game plan");
    println!(
        concat!(
            "{{\"schema\":\"tac-scm-public-plan-summary/v1\",",
            "\"profile_id\":\"{}\",",
            "\"root_seed_commitment\":\"{}\",",
            "\"loan_rate_ppb\":{},",
            "\"deposit_rate_ppb\":{},",
            "\"storage_rate_ppb\":{},",
            "\"initial_means_nano\":[{},{},{}],",
            "\"configuration_identity\":\"{}\"}}"
        ),
        plan.profile_id,
        hex(&tac_scm_engine_core::crypto::sha256(&seed)),
        plan.loan_rate_ppb,
        plan.deposit_rate_ppb,
        plan.storage_rate_ppb,
        plan.initial_means_nano[0],
        plan.initial_means_nano[1],
        plan.initial_means_nano[2],
        hex(&plan.configuration_identity),
    );
}

fn parse_digest(value: &str) -> [u8; 32] {
    assert!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "root seed must be one 32-byte hexadecimal digest"
    );
    let mut result = [0_u8; 32];
    for (index, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .expect("validated hexadecimal root seed");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::parse_digest;

    #[test]
    fn parses_exact_root_digest() {
        assert_eq!(parse_digest(&"ab".repeat(32)), [0xab; 32]);
    }
}
