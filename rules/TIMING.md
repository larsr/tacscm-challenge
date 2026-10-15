# Timing, defaults and startup

How an agent's day is timed, what happens when it is late, crashes or
sends something malformed, and how a game starts and ends. This describes
the game runner shipped with engine `tac-scm-engine-core` 0.5.1 when it
runs each agent as a process over stdin/stdout (the transport the
challenge uses). The message format itself is specified in
`sdk/PROTOCOL.md`.

**Challenge values.** The container limits (CPU, memory), the startup
grace and any runner setting the challenge changes from the defaults below
are given in `tournament/SUBMISSION.md`; where it gives a value, that value
applies. The daily decision deadline of the game is 15 seconds.

## 1. Messages in a game

| When | Direction | Message |
|---|---|---|
| Start | runner → agent | `GameBootstrap`: agent id and seat, roster, catalog, public configuration (`RULES.md` section 3). |
| Each day, after Inventory close | runner → agent | `DayObservation` for day `d` (`RULES.md` section 6). The action window opens when it is sent. |
| During the window | agent → runner | One or more `DayActionSet` messages for day `d`. |
| After each agent message | runner → agent | `ActionReceipt`: whether the message was accepted as a candidate, its revision and digest, or the schema error. |
| After the window closes and the day is committed | runner → agent | `ActionCommitted`: the action that was committed (its digest), its cause (`agent-submission` or a default cause), and for every command and line of it whether it was accepted or rejected, with the rejection code, path and detail. |
| After day 219 | runner → agent | `GameFinished`: status and the six final scores. Then the runner closes the agent's stdin. |

Each message is one frame: a 4-byte big-endian length followed by that
many bytes of canonical CBOR.

## 2. The daily decision window

- **Deadline.** The window for day `d` lasts **15,000 ms**
  (`OperationalPolicy.action_deadline`) from the moment the runner has sent
  the day-`d` observations. Time spent before the observation arrives, for
  example still working on the previous day, counts against the window.
- **Revisions.** An agent MAY send several action sets in one window, with
  increasing `revision` numbers (starting at 1). The valid candidate with
  the highest revision received before the cutoff is committed. A message
  whose revision is not higher than the current candidate's is ignored.
  Message ids MUST be unique for the whole game: resending the identical
  message is ignored, reusing an id with different bytes is a
  `transport-error`.
- **Final submission.** An agent that sets `final_submission: true` in the
  envelope of a valid action set gives up the rest of its window; later
  messages that day are ignored. When every agent has done so (or has
  already defaulted), the window closes early. Otherwise the runner waits
  for the full deadline even if all agents have sent a candidate.
- **Limits per window.** At most 16 frames per agent per day
  (`max_frames_per_window`), each at most 2 MiB (`MAX_FRAME_BYTES`).
- The committed action is the candidate itself; there is no implicit
  carry-over from earlier days (`RULES.md` section 7).

## 3. Defaults

If an agent has no valid candidate when its window ends, it gets the
**default action**: the empty action set (no customer offers, no supplier
RFQs or orders, and no production and no deliveries on the next day). The
default is recorded with a cause, reported back in `ActionCommitted.cause`
and in every later observation's `default_history`:

| Cause | When |
|---|---|
| `timeout` | No valid candidate arrived before the deadline. |
| `transport-error` | The agent sent a malformed message, or reused a message id with different bytes. |
| `resource-limit` | A frame over 2 MiB, or more than 16 frames in one window. |
| `process-exit` | The agent's process exited or closed its output, or the runner could no longer send to it. |

A default changes nothing else: the rules of the game apply to the empty
action exactly as to any other. Customer orders still fall due and draw
penalties, suppliers still deliver and invoice, interest and storage are
still charged.

**After a timeout the agent continues.** A slot that merely missed the
deadline gets the next day's observation as usual. Its late action for the
earlier day is recognised by its day and observation id and silently
ignored; it is not an error.

**After any other cause the agent is out for the rest of the game.** For
a process run over stdin/stdout, a malformed message, a reused message id,
an oversized frame, too many frames, or the process exiting ends the
slot's participation: at the end of that day's window the runner stops the
process (stdin closed, then killed after the shutdown grace), and the slot
receives the default action, with cause `process-exit`, on every remaining
day. On the day it happened, a valid candidate the agent had already sent
is still committed (except after a reused message id, which discards it);
without one the day defaults with the cause in the table. The agent's cash
keeps evolving under the rules until the end of the game, and its final
cash is its score.

