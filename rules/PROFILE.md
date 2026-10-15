# Profile `tac-scm-2026-draft3`: every constant

This file lists every constant of the game profile `tac-scm-2026-draft3`
(profile revision 1) as implemented by engine `tac-scm-engine-core` 0.5.1
(engine semantic identity `tac-scm-engine-core/0.5.1`). `RULES.md` cites
these constants by name.

The **Engine name** column gives the Rust constant where one exists, or the
key under which the runner publishes the value to every agent in the game
bootstrap (`semantic_configuration.<section>.<key>`), or, for values that
are literals in the engine, a descriptive name in *italics*.

## Units

| Unit | Meaning |
|---|---|
| credit | The game's currency. |
| microcredit (µcr) | 10^-6 credit. Every amount and unit price on the wire is an integer number of microcredits. |
| ppb | Parts per billion: 1,000,000,000 ppb = 1. Rates and probabilities. |
| ppm | Parts per million: 1,000,000 ppm = 1. Supplier reputation. |
| nano-unit | 10^-9 of a unit: demand means are stored as integers in nano-units (mean 25 is `25_000_000_000`). |
| bps | Basis points: 10,000 bps = 1 (100%). Used only to sample annual rates. |
| day | One game day, numbered 0..219. |
| cycle | One unit of factory capacity. |

## Identity

| Engine name | Value | Notes |
|---|---|---|
| `DRAFT3_PROFILE_ID` | `tac-scm-2026-draft3` | Profile id, frozen into each game plan and the bootstrap. |
| `DRAFT3_PROFILE_REVISION` | 1 | |
| `DRAFT3_ENGINE_SEMANTIC_ID` | `tac-scm-engine-core/0.5.1` | Engine package version 0.5.1. |
| `PROFILE_ID` (stream namespace) | `tac-scm-2026-draft2` | Draft 3 keeps draft 2's random-stream namespace (see RULES.md section 15). |
| `AGENT_IDS` | `agent-a` … `agent-f` | The six manufacturer ids; an agent's id is its seat. |

## Calendar and factory

| Engine name | Value | Unit |
|---|---|---|
| `DAYS` | 220 (days 0..219) | days |
| `calendar.first_day` / `calendar.last_day` | 0 / 219 | day |
| `FACTORY_CYCLES` (`factory.cycles_per_day`) | 2,000 | cycles per agent per day |
| `factory.production_lines_max` | 256 | production lines per action set |
| `factory.delivery_lines_max` | 4,096 | delivery lines per action set |

## Components (`COMPONENTS`)

| Component | Base price | Base price (µcr) | Suppliers |
|---|---:|---:|---|
| `cpu-pintel-2` | 1,000 cr | 1,000,000,000 | `pintel` |
| `cpu-pintel-5` | 1,500 cr | 1,500,000,000 | `pintel` |
| `cpu-imd-2` | 1,000 cr | 1,000,000,000 | `imd` |
| `cpu-imd-5` | 1,500 cr | 1,500,000,000 | `imd` |
| `board-pintel` | 250 cr | 250,000,000 | `basus`, `macrostar` |
| `board-imd` | 250 cr | 250,000,000 | `basus`, `macrostar` |
| `memory-1` | 100 cr | 100,000,000 | `mec`, `queenmax` |
| `memory-2` | 200 cr | 200,000,000 | `mec`, `queenmax` |
| `disk-300` | 300 cr | 300,000,000 | `watergate`, `mintor` |
| `disk-500` | 400 cr | 400,000,000 | `watergate`, `mintor` |

Supplier lines (one per supplier and component it sells): 16, namely
`pintel`/`cpu-pintel-2`, `pintel`/`cpu-pintel-5`, `imd`/`cpu-imd-2`,
`imd`/`cpu-imd-5`, and for each of `basus`, `macrostar` both boards, for
each of `mec`, `queenmax` both memories, for each of `watergate`, `mintor`
both disks. CPUs are single-source; boards, memory and disks are two-source.

## Products (`PRODUCTS`)

Each product consumes one unit of each of its four components.

