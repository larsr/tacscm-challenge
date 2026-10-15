# TAC SCM 2026 game rules

Profile `tac-scm-2026-draft3`, revision 1, as implemented by engine
`tac-scm-engine-core` 0.5.1. Constants are cited as `NAME` (value); every
constant is listed with its unit in `PROFILE.md`.

The words MUST, MUST NOT and MAY are used in their RFC 2119 sense.

## Contents

1. The game in brief
2. Money, numbers and rounding
3. Agents and what each knows at the start
4. Components, products and suppliers
5. The day cycle
6. The daily observation
7. The daily action
8. Customers: demand, RFQs, offers and orders
9. The factory: production, delivery and inventory
10. Suppliers: capacity, RFQs, offers and orders
11. Supplier reputation
12. The bank
13. Storage cost
14. Reports
15. Randomness
16. The end of the game and the score
17. A worked timeline

## 1. The game in brief

Six manufacturer agents each run a PC factory for 220 days, numbered 0
through 219 (`DAYS` = 220). Each day an agent buys components from eight
suppliers, assembles 16 kinds of PCs, and sells them to customers who issue
requests for quotes (RFQs) in three market segments. Agents compete for the
same customer RFQs and for the same supplier capacity. Customer demand and
supplier capacity vary randomly over the game. Cash starts at zero and may
go negative (a loan, charged interest); a positive balance earns interest;
inventory costs storage.

**The score is the agent's cash balance at the end of the game.**
Higher is better; equal balances are tied. Components and PCs held at the
end are worth nothing.

The game is a deterministic state machine: given its root seed and the six
agents' daily actions, every outcome is fixed. Wall-clock time is not a
game rule; the daily decision deadline is described in `TIMING.md`.

## 2. Money, numbers and rounding

- Money and unit prices are signed 64-bit integers in **microcredits**
  (µcr); 1 credit = 1,000,000 µcr. Every amount in messages is in µcr.
- Days, quantities, cycles and capacities are unsigned integers.
- Rates are integers in **ppb** (1,000,000,000 = 1). Supplier reputation is
  in **ppm** (1,000,000 = 1). Customer demand means are integers in
  nano-units (10^-9).
- Arithmetic is exact with 128-bit intermediates. **Rounding is
  round-half-even** (to the nearest integer, ties to the even integer)
  unless a rule says floor or truncation. No floating point decides any
  outcome.
