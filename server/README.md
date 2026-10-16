# Server

The game engine and the game runner, in Rust: `crates/engine-core` (the
deterministic game: market, factory, bank, scoring, replay) and
`crates/game-runner` (the `tac-scm-game-runner` executable that plays one
game of six agent processes and writes its result). Engine 0.5.1; it plays
the profiles `tac-scm-2026-draft2` (its default) and `tac-scm-2026-draft3`.

**The challenge uses `tac-scm-2026-draft3`.** The runner selects it from the
environment (`TAC_SCM_PROFILE=tac-scm-2026-draft3`); the kit's
`runner/play_game.py` always sets it, and every game plan records the
profile it was played under. The rules are in `rules/`.

## Build

Needs Rust 1.96 or later (`rustup`), no other system libraries.

```sh
cd server
cargo build --release --locked
# the runner: target/release/tac-scm-game-runner
```

The only third-party crate is `sha2` 0.10.9 with its dependencies (all MIT or
MIT OR Apache-2.0), pinned by `Cargo.lock`.

## Test

The runner tests play games with the three Python baseline agents of `sdk/`,
built as standalone executables first (needs Python 3.11 or later):

```sh
cd server
python3 scripts/build-test-baselines     # writes server/baselines/
cargo test --release --locked
```

Some runner tests bind loopback TCP ports (the remote mode); a sandbox that
forbids that makes only those tests fail.

## Run a game

Use the kit's runner (`runner/README.md`), which sets the profile and the
tournament's deadline and prints the scores:

```sh
uv run python runner/play_game.py --root 0 --out runs/g0 AGENT AGENT AGENT AGENT AGENT AGENT
```

The runner binary directly, for reference:

```sh
TAC_SCM_PROFILE=tac-scm-2026-draft3 TAC_SCM_ACTION_DEADLINE_MS=15000 \
  target/release/tac-scm-game-runner one OUTPUT_DIR A1 A2 A3 A4 A5 A6
```

`one` plays one game with a root seed derived from ordinal 0; `planned`
takes an explicit root seed and a schedule position (see `play_game.py`);
`identity` prints the engine and runner identity. Environment settings:

| Variable | Effect |
|---|---|
| `TAC_SCM_PROFILE` | `tac-scm-2026-draft3` for the challenge (unset: draft2) |
| `TAC_SCM_ACTION_DEADLINE_MS` | daily decision window, default 15000 |
| `TAC_SCM_STARTUP_GRACE_MS` | pause after starting the agents, default 250 |
| `TAC_SCM_SHUTDOWN_GRACE_MS` | time to exit after the game, default 100 (max 15000) |
| `TAC_SCM_EVIDENCE` | `thin` (default) or `full` (adds the event log and audit) |
| `TAC_SCM_RUNTIME_SUMMARY` | `1` writes per-agent decision times |
| `TAC_SCM_TRAINING_RECORD` | a path: writes every seat's observations and committed actions as a CBOR sequence, for training |
| `TAC_SCM_RUNNER_INHERIT_STDERR` | set: agents' standard error goes to the terminal |

`engine-core` also builds `tac-scm-engine-core` (a self-test game with
built-in policies) and `materialize-plan-summary` (the public parameters of
one root seed). The `planned-research` command and `historical.rs` are
research counterfactuals of historical rule sets, never used by the
challenge profile.

## Container image

`Dockerfile` builds an image with the runner, the SDK and the house agents
(Python 3.12, numpy, scipy). Build it from the repository root, after
placing the release-asset models under `agents/models/` (`agents/MODELS.md`):

```sh
docker build -f server/Dockerfile -t tacscm-kit .
docker run --rm tacscm-kit python runner/play_game.py --root 0 --out /tmp/g0 \
  tactex-inspired-v4-m5 deepmaize-inspired-v4-m2 botticelli-inspired-v4 \
  narc-inspired-v1 iota-inspired-v1 baseline-heuristic
```

The image is based on `python:3.12-slim-bookworm`, which carries Debian
packages under their own licences (GPL and LGPL among them; sources from
Debian). Nothing copyleft is part of this kit's own source.

## Licence

MIT, as the rest of the kit.
