# NaRC-inspired house agent

Id: `narc-inspired-v1` (`tac_scm_gallery/narc.py`, with the Botticelli-inspired
v1 plumbing of `botticelli.py`).

A historical team name identifies a role model only. This agent is
independently written for the 2026 rules: it is not the NaRC team's agent, is
not endorsed by that team or its institution, and is not claimed to behave
like it.

## Published work followed

- S. Buffett and N. Scott. *An Algorithm for Procurement in Supply Chain
  Management.* TADA workshop, 2004 (NRC 47455).

## Claim level

Publication-inspired. No historical code, value table, parameters or
competition configuration was used; no exact reproduction is claimed.

## Reconstructed versus new

From the paper: procurement as a Markov decision process over incoming
orders, open quotes, purchase cost, day and inventory, comparing accepting,
requesting and waiting under demand, delivery and price distributions. New
for 2026: per-component decomposition, inventory balance aggregated to
-16..32, a five-day horizon with three fixed price outcomes and delivery
outcomes from public supplier reputation, a 2,048-state budget with a
cheapest-safe fallback, and every reward and bound. Sales, production and
delivery come from the Botticelli-inspired v1 plumbing and the common 2026
completion layer, so that procurement is the agent's distinctive part.

## Learned components

None.

## Known limitations

- In its own evaluation its full-game behaviour matched its never-wait
  ablation: the waiting action is available but rarely chosen.
- Developed under profile `tac-scm-2026-draft2` on an earlier engine release
  and plays draft3 unchanged.
