# Agent protocol

How an agent talks to the game runner, in enough detail to write an agent in
any language. The Python SDK in this directory implements it
(`tac_scm_sdk.transport`, `tac_scm_sdk.runtime`); the closed shape of every
message is `schema/contracts-v0.4.0.cddl`. The game itself (what the commands
do, prices, capacities, penalties) is in `rules/`.

Identifiers: message schema `tac-scm-message/2` version `[2, 0, 0]`; transport
`tac-scm-framed-cbor-stdio/1`. The runner in `server/` is the reference
implementation of this protocol.

## 1. One process per game

The runner starts each of the six agents as an executable, once per game. It
writes messages to the agent's standard input and reads the agent's messages
from its standard output. Standard error is captured (up to 64 KiB) and
otherwise discarded; it is the place for logging. The agent never opens a
network connection and keeps no state between games.

The runner may pass these arguments, which an agent may use or ignore but must
accept, and an agent must also tolerate arguments it does not know:

| Argument | Meaning |
|---|---|
| `--campaign-id ID` | the series of games this game belongs to |
| `--artifact-digest HEX` | SHA-256 of the agent's executable |
| `--instance-id ID` | the seat's instance name in the schedule |
| `--block-index N` | the game's root index in the series (a seed source) |

The agent learns its own id (`agent-a` .. `agent-f`, by seat) from the
messages. Seats are anonymous: nothing tells an agent who its opponents are.

## 2. Framing

Every message in either direction is one frame: a 4-byte unsigned big-endian
length, then exactly that many bytes holding one CBOR data item. There is no
other delimiter, header or padding.

| Direction | Limit |
|---|---|
| agent to runner | 2 MiB per frame (the rules bound an action set at 2 MiB of canonical CBOR). A longer frame ends the agent's game (section 6). |
| runner to agent | not bounded by the rules. An observation grows with the agent's open positions; the SDK reads frames up to 32 MiB, far above any observation seen in a 220-day game. A reader in another language should not impose a smaller limit. |

## 3. Canonical CBOR

Every item is deterministic CBOR (RFC 8949 section 4.2.1): definite lengths,
the shortest integer encoding, map keys sorted by their encoded bytes, no
duplicate keys, NFC-normalised UTF-8 text, and no floating-point values. Money
is an integer number of microcredits (1 credit = 1,000,000 microcredits);
fractions of a million (ppm) or a billion (ppb) are integers. Maps are closed:
an unknown or missing key makes a message invalid. A key whose value is absent
is written explicitly as `null` where the schema allows it, never omitted,
except the optional envelope keys below.

`digest(x)` is SHA-256 of the canonical encoding of `x`.

## 4. The envelope

Every frame holds one message map:

| Key | Value |
|---|---|
| `schema_id` | `"tac-scm-message/2"` |
| `schema_version` | `[2, 0, 0]`; a receiver rejects any other major version |
| `game_id` | the game's id, copied from the runner's messages |
| `message_id` | unique in the game; the SDK uses `message/{agent_id}/{day}/{sequence}` |
| `sender_role` | `"manufacturer"` for an agent; `"engine"` or `"runner"` towards the agent |
| `sender_id` | the agent's own id in its messages |
| `recipient` | `"runner"` in the agent's messages; the agent's id in messages to it |
| `game_day` | the day (0..219); required on an action message |
| `phase` | `"action-window"` on an action message |
| `sender_sequence` | strictly increasing per sender (gaps allowed) |
| `causation_id` | optional |
| `final_submission` | optional, only on an action message (section 5) |
| `payload_digest` | `digest(payload)`, 32 bytes |
| `payload` | one typed payload map; its `type` key names it |

## 5. Message sequence

```
runner -> agent   game-bootstrap               once, before day 0
for each day 0 .. 219:
  engine -> agent   day-observation            exactly one per agent and day
  agent  -> runner  day-action-set             one or more revisions
  runner -> agent   action-receipt             one per frame received
  engine -> agent   action-committed           the action that counted
engine -> agent   game-finished                final scores
runner closes the agent's standard input; the agent exits
```

- **game-bootstrap**: the game definition, profile, the agent's id and seat,
  the roster and the public configuration (catalog, bill of materials,
  supplier mapping, factory parameters).
