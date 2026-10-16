# SDK

The Python SDK for TAC SCM 2026 agents, the protocol specification and a
template agent.

| Path | Contents |
|---|---|
| `tac_scm_sdk/` | typed, immutable message records (`contracts`), canonical CBOR (`cbor`), framing (`transport`), the serve loop (`runtime`), catalog facts (`profile`); standard library only |
| `tac_scm_baselines/` | three transparent baseline policies (random, heuristic, rolling-horizon optimizer), as examples and opponents |
| `template/my_agent.py` | a minimal agent that plays legally, to start from |
| `PROTOCOL.md` | the stdio protocol, for an agent in any language |
| `schema/contracts-v0.4.0.cddl` | the closed schema of every message and event |
| `tests/` | SDK checks (`uv run pytest sdk/tests`) |

The SDK speaks `tac-scm-message/2`, the message schema of engine 0.5.1, under both
profiles; it needs Python 3.11 or later.

## Install

From the repository root, with [uv](https://docs.astral.sh/uv/):

```sh
uv sync --locked   # SDK, baselines and house agents
```

Run commands in that environment with `uv run`. `uv sync --package
tac-scm-sdk` installs only the SDK and the console scripts
`baseline-random`, `baseline-heuristic` and `baseline-optimizer`.

## Write an agent

An agent is an object with `act(observation) -> DayActionSet`, served by
`tac_scm_sdk.runtime.serve`:

```python
from tac_scm_sdk.contracts import DayActionSet, DayObservation, empty_action
from tac_scm_sdk.runtime import serve

class MyAgent:
    def act(self, observation: DayObservation) -> DayActionSet:
        return empty_action(observation)   # replace with a policy

raise SystemExit(serve(MyAgent()))
```

`template/my_agent.py` is a complete small policy (deliver, produce, bid,
buy). Make the file executable and play it against the house agents:

```sh
uv run python runner/play_game.py --root 0 --out runs/t0 sdk/template/my_agent.py \
  baseline-heuristic iota-inspired-v1 narc-inspired-v1 minnetac-inspired-v1 botticelli-inspired-v4
```

Things worth knowing:

- The observation holds everything the agent may know that day; the SDK's
  `History` keeps earlier observations if the agent wants them. Ids, prices
  and quantities are integers (money in microcredits).
- `DayActionSet` validates itself on construction (id formats, unique ids,
  at most five positive supplier RFQs and one probe per supplier-component
  pair and day); an invalid action raises `ContractError`. An uncaught
  exception ends the agent's game (`PROTOCOL.md` section 6), so catch errors
  in your policy and fall back to `empty_action` if in doubt.
- Each day's decision has a 15 s deadline (`rules/TIMING.md`); the SDK
  submits one final action per day.
- Frames from the runner may be large late in a game; the SDK reads up to
  32 MiB per frame and writes at most 2 MiB (the rules' bound on an action).

## Changes from earlier SDK versions

This SDK is the one the house agents were built against (`tac-scm-message/2`
records), published as source. Relative to its predecessor: the schema id and
version are `tac-scm-message/2` `[2, 0, 0]`; inventory items, schedule
results, supplier offers (`valid_day`, `reputation_exposure`), supplier
orders (`down_payment`), deliveries (`delivery_id`), reputations
(`reputation_id`, `component_id`), customer orders (`delivery_day`) and
settlements (`settlement_id`, `penalty_count`) carry their message/2 fields;
the read limit for frames from the runner is 32 MiB instead of 2 MiB; and the
supplier RFQ limit is checked per supplier-component pair, as the draft3
rules state.