| Product | CPU | Board | Memory | Disk | Cycles per unit | Segment | Nominal value (cr) |
|---|---|---|---|---|---:|---|---:|
| `p01` | `cpu-pintel-2` | `board-pintel` | `memory-1` | `disk-300` | 4 | low | 1,650 |
| `p02` | `cpu-pintel-2` | `board-pintel` | `memory-1` | `disk-500` | 5 | low | 1,750 |
| `p03` | `cpu-pintel-2` | `board-pintel` | `memory-2` | `disk-300` | 5 | mid | 1,750 |
| `p04` | `cpu-pintel-2` | `board-pintel` | `memory-2` | `disk-500` | 6 | mid | 1,850 |
| `p05` | `cpu-pintel-5` | `board-pintel` | `memory-1` | `disk-300` | 5 | mid | 2,150 |
| `p06` | `cpu-pintel-5` | `board-pintel` | `memory-1` | `disk-500` | 6 | high | 2,250 |
| `p07` | `cpu-pintel-5` | `board-pintel` | `memory-2` | `disk-300` | 6 | high | 2,250 |
| `p08` | `cpu-pintel-5` | `board-pintel` | `memory-2` | `disk-500` | 7 | high | 2,350 |
| `p09` | `cpu-imd-2` | `board-imd` | `memory-1` | `disk-300` | 4 | low | 1,650 |
| `p10` | `cpu-imd-2` | `board-imd` | `memory-1` | `disk-500` | 5 | low | 1,750 |
| `p11` | `cpu-imd-2` | `board-imd` | `memory-2` | `disk-300` | 5 | low | 1,750 |
| `p12` | `cpu-imd-2` | `board-imd` | `memory-2` | `disk-500` | 6 | mid | 1,850 |
| `p13` | `cpu-imd-5` | `board-imd` | `memory-1` | `disk-300` | 5 | mid | 2,150 |
| `p14` | `cpu-imd-5` | `board-imd` | `memory-1` | `disk-500` | 6 | mid | 2,250 |
| `p15` | `cpu-imd-5` | `board-imd` | `memory-2` | `disk-300` | 6 | high | 2,250 |
| `p16` | `cpu-imd-5` | `board-imd` | `memory-2` | `disk-500` | 7 | high | 2,350 |

The nominal value is the sum of the four component base prices. Segments:
low = `p01 p02 p09 p10 p11` (5 products), mid = `p03 p04 p05 p12 p13 p14`
(6), high = `p06 p07 p08 p15 p16` (5).

## Customer market

| Engine name | Value | Unit |
|---|---|---|
| *initial demand mean, low and high* (`customer.low_initial_q_nano`, `customer.high_initial_q_nano`) | uniform integer 25..100, sampled per game | RFQs per day (stored in nano-units) |
| *initial demand mean, mid* (`customer.mid_initial_q_nano`) | uniform integer 30..120, sampled per game | RFQs per day |
| *demand mean bounds, low and high* (`Segment.min_nano`, `max_nano`) | 25 .. 100 | RFQs per day |
| *demand mean bounds, mid* | 30 .. 120 | RFQs per day |
| *initial trend* | 1,000,000,000 | ppb (= 1) |
| `customer.trend_min_ppb` | 950,000,000 | ppb (= 0.95) |
| *trend maximum, the walk's clamp* (`customer_trend`) | 1,052,631,579 | ppb (≈ 1/0.95). The bootstrap's `customer.trend_max_ppb` field is 1,050,000,000. |
| *trend step* `epsilon` | uniform integer −10,000,000 .. +10,000,000 | ppb per day |
| `customer.quantity_min` / `quantity_max` | 1 / 20 | PCs per RFQ |
| `customer.lead_min` / `lead_max` | 3 / 12 | days from issue to due day |
| `customer.reserve_percent_min` / `max` | 75 / 125 | percent of nominal value |
| `customer.penalty_percent_min` / `max` | 5 / 15 | percent of order reserve value per day |
| `customer.penalty_cap` | 5 | late penalties; the fifth cancels the order |
| *last issue condition* | due day ≤ 219 | RFQs due later are suppressed |
| *day-0 RFQs* (`ProfileRules::DRAFT3.no_day_zero_customer_rfqs`) | none | |
| *offer window* (`ProfileRules::DRAFT3.issue_day_customer_bids`) | issue day only | |

## Supplier market