- `mul_rate(x, r)` = round_half_even(x · r / 1,000,000,000).
- An annual rate is turned into a daily one by dividing by 220 (the
  simulated year is the game's 220 days).

## 3. Agents and what each knows at the start

The six agents have the fixed ids `agent-a` … `agent-f` (`AGENT_IDS`); an
agent's id is its seat in the game and says nothing about who it is.

Before day 0 each agent receives a **game bootstrap** containing its own id
and seat, the roster of six ids, the catalog (components with base prices
and suppliers, products with components, cycles and segment, suppliers with
the components they sell), and the game's public configuration, including:

- this game's sampled annual **loan rate**, **deposit rate** and **storage
  rate** (`finance.loan_rate_ppb`, `finance.deposit_rate_ppb`,
  `finance.storage_rate_ppb`);
- this game's sampled **initial demand mean** of each customer segment
  (`customer.low_initial_q_nano`, `mid_…`, `high_…`);
- fixed constants of the customer and supplier markets (all fixed
  constants are in `PROFILE.md`).

Every agent starts with zero cash, zero inventory, no orders and supplier
reputation at its maximum.

What an agent never learns directly: the other agents' cash, inventory,
offers, bids, orders, schedules or reputation; which agent won an RFQ it did
not win; the current customer demand means after the start; and the
suppliers' daily capacity, inventory and order books. It learns about these
only through the public reports (section 14), through the prices and
quantities suppliers offer it (section 10), and through which of its own
customer offers win.

## 4. Components, products and suppliers

There are ten components and sixteen products (tables in `PROFILE.md`).
Every product consumes exactly one unit of each of its four components: a
CPU, a motherboard, a memory and a disk. Products `p01`–`p08` use Pintel
CPUs and `board-pintel`; `p09`–`p16` use IMD CPUs and `board-imd`. Within a
family the products enumerate slow/fast CPU, 1/2 GB memory and 300/500 GB
disk. Assembling one PC takes 4 to 7 factory cycles depending on the
product. Each product belongs to one customer segment: low (5 products),
mid (6) or high (5).

The **nominal value** of a product is the sum of its four component base
prices (1,650 to 2,350 credits).

There are eight suppliers. `pintel` sells the two Pintel CPUs and `imd` the
two IMD CPUs (single-source components). `basus` and `macrostar` each sell
both motherboards, `mec` and `queenmax` both memories, `watergate` and
`mintor` both disks (two-source components). Each supplier has one
**production line** per component it sells: 16 lines in all. Lines are
independent: each has its own capacity, inventory, order book and, for
each agent, its own reputation account.

## 5. The day cycle

Each day `d` runs these phases in order. Within a phase all six agents are
processed; no phase depends on the order in which messages arrived.

1. **Finance open.** Interest on each agent's opening cash (section 12).
2. **Supplier fulfillment.** On every line: the day's capacity is drawn
   (from day 1), the supplier produces, and supplier orders that are due
   are delivered (section 10.3). Delivered components are *pending*: they
   cannot be used today.
3. **Factory execution.** Each agent's delivery schedule and then its
   production schedule, both submitted on day `d−1`, are executed against
   the inventory available at the start of this phase (section 9). New PCs
   are pending: they cannot be delivered today.
4. **Customer settlement.** In this order: late penalties and
   cancellations; the customers accept the deliveries staged in phase 3;
   payments due today are made; customer offers submitted on day `d−1` are
   awarded; the RFQs of day `d−1` close; the RFQs of day `d` are issued; the
   demand of each segment takes its daily step; on days 19, 39, …, 219 the
   market report is compiled (sections 8 and 14).
5. **Inventory close.** Pending components and PCs become usable
   inventory; storage is charged on the resulting inventory (section 13).
6. **Observation.** Each agent receives its day-`d` observation
   (section 6).
7. **Action window.** Each agent submits one action set for day `d`
   (section 7, `TIMING.md`). An agent that submits nothing valid in time
   gets the empty action set.
8. **Action commit.** In this order: supplier orders are accepted and down
   payments debited; the supplier offers of day `d` expire and reputations
   are updated; the new customer offers, production schedule and delivery
   schedule are recorded; the new supplier RFQs are cleared as one sealed
   batch and their offers prepared for day `d+1` (section 10).

After the action commit of day 219 the game ends with final settlement
(section 16).

**When an action takes effect**

| Submitted on day `d` | Takes effect |
|---|---|
| Customer offer on an RFQ of day `d` | Awarded in the customer settlement of day `d+1`; a won order appears in the day-`d+1` observation. |
| Production line | Executes in the factory phase of day `d+1`; its PCs are usable from the close of day `d+1`. |
| Delivery line | Executes in the factory phase of day `d+1`; the customer accepts it the same day. |
| Supplier RFQ | Offers appear in the day-`d+1` observation and can be ordered only in the action of day `d+1`. |
| Supplier order (on an offer valid on day `d`) | Accepted at the commit of day `d` (down payment debited then); delivered on or after its due day; components usable from the day after delivery. |

## 6. The daily observation

Exactly one observation per agent per day, produced after Inventory close.
It is private to the agent except for the parts marked public. All lists
are complete (no field is omitted because it is empty or zero). Fields:

| Field | Content |
|---|---|
| `observation_id`, `day` | The observation's id (to be quoted in the action) and the day. |
| `state_commitment`, `event_cursor` | Integrity data; no economic meaning. |
| `cash_microcredits` | The agent's cash after Inventory close of day `d`. |
| `ledger_since_previous` | The agent's cash transactions of day `d` (all phases so far) plus those of the previous day's action commit (down payments). Each has id, day, phase, source and destination account, amount, reason (`bank-interest`, `supplier-down-payment`, `supplier-delivery-balance`, `supplier-refund`, `customer-payment`, `late-penalty`, `storage-charge`) and the id of its cause. |
| `component_inventory`, `pc_inventory` | Usable quantity of every component and every product (all 10 and 16 listed, zeros included). |
| `pending_inventory` | Pending components and PCs. Because pending items become usable at Inventory close, before the observation, every pending quantity in an observation is zero. |
| `production_results`, `delivery_results` | The result of every line executed in today's factory phase: line id, `executed` true/false, quantity, and for a failed line a rejection (code, path, detail). |
| `supplier_rfqs` | The agent's supplier RFQs accepted at the previous day's commit (those whose offers are in this observation). Each carries the engine's id for the RFQ in `rfq_id` and the agent's own id in `client_rfq_id`; offers refer to the RFQ by the engine id (`rfq_id`) and to their alternative group by `alternative_group_id`. |
| `supplier_offers` | Every offer to the agent valid today, including probe results and no-offer results (section 10.5). |
| `supplier_orders` | The agent's supplier orders not yet completely delivered, plus those completely delivered today (each listed once more, then removed). Each with quantity, due day, unit price, acceptance ordinal, retained down payment and delivered quantity. |
| `supplier_deliveries` | Supplier deliveries made to the agent today: order, component, quantity, unit price and the invoice amount debited. |
| `supplier_reputations` | The agent's reputation on each of the 16 lines: cumulative offered and purchased quantities and the reputation in ppm (section 11). |
| `customer_rfqs` | **Public.** The customer RFQs issued today, the same for every agent: id, product, quantity, due day, unit reserve price and daily penalty. |
| `new_customer_orders` | Orders the agent won in today's settlement. |
| `active_customer_orders` | All the agent's customer orders still retained: open, delivered and awaiting payment, and those that became paid or cancelled today (each listed once more, then removed). Each has id, RFQ id, product, quantity, due day, unit price, daily penalty, penalties charged so far, status (`open`, `delivered-pending-payment`, `delivered-paid`, `cancelled`), payment status (`not-due`, `due`, `paid`) and delivery day. |
| `customer_settlements` | Today's customer events for the agent: penalties (with count and whether the order was cancelled), accepted deliveries, and payments. |
| `previous_action` | Day, digest and cause (`agent-submission` or a default cause) of the agent's action committed the previous day; `null` on day 0. |
| `daily_price_report` | **Public.** Section 14.1. |
| `market_report` | **Public.** On days 19, 39, …, 219 the 20-day report (section 14.2); otherwise `null`. |
| `default_history` | Every day on which the agent's action was a default, with its cause. |

Rejections of the commands in an action are not in the observation; they
are returned in the `ActionCommitted` message right after the commit
(`TIMING.md`). The observation is sufficient for an agent that has kept no
memory, except for information that only appeared in earlier observations
(earlier RFQs, reports and prices).

## 7. The daily action

An action set answers one observation. It quotes the observation id, the
day, and a revision number (1 or higher; a later revision replaces an
earlier one within the same window). It has exactly five lists; an empty
list means "nothing", and nothing carries over from earlier days:

| List | Each entry |
|---|---|
| `customer_offers` | `command_id`, `rfq_id`, `unit_price` (µcr) |
| `supplier_rfqs` | `command_id`, `rfq_id` (the agent's own name for the RFQ), `supplier_id`, `component_id`, `quantity`, `due_day`, `max_unit_price` (µcr or `null` for no limit) |
| `supplier_orders` | `command_id`, `offer_id` |
| `production_schedule` | `line_id`, `priority`, `product_id`, `quantity` |
| `delivery_schedule` | `line_id`, `priority`, `customer_order_id`, `quantity` |

**Structural rules.** Every `command_id` and `line_id` MUST match
`[a-z0-9][a-z0-9._-]{0,31}`, and all of them in one action set MUST be
distinct (one shared namespace across the five lists). At most 256
production lines and 4,096 delivery lines. Within one action set the
`rfq_id`s of the supplier RFQs MUST be distinct per supplier and component.
Reusing an `rfq_id` for another component of the same supplier is accepted,
but the two RFQs then share one engine RFQ id and their offers form one
alternative group (at most one of them can be ordered). A fresh `rfq_id`
for every RFQ is simplest; reuse on a later day is harmless. An action set
that breaks a structural rule is a malformed action, not a set of rejected
commands: the runner treats it as a malformed message (`TIMING.md`
section 4).

**Command rules.** Within a structurally valid set each command is checked
on its own: an invalid command is rejected with a code (`unknown-reference`,
`expired`, `cardinality`, `out-of-range`, `term-mismatch`,
`later-effect`) and the others still take effect. The rules for each list
are in sections 8.3, 9 and 10.

**The empty (default) action** has all five lists empty: no offers, no
RFQs, no orders, and **no production and no deliveries on the next day**.

**Day 219.** Nothing submitted on day 219 can take effect: customer offers,
supplier RFQs, production lines and delivery lines are rejected with
`later-effect`. (No supplier offer is valid on day 219, so no supplier
order can be placed either.)

## 8. Customers: demand, RFQs, offers and orders

### 8.1 Demand

Each segment `s` ∈ {low, mid, high} has a demand mean `Q` (RFQs per day)
and a trend `T`. At the start `Q` is this game's sampled initial mean (low
and high: uniform integer 25..100; mid: uniform integer 30..120) and
`T` = 1. The mean is bounded: low and high 25 ≤ `Q` ≤ 100, mid
30 ≤ `Q` ≤ 120.

In the customer settlement of each day `d`, for each segment in the order
low, mid, high:

1. The number of RFQs `N` is drawn from a Poisson distribution with mean
   `Q` (exact inverse-CDF sampling). **On day 0 no RFQs are issued** and no
   count is drawn.
2. `N` RFQs are generated (section 8.2).
3. The demand takes its daily step (also on day 0):
   - draw `epsilon` uniform integer in −10,000,000..+10,000,000 ppb;
   - `T' = clamp(T + epsilon, 950,000,000, 1,052,631,579)` ppb;
   - `Q' = round_half_even(Q · T' / 10^9)` (in nano-units);
   - if `Q'` is below the segment's lower bound, set `Q` to the lower
     bound and `T` to 1; if above the upper bound, set `Q` to the upper
     bound and `T` to 1; otherwise `Q = Q'`, `T = T'`.

The bootstrap's `customer.trend_max_ppb` field is 1,050,000,000; the
walk's clamp is 1,052,631,579 ppb (about 1/0.95), as above.

Demand is exogenous: it does not depend on what agents do.

### 8.2 RFQs

RFQ ordinal `i` = 0..N−1 of segment `s` on day `d` has id
`customer-rfq/{d}/{s}/{i}` and these attributes, each drawn independently
and uniformly:

| Attribute | Rule |
|---|---|
| product | one of the segment's products, uniform |
| quantity | integer 1..20 |
| due day | `d` + lead time, lead time integer 3..12 |
| unit reserve price | `floor(nominal_value_µcr · r / 100)`, `r` integer 75..125 |
| daily penalty | `floor(quantity · unit_reserve · p / 100)`, `p` integer 5..15 |

The daily penalty is one amount for the whole order (not per unit). An RFQ
whose due day would be after day 219 is suppressed: no agent sees it. All
other RFQs of day `d` appear, identically for all agents, in the day-`d`
observation. Ordinals count suppressed RFQs too, so the visible ids of a
day can have gaps. Because the lead time is at least 3 days, the last
customer RFQs are issued on day 216 and awarded on day 217.

### 8.3 Offers

An agent bids on an RFQ by a customer offer `(rfq_id, unit_price)` in its
action of the **same day** the RFQ was issued. An offer is always for the
RFQ's full quantity and due day. It is rejected if:

- the action contains more than one offer for the same RFQ: **every** offer
  for that RFQ in the action is rejected (`cardinality`);
- it is day 219 (`later-effect`);
- the RFQ is not one of today's open RFQs (`unknown-reference`); this
  includes any RFQ from an earlier day;
- the unit price is negative or above the RFQ's unit reserve price
  (`out-of-range`).

### 8.4 Award

In the customer settlement of day `d+1`, for each RFQ of day `d` that
received at least one valid offer: **the lowest unit price wins the whole
order.** If several agents offer the same lowest price, the tied offers
are put in a uniformly random order (a random permutation drawn for that
RFQ) and the first of them wins. The winner gets a customer order
`customer-order/{rfq_id}` with the RFQ's product, quantity, due day and
daily penalty and its own unit price; it appears in `new_customer_orders`
of the day-`d+1` observation. Price and penalty never change afterwards.
Losers are not told who won. Then every RFQ of day `d`, awarded or not,
closes.

### 8.5 Delivery, payment, penalties and cancellation

An order is delivered by a delivery line (section 9.2) for its full
quantity; partial deliveries do not exist. A delivery executed in the
factory phase of day `t` is accepted by the customer in the customer
settlement of day `t` (unless the order was cancelled that same moment;
see below). Delivery before the due day is allowed.

**Payment.** The customer pays `quantity · unit_price` once, on the later
of the due day and the delivery day, in that day's customer settlement.
An order delivered early has status `delivered-pending-payment` until its
due day.

**Late penalties.** On each day `t` > due day, in the customer settlement,
an order that is still undelivered at the start of the settlement is
charged its daily penalty. This happens before that day's deliveries are
accepted: an order delivered on day due+1 pays one penalty and is then
paid in full the same day. Penalties are never refunded.

**Cancellation.** The fifth penalty (on day due+5) cancels the order. A
delivery staged for the order on that same day is refused and its PCs
return to the agent's inventory. A cancelled order earns nothing. At most
five penalties are ever charged per order.

A paid or cancelled order is listed in one more observation and then
removed.

## 9. The factory: production, delivery and inventory

Each agent's factory has `FACTORY_CYCLES` (2,000) cycles per day. The
schedules submitted on day `d` replace any earlier schedule and execute in
the factory phase of day `d+1`. Inventory available in that phase is the
usable inventory at the end of day `d`: components delivered by suppliers
on day `d+1` and PCs produced on day `d+1` are not available.

### 9.1 At commit

A production line is rejected at commit if its quantity is zero
(`out-of-range`) or the product does not exist (`unknown-reference`).
A delivery line is checked at commit in this order, and the first check
that fails gives the code: on day 219 every line is rejected
(`later-effect`); its quantity is zero (`out-of-range`); it does not name
one of the agent's open (undelivered, uncancelled) customer orders
(`unknown-reference`); its quantity is not exactly the order's quantity
(`term-mismatch`); **at most one delivery line per customer order:** an
earlier line of the action's `delivery_schedule` list for the same order
was already accepted (`cardinality`), and the accepted line stands.

### 9.2 Delivery schedule

In the factory phase the agent's delivery lines execute first, in
ascending `(priority, line_id)` order. A line executes if the order is
still open with that quantity and the agent has at least that many usable
PCs of the product; it then removes the PCs from inventory and stages the
delivery. Otherwise it fails (`unknown-reference` if the order is no longer
open, `insufficient-inventory` if PCs are short) and changes nothing.

Because an action holds at most one delivery line per order (section 9.1),
an order is delivered at most once a day.

### 9.3 Production schedule

Then the agent's production lines execute, in ascending
`(priority, line_id)` order. A line for `n` PCs needs `n` of each of the
product's four components and `n · cycles(product)` cycles out of what is
left of the day's 2,000. **A line is all-or-nothing:** if components or
cycles are short it fails (`insufficient-capacity` if cycles are short,
otherwise `insufficient-inventory`) and consumes nothing; later lines still
run. An executed line consumes the components and cycles at once; its PCs
become usable at Inventory close the same day. Unused cycles are lost.

### 9.4 Inventory

Usable inventory changes only by: production consuming components;
deliveries removing PCs; a refused delivery returning PCs; Inventory close
adding the day's supplier deliveries and produced PCs. There is no limit
on inventory; it costs storage (section 13).

## 10. Suppliers: capacity, RFQs, offers and orders

### 10.1 Capacity

Each line has an **actual capacity** (units per day). Day 0's is drawn
uniformly from 357..743. On every later day, in Supplier fulfillment, with
`u` drawn uniformly from [0, 1):

`capacity = max(1, trunc(prior + 55 · (u − 1/2) + (550 − prior) / 100))`

where `trunc` rounds toward zero. Capacity therefore wanders around the
nominal 550 with mean reversion. It is exogenous (independent of agents)
and agents never observe it directly, only as a 20-day mean in the market
report.

### 10.2 Production

Each day, after the capacity draw, if the line's outstanding order
quantity (all accepted, undelivered units, whatever their due day) exceeds
its finished inventory, the line produces
`min(capacity, outstanding − inventory)` units into its inventory.
Inventory is shared by all orders on the line. A supplier never produces
for orders it does not have.

### 10.3 Delivery

Then the line's undelivered orders are taken in ascending
`(due_day, acceptance_ordinal)` order, skipping orders not yet due. Each
due order is delivered in full if the line's inventory covers its whole
remaining quantity; **if it does not, delivery on that line stops for the
day** (later orders are not served around it). A late order keeps its
priority. Suppliers never deliver before the due day, never deliver part
of an order during the game, and owe no penalty for lateness.

A delivered order's invoice, `quantity · unit_price − down_payment`, is
debited from the agent's cash at once. The components are pending and
become usable at Inventory close.

### 10.4 RFQs

An agent asks a supplier for a component with a supplier RFQ: supplier,
component, `quantity` (0..100,000; 0 is a **price probe**), `due_day`, and
`max_unit_price` (or `null`). An RFQ submitted on day `d` is rejected if:

- it is day 219 (`later-effect`);
- the supplier does not sell the component (`unknown-reference`);
- the quantity exceeds 100,000, or the due day is before `d+2` or after 219
  (`out-of-range`);
- the maximum price is negative (`out-of-range`; such an RFQ still
  enters the day's batch and appears in the next observation with a `none`
  offer, reason `term-mismatch`; it receives nothing, affects no other RFQ
  and does not count toward the limit below);
- the agent's otherwise valid RFQs to the same supplier and component in
  this action include more than five with positive quantity, or more than
  one probe: then **every** RFQ of the exceeded kind for that pair is
  rejected (`cardinality`). An RFQ rejected for another reason does not
  count.

The accepted RFQs of all six agents are cleared together, line by line, at
the end of the commit of day `d` (sections 10.5–10.8). Clearing uses the
line's state at that moment: actual capacity and inventory of day `d`, and
all orders accepted up to and including day `d`'s commit. RFQs of the same
batch do not affect each other's price.

### 10.5 Planning capacity and the posted price

For a line on day `d`, define for each future day `t`:

- **expected capacity** for horizon `h = t − d`: `expected(0)` is the
  actual capacity of day `d`, and
  `expected(h) = round_half_even((99 · expected(h−1) + 550) / 100)`;
- **willing capacity** on production day `d+h`: `expected(h)` for
  `h ≤ 20`, and `floor(expected(h) · (220 − h) / 200)` for `h > 20`;
- **supply(t)** = the line's finished inventory now + the willing capacity
  of production days `d+2` … `t`;
- **committed(t)** = the undelivered quantity of the line's accepted orders
  due on or before `t`;
- **budget B(t)** = max(0, supply(t) − committed(t)).

The **posted unit price** for due day `t` is

`price(t) = round_half_even(base · (1/2 + u(t)/2))`, with
`u(t) = min(2, committed(t) / supply(t))`, and `price(t)` = 1.5 · base
when `supply(t)` = 0.

It ranges from half the component's base price (nothing committed) to
1.5 times it. Every RFQ of the batch for the same line and due day sees the
same price.

### 10.6 Requested-date allocation

An RFQ with positive quantity whose `max_unit_price` is `null` or at least
the posted price for its due day is **eligible**. Eligible RFQs share the
line's budget by **weighted progressive filling**, one unit at a time:

- an allocation is feasible when, for every day `t`, the total allocated to
  RFQs due on or before `t` is at most `B(t)`;
- while some eligible RFQ `r` with allocation `x_r` below its quantity
  `q_r` can take one more unit feasibly, the unit goes to the one with the
  smallest `(x_r + 1) / (q_r · ρ_r³)`, where `ρ_r` is the agent's
  reputation on the line in ppm (at least 1); fractions are compared
  exactly;
- exact ties go to the earlier due day, then by a uniformly random rank
  of the RFQs drawn once per line and batch (no agent id enters).

Larger requests and better reputation get proportionally more; a
reputation below the maximum is penalised steeply (cube).

### 10.7 Earliest-complete alternatives

An eligible RFQ that received less than it asked for may also get an
**earliest-complete alternative**: an offer for its full quantity at a
later due day. Such RFQs are processed in increasing order of
`(x_r + 1) / (q_r · ρ_r³)` (ties as above). For each, the engine searches
the days after its due day, through 219, for the first day `t'` whose
posted price is within the RFQ's maximum price (if any) and on which the
unallocated remainder fits within the budgets on top of everything already
allocated or reserved in this batch. If found, the remainder is reserved
(for this batch's planning only) and a complete offer is made for the full
quantity, due on `t'`, at the posted price of `t'`. If none is found, there
is no alternative.

All offers made in one batch are jointly feasible if each agent orders at
most one offer per RFQ.

### 10.8 Offers

For each accepted RFQ the agent receives, in the day-`d+1` observation:

| Kind | When | Quantity, due day, price |
|---|---|---|
| `on-date` | allocation > 0 | the allocation (possibly less than requested), the requested due day, the posted price of that day |
| `complete` | an alternative was found (in addition to the on-date or no-offer result) | the full requested quantity, the alternative due day, the posted price of that day |
| `probe` | quantity 0 | 0, the requested due day, the posted price of that day |
| `none` | positive quantity but no allocation | 0, the requested due day, the posted price evaluated; reason `term-mismatch` if the maximum price was below it, otherwise `insufficient-capacity` |

The offers of one RFQ form an **alternative group**. Every offer is valid
only on day `d+1` (`valid_day`) and expires at that day's commit.

### 10.9 Orders and down payment

A supplier order names one offer. It is accepted at the commit of the day
the offer is valid, with exactly the offer's quantity, due day and unit
price. It is rejected if the offer is unknown, expired, not the agent's,
or a probe or no-offer result (`expired`), or if the action orders more
than one offer of the same alternative group (then every order in that
group is rejected, `cardinality`).

On acceptance the agent pays a **down payment** of
`mul_rate(quantity · unit_price, 100,000,000)` (10%, rounded half-even);
the rest is invoiced on delivery. Orders accepted on the same line on the
same day receive consecutive acceptance ordinals in a uniformly random
order; earlier days' orders always have lower ordinals. The ordinal decides
delivery priority among orders with the same due day (section 10.3).

## 11. Supplier reputation

Each agent has a reputation account on each line, with cumulative offered
quantity `O` and purchased quantity `P`, both starting at 0.

When an alternative group expires (at the commit of the day its offers are
valid, after that day's orders are accepted), the account is updated:

- `P` += the quantity ordered from the group (0 if none);
- `O` += the group's **exposure**: for an RFQ that received an `on-date`
  offer of quantity `a`, `max(a, floor(q/5))` where `q` is the requested
  quantity; for a probe, and for an RFQ that received no on-date quantity,
  0; in every case, if the `complete` offer was ordered, at least the
  ordered quantity.

The reputation on day `d` is given by the formula below. The day-`d`
observation shows it with the counts before that day's expiry; the
clearing at the end of the day-`d` commit uses the counts as updated by
that commit's expiry. The formula:

`ρ = min(1,000,000, floor(1,000,000 · (2,000 + P + 100·d) / (2,000 + floor(apr · O / 10^9) + 100·d)))` ppm

with acceptable purchase ratio `apr` = 750,000,000 ppb on the single-source
CPU lines and 450,000,000 ppb on all other lines. An agent that buys at
least `apr` of what it is offered keeps reputation 1,000,000; one that asks
for much and buys little loses allocation weight (section 10.6), recovering
slowly with time.

## 12. The bank

At Finance open of every day, on the opening cash `c` (the balance after
the previous day's commit):

- if `c < 0`: debit `round_half_even(|c| · loan_rate_ppb / (10^9 · 220))`;
- if `c > 0`: credit `round_half_even(c · deposit_rate_ppb / (10^9 · 220))`.

The loan rate is sampled per game uniformly from 600..1,200 basis points
per year (6%–12%); the deposit rate is exactly half of it. Both are in the
bootstrap. There is no credit limit. Interest is simple per day (no other
compounding than the daily balance) and is not charged after day 219.

## 13. Storage cost

At Inventory close of every day, after the day's supplier deliveries and
produced PCs have been added, each agent pays

`round_half_even(V · storage_rate_ppb / (10^9 · 220))`

where `V` is the base value of its usable inventory: each component at its
base price, each PC at its nominal value (sum of its components' base
prices), summed over everything held, in µcr. PCs removed for delivery
that day are not held; PCs returned from a refused delivery are. The
storage rate is sampled per game uniformly from 2,500..5,000 basis points
per year (25%–50%) and is in the bootstrap.

## 14. Reports

### 14.1 Daily price report (every day, public)

For each of the 16 products, the minimum and maximum winning unit price of
the orders awarded in that day's customer settlement (that is, on the RFQs
of the previous day), or `null` for both when no order for the product was
awarded.

### 14.2 Market report (every 20 days, public)

Compiled in the customer settlement of days 19, 39, …, 219 and included in
that day's observation. It covers the window from the day after the
previous report (day 0 for the first) through the report day. The
window's contents are what happened from the customer settlement and
supplier fulfillment of its first day through the customer settlement of
its last day, so supplier orders accepted at the commit of the report day
count in the next window.

| Part | Per | Fields |
|---|---|---|
| `products` | each product | `rfq_count`, `rfq_quantity` (RFQs issued and visible; suppressed RFQs excluded), `order_count`, `order_quantity` (orders awarded), `minimum_price`, `maximum_price`, `mean_price` of winning unit prices |
| `components` | each component | `ordered_quantity` and `mean_unit_price` of supplier orders accepted (all agents, all suppliers), `delivered_quantity` delivered in normal supplier fulfillment |
| `supplier_lines` | each line | `mean_capacity_nano`: the mean of the line's daily actual capacity over the window's days, in **whole units** per day (the field name notwithstanding), rounded half-even. |

Means of prices are quantity-weighted (sum of `quantity · unit_price`
divided by total quantity, rounded half-even once). A mean, minimum or
maximum with nothing to average is `null`.

## 15. Randomness

All randomness comes from named streams derived from the game's 32-byte
root seed (HMAC-SHA-256 key per stream name, ChaCha20 stream, exact
rejection sampling for uniform integers, exact inverse-CDF Poisson). The
root seed is secret during the game; the bootstrap carries only its
SHA-256 commitment. Each
random quantity has its own stream, addressed by name (for example the
count of mid-segment RFQs on day 37, or the quantity of a given RFQ). As a
consequence:

- the loan, deposit and storage rates, the initial demand means, the
  supplier capacities, the demand walk and every customer RFQ are fixed by
  the root seed alone and do not depend on what any agent does;
- the only random draws that interact with agents are the customer-award
  tie draws and the random orders used for exact ties in supplier
  allocation and for same-day supplier order ordinals; these are fair
  (uniform) and do not favour any seat.

Draft 3 uses draft 2's stream namespace, so a root seed produces the same
exogenous market under both profiles.

## 16. The end of the game and the score

Day 219 runs normally through its observation and action window, but
nothing submitted on day 219 takes effect (section 7). After the commit of
day 219, **final settlement**:

1. **Supplier lines.** On each line, the components the supplier has
   already produced (its finished inventory; no further production) are
   delivered to the undelivered orders, grouped by due day in ascending
   order. A group the inventory covers is delivered in full. If the
   inventory cannot cover a group, it is shared among that group's orders
   in proportion to their remaining quantities: each gets the floor of its
   exact share, and the leftover units go one each to the orders with the
   largest fractional remainders (ties by lower acceptance ordinal). No
   later group receives anything. For an order with remaining quantity `q`
   that receives `q_d`:
   - retained down payment = `round_half_even(down_payment · q_d / q)`;
   - refund = `down_payment − retained` (credited);
   - invoice = `q_d · unit_price − retained` (debited);
   - the undelivered part is cancelled.
   Components delivered here are worthless (the game is over) but must be
   paid for.
2. **Customer orders.** Every customer order still open is charged its
   remaining late penalties up to five in total, whether or not it is
   already late, and is cancelled.
3. No interest and no storage are charged.

**Score.** Each agent's score is its cash balance after final settlement,
in µcr. The game result lists the six scores by agent id. Equal scores are
ties; there is no tie-break within a game. How games are aggregated into a
ranking is set in `tournament/FORMAT.md`.

## 17. A worked timeline

An agent wins a mid-segment RFQ for 10 PCs of `p05` and builds them from
components it orders for the purpose. Days are game days.

| Day | What happens |
|---|---|
| 10 | Observation shows RFQ `customer-rfq/10/mid/3`: 10 × `p05`, due 20. The agent offers a unit price ≤ the reserve, and sends RFQs to `pintel` (`cpu-pintel-5`), `basus` (`board-pintel`), `mec` (`memory-1`), `watergate` (`disk-300`), each for 10 units due day 13 (≥ 10 + 2). |
| 11 | Customer settlement awards the RFQ to the lowest offer; the agent's order appears in `new_customer_orders`. Its four supplier offers are in the observation; it orders the four `on-date` offers today (they expire tonight) and pays 10% down at the commit. |
| 13 | Supplier fulfillment delivers the four orders (if the lines' inventory covers them); the invoices are debited; the components are usable from Inventory close of day 13. |
| 13 | The agent schedules a production line of 10 × `p05` (50 cycles). |
| 14 | The line executes; 10 PCs are usable from Inventory close. The agent schedules a delivery line for the order. |
| 15 | The delivery executes and is accepted: status `delivered-pending-payment`. |
| 20 | Due day: the customer pays 10 × unit price. |

Had the delivery first executed on day 21, one daily penalty would have
been charged that day and the payment made the same day; with no delivery
by day 25, five penalties would have been charged and the order cancelled
on day 25.