## 4. What counts as malformed

The runner accepts a message as a candidate only if all of the following
hold; otherwise it answers with an `ActionReceipt` carrying a
`schema-invalid` error, and the slot is treated as in section 3
(`transport-error`, then out for the rest of the game):

- canonical CBOR, a map with exactly the envelope keys (`schema_id`,
  `schema_version`, `game_id`, `message_id`, `sender_role`, `sender_id`,
  `recipient`, `game_day`, `phase`, `sender_sequence`, `payload_digest`,
  `payload`, and optionally `causation_id`, `final_submission`);
- `schema_id` `tac-scm-message/2`, `schema_version` `[2,0,0]`, the game's
  id, `sender_role` `manufacturer`, `sender_id` the agent's own id,
  `recipient` `runner`, `phase` `action-window`;
- `payload_digest` = SHA-256 of the canonical payload;
- the payload is a closed `day-action-set` with the agent's id, all five
  lists present, every entry with exactly its fields and types;
- `revision` ≥ 1.

Exceptions: a message whose `game_day` or `day` is not the current day, or
whose `observation_id` is not the current observation's, is a late message
and is ignored without a default.

**Structural rules.** The action set is also malformed, with the same
consequence, if:

- a `command_id` or `line_id` does not match `[a-z0-9][a-z0-9._-]{0,31}`;
- two of its `command_id`s and `line_id`s are equal (one namespace over
  all five lists);
- it has more than 256 production lines or more than 4,096 delivery lines;
- two of its supplier RFQs have the same `supplier_id`, `component_id`
  and `rfq_id`.

The runner makes these checks when the message arrives, before the action
reaches the engine. The receipt's `detail` names the check:
`schema-invalid: ` followed by `invalid-id`, `duplicate-id`,
`production-line-limit`, `delivery-line-limit` or
`duplicate-supplier-rfq-id`. Reusing an `rfq_id`
for another component of the same supplier is not malformed (it merges the
two RFQs into one alternative group, `RULES.md` section 7). The kit's SDK
refuses to build a malformed action and, as a guard, is stricter on RFQ
ids: it requires distinct (`supplier_id`, `rfq_id`) pairs, so an agent
built with it never sends the merged-group case either.

**Economic errors are not malformed.** A well-formed action whose commands
break a game rule (an offer above the reserve, an RFQ due too early, an
order on an expired offer, a delivery for the wrong quantity, …) is
committed; each such command is rejected individually and reported in
`ActionCommitted`, and the rest take effect (`RULES.md` section 7).

## 5. Startup

1. The runner starts the six agent processes and sends each its
   `GameBootstrap`.
2. It then waits the **startup grace** (`OperationalPolicy.startup_grace`;
   runner default 250 ms; the challenge's value is in
   `tournament/SUBMISSION.md`).
3. It then runs day 0 and sends the day-0 observations, which opens the
   day-0 window.

The bootstrap and observations are buffered on the agent's stdin, so an
agent that is still loading when day 0 opens loses nothing but time: if it
is not ready within the day-0 window it defaults for that day with
`timeout` and continues on the next day (section 3). An agent SHOULD
therefore finish any heavy initialisation within the startup grace plus
the day-0 window, and SHOULD drain stdin promptly: the runner buffers a
bounded number of outgoing frames per agent, and an agent that stops
reading for very long is treated as gone (`process-exit`).

Day 0 has no customer RFQs (`RULES.md` section 8.1), so a missed day 0
costs at most a day of supplier ordering and nothing on the customer side.

## 6. End of game

After the commit of day 219 and final settlement, each live agent receives
`GameFinished` with the six scores. The runner then closes the agent's
stdin and kills the process if it has not exited within the shutdown grace
(`OperationalPolicy.shutdown_grace`, default 100 ms). Nothing an agent
sends after day 219's window has any effect. No state is kept between
games.

## 7. Running locally

The kit's runner reads its timing from the environment:
`TAC_SCM_ACTION_DEADLINE_MS`, `TAC_SCM_STARTUP_GRACE_MS`,
`TAC_SCM_SHUTDOWN_GRACE_MS` (the last must be 1..15,000). It plays this profile only with
`TAC_SCM_PROFILE=tac-scm-2026-draft3`; without it the runner plays the
older default profile `tac-scm-2026-draft2`. An agent's stderr is
discarded unless `TAC_SCM_RUNNER_INHERIT_STDERR` is set.