| Engine name | Value | Unit |
|---|---|---|
| `supplier.nominal_capacity` | 550 | units per line per day |
| `supplier.initial_capacity_min` / `max` | 357 / 743 | units per day (uniform integer) |
| *capacity noise amplitude* (`supplier_capacity`) | 55 | units |
| *capacity mean reversion* | 1/100 of (550 − capacity) per day | |
| *capacity floor* | 1 | unit per day |
| *expected-capacity recursion* | `expected(h) = round_half_even((99·expected(h−1) + 550) / 100)` | units |
| `supplier.reservation_horizon` | 20 | days ahead at full expected capacity |
| `supplier.reservation_reduction_ppb` | 5,000,000 | ppb per day beyond the horizon (willing factor `(220 − h) / 200`) |
| `supplier.price_discount_ppb` | 500,000,000 | ppb; the price at zero utilisation is half the base price |
| *maximum utilisation* | 2 | price at most 1.5 × base |
| `supplier.single_source_apr_ppb` | 750,000,000 | acceptable purchase ratio, CPU lines |
| `supplier.two_source_apr_ppb` | 450,000,000 | acceptable purchase ratio, all other lines |
| *reputation endowment* | 2,000 | units |
| *reputation daily recovery* | 100 | units per day |
| *reputation maximum* | 1,000,000 | ppm (= 1) |
| `supplier.allocation_exponent` | 3 | |
| `supplier.down_payment_ppb` | 100,000,000 | ppb (= 10% of order value) |
| *RFQ quantity range* | 0 .. 100,000 | units (0 is a price probe) |
| *minimum supplier lead* | due day ≥ d + 2 | for an RFQ sent on day d |
| *latest due day* | 219 | |
| *RFQs per supplier-component pair and day* (`ProfileRules::DRAFT3.supplier_rfq_pair_limit`) | 5 positive + 1 probe | per agent |
| *offer validity* | the day after the RFQ only | |
| *delivered supplier orders* (`ProfileRules::DRAFT3.retire_delivered_supplier_orders`) | reported once with their delivery, then removed | |

## Bank and storage

| Engine name | Value | Unit |
|---|---|---|
| *loan rate range* (`config/loan-rate`) | uniform integer 600..1,200 bps, sampled per game | annual; materialised as bps × 100,000 ppb (6%..12%) |
| `finance.loan_rate_ppb` | sampled value | ppb per year, published in the bootstrap |
| `finance.deposit_rate_ppb` | loan rate ÷ 2 (exact) | ppb per year |
| *storage rate range* (`config/storage-rate`) | uniform integer 2,500..5,000 bps, sampled per game | annual (25%..50%) |
| `finance.storage_rate_ppb` | sampled value | ppb per year |
| *simulated year* (`GAME_DAYS` in `arithmetic.rs`) | 220 | days; a daily rate is the annual rate ÷ 220 |
| *initial cash* | 0 | µcr |
| *credit limit* | none | cash may be negative without limit |

## Reports

| Engine name | Value | Unit |
|---|---|---|
| `customer.report_period` | 20 | days; reports on days 19, 39, …, 219 |
| *daily price report* | every day | min and max winning unit price per product |
| *supplier line capacity in the market report* (`mean_capacity_nano`) | 20-day mean of the actual capacity | whole units per day (the field name notwithstanding) |

## Action set limits and identifiers

| Engine name | Value |
|---|---|
| *agent-supplied id pattern* (`valid_id`) | `[a-z0-9][a-z0-9._-]{0,31}` (1..32 bytes), for every `command_id` and `line_id` |
| *id uniqueness* | all `command_id` and `line_id` values of one action set are distinct (one shared namespace) |
| `factory.production_lines_max` | 256 |
| `factory.delivery_lines_max` | 4,096 |
| *customer offers per RFQ* | 1 per action set |
| *supplier orders per alternative group* | 1 per action set |
| *delivery lines per customer order* (`ProfileRules::DRAFT3.one_delivery_line_per_order`) | 1 per action set; a later line for the same order is rejected (`cardinality`) |

## Arithmetic

| Engine name | Value |
|---|---|
| `arithmetic.money_scale` | 1,000,000 µcr per credit |
| `arithmetic.rate_scale` (`RATE_SCALE`) | 1,000,000,000 ppb |
| `arithmetic.intermediate_bits` | 128 (checked) |
| `arithmetic.rounding_id` | `round-half-even` unless a rule says floor or truncation |

## Runner timing defaults (see `TIMING.md`)

These are runner policy, not game rules; the challenge's values are in
`tournament/SUBMISSION.md`.

| Engine name | Default |
|---|---|
| `OperationalPolicy.action_deadline` (`TAC_SCM_ACTION_DEADLINE_MS`) | 15,000 ms |
| `OperationalPolicy.startup_grace` (`TAC_SCM_STARTUP_GRACE_MS`) | 250 ms |
| `OperationalPolicy.shutdown_grace` (`TAC_SCM_SHUTDOWN_GRACE_MS`) | 100 ms |
| `MAX_FRAME_BYTES` (`max_frame_bytes`) | 2 MiB per agent-to-runner frame |
| `max_frames_per_window` | 16 frames per agent per day |