- **day-observation**: everything the agent may know that day: cash and own
  ledger entries, inventories, last day's production and delivery results,
  own supplier RFQs, offers, orders, deliveries and reputation, today's
  customer RFQs, new and active customer orders, settlements, the disposition
  of yesterday's action, the daily price report, the periodic market report
  and the agent's default history. It is sufficient on its own: an agent
  that kept no memory could still act.
- **day-action-set**: the complete replacement for the day (not a patch):
  `agent_id`, `observation_id` (from the day's observation), `day`,
  `revision` (1, 2, ... for successive candidates of the same day), and five
  arrays, all present, empty meaning no action:

  | Array | Record | Bound |
  |---|---|---|
  | `customer_offers` | command id, customer RFQ id, unit price | at most one per RFQ; only today's RFQs accept offers |
  | `supplier_rfqs` | command id, own RFQ id, supplier, component, quantity (0 = price probe), due day, maximum unit price or null | at most five positive RFQs and one probe per supplier-component pair and day |
  | `supplier_orders` | command id, supplier offer id | at most one per alternative group |
  | `production_schedule` | line id, priority, product, positive quantity | at most 256 lines |
  | `delivery_schedule` | line id, priority, customer order id, the order's full quantity | at most 4096 lines; at most one per customer order |

  Agent-chosen ids (command, line, own RFQ ids) match
  `[a-z0-9][a-z0-9._-]{0,31}` and are unique within the action set.
- **action-receipt**: the revision and digest the runner received, whether the
  candidate is valid, and structural errors.
- **action-committed**: the digest of the action that counted for the day,
  its cause (`agent-submission`, or the default's cause: `timeout`,
  `process-exit`, `resource-limit`, `transport-error`, `missing`) and, for
  every command and line, whether it was accepted or rejected and why.
- **game-finished**: termination status and the six final scores.

A command that is well formed but economically impossible (an expired offer,
too few components, too few cycles) is rejected alone; the rest of the action
still counts, and the rejection is reported in `action-committed`.

## 6. Deadlines and defaults

- The runner starts all six agents, sends the bootstrap, waits a short
  startup grace, then sends day 0's observation. The tournament's values
  (deadline, startup grace) are in `rules/TIMING.md`; the runner's defaults
  are a 15 s window and a 250 ms grace.
- A day's window opens when the observation is sent and lasts the action
  deadline (15 s); time still spent on the previous day counts against it.
  Until the cutoff the agent may send further revisions; the highest valid
  revision received before the cutoff counts, and a revision not higher than
  the current candidate's is ignored. Message ids are unique for the whole
  game: resending identical bytes is ignored. An action
  message with `final_submission: true` gives up the rest of the window (the
  SDK always sets it); the day ends early once every agent has done so.
- At most 16 frames per agent and window.
- No valid candidate by the cutoff: the agent's action for the day is the
  default, an empty action set, with cause `timeout`; the agent continues the
  next day.
- These end the agent's game, with the empty default for every remaining day:
  exiting or closing its output; a frame over 2 MiB or a 17th frame in one
  window; a frame that is not a valid action message for the current
  observation (bad CBOR, wrong schema, game, sender, recipient, phase, digest,
  agent, or revision 0); a reused message id with different bytes. An action
  for an earlier day or an earlier observation is only ignored. On the day it
  happens, a valid candidate already received still counts (not after a
  reused message id).
- A structurally invalid action set (an id outside the id pattern, ids not
  distinct across all commands and lines, more than 256 production or
  4,096 delivery lines, a repeated supplier, component and `rfq_id`) is
  malformed: the runner rejects it before the engine, like the cases above
  (`rules/TIMING.md` section 4). The SDK's `DayActionSet` refuses all of
  them on construction, and is stricter on `rfq_id` (distinct per
  supplier).
- After `game-finished` the runner closes the agent's input; an agent still
  running after the shutdown grace is killed.

The runner's clock alone decides what arrived in time; replays use the
recorded actions and never rerun agents.

## 7. Remote play (not used in this edition)

The runner can also serve one game over TCP (`tac-scm-game-runner serve`),
with the same frames inside a session protocol (`tac-scm-session/1`): the
first frame authenticates a seat with a per-seat token, later frames wrap one
`tac-scm-message/2` item each, with the same 2 MiB bound on the agent's
frames. A non-loopback bind requires TLS terminated in front of the server.
The challenge runs every agent as a local process (section 1), so an entrant
does not need this.
