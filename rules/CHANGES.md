# Changes

How the 2026 game differs from the published 2007 TAC SCM rules document.

Why the 2026 game differs at all: the engine is a new implementation
written from the published rules, not from the old server, so every point
the 2007 document left open is made exact ("clarified"); all randomness
comes from named streams of one root seed, so the same root gives the same
market and every game can be replayed and compared entrant against entrant;
and the 2005-2007 supplier routine, complex and by its own document not
matching its implementation, is replaced by a deterministic batch market
that keeps its strategic features but cannot be moved by requests (no price
spoofing) and allocates scarce capacity without lotteries.

The 2026 game keeps the 2007 game's structure: six manufacturers, 220
days, the same ten components, sixteen products and bill of materials,
three customer segments, eight suppliers, the bank, storage cost, late
penalties, and final cash as the score. The table classifies each area:
**preserved** keeps the documented strategic behaviour; **clarified** makes
an under-specified point exact; **changed** deliberately differs; **new**
has no counterpart in the 2007 document. The right column is the 2026 rule,
detailed in `RULES.md`.

| Area | Versus 2007 | 2026 rule |
|---|---|---|
| Six agents, 220 days | preserved | Fixed for the profile; days numbered 0..219. |
| Catalog and bill of materials | preserved, clarified | Same ten components and sixteen products; ids such as `cpu-pintel-2`, `p01`. |
| Day numbering, end-of-game actions | clarified | Days 0..219; actions whose first effect would fall after day 219 are rejected; RFQs due after day 219 are suppressed. |
| Day pacing | clarified | A fixed sequence of phases per day; wall-clock time is runner policy (`TIMING.md`), not a rule. |
| Customer demand walk | preserved, clarified | Three segments, each with a mean and a trend; exact fixed-point trend step and unbiased draws. |
| Number of RFQs per day | preserved, clarified | Poisson with the segment mean, sampled exactly by inverse CDF. |
| RFQ product and fields | preserved, clarified | Same ranges (quantity 1..20, lead 3..12 days, reserve 75..125%, penalty 5..15%); exact rounding fixed. |
| Customer award | preserved, clarified | The lowest complete offer wins; ties broken by a uniform random draw. |
| Late penalty start and cap | clarified | Penalties start the day after the due day; the fifth cancels the order, before a delivery staged that day is accepted. |
| Customer payment | preserved, clarified | Paid on the due day for an early delivery, otherwise on the delivery day. |
| Daily price report | preserved, clarified | Minimum and maximum winning price per product of the day's awards; `null` when none. |
| Periodic market report | preserved, clarified | Every 20 days, exact aggregates and rounded means. |
| Supplier capacity | preserved, clarified | Same initial range and daily transition, with exact arithmetic. |
| Supplier planning | preserved, changed | Expected and willing capacity computed iteratively; offers must be feasible against cumulative budgets for every day. |
| Supplier price | changed | One posted price per line and due day, from commitments accepted before the batch; requests in the same batch (including probes) do not move it, so RFQs cannot be used to spoof prices. |
| Supplier allocation | changed | Deterministic weighted progressive filling by request size and reputation cubed. |
| Supplier alternatives | preserved, clarified | A partial offer plus an earliest-complete alternative, jointly feasible; no lottery. |
| Supplier reputation | preserved, clarified | Exposure counted when offers expire; documented acceptable purchase ratios (75% CPUs, 45% other components). |
| Supplier down payment | preserved, clarified | Exactly 10% of the order value, at acceptance. |
| Final supplier refund | changed | Undelivered quantity at the end is cancelled and the down payment refunded pro rata. |
| Factory schedule timing | preserved, clarified | A schedule sent on day `d` executes on day `d+1`; new PCs cannot be delivered the day they are made. |
| Factory priority and atomicity | clarified | Lines run in the agent's declared `(priority, line_id)` order; each line runs in full or not at all. |
| Supplier delivery visibility | preserved, clarified | Components delivered on day `d` are usable in production from day `d+1`. |
| Bank-rate sampling | clarified | Loan rate uniform over whole basis points 600..1,200; deposit rate half of it. |
| Bank interest | preserved, clarified | Simple daily interest at annual rate / 220, rounded half-even. |
| Storage-rate sampling | clarified | Uniform over whole basis points 2,500..5,000 per year. |
| Storage formula and timing | clarified, changed | Annual rate / 220 on the base value of the inventory held at the close of each day. |
| Numbers and overflow | clarified | Integer microcredits and ppb, checked arithmetic, no floating point. |
| Ledger | new, clarified | Every cash movement is a balanced transaction with a reason and a cause. |
| Score | preserved, clarified | Final cash; equal scores tie; tournament aggregation is outside the game. |
| Random streams | new, clarified | Named streams derived from a secret root seed; reproducible games. |
| Action timeout and recovery | new | A recorded empty default action; see `TIMING.md` for when a slot continues. |
| Protocol | new | One typed observation and one atomic action per day, independent of transport. |
| Event log and replay | new | Canonical events, state commitments, replay from the actions. |
