# RedAgent-inspired house agent

Id: `redagent-inspired-v1` (`tac_scm_gallery/redagent.py`).

A historical team name identifies a role model only. This agent is
independently written for the 2026 rules: it is not the RedAgent team's
agent, is not endorsed by that team or its institution, and is not claimed to
behave like it.

## Published work followed

- P. W. Keller, F.-O. Duguay and D. Precup. *RedAgent - Winner of TAC SCM
  2003.* ACM SIGecom Exchanges 4(3), 2004.

## Claim level

Publication-inspired. No RedAgent source, parameters or executable behaviour
were available or used; no exact reproduction is claimed.

## Reconstructed versus new

A strategy kernel on the common 2026 completion layer (`agent`, `history`,
`safety`, `runtime`). From the paper: separate internal markets value
components, finished PCs, factory cycles and customer commitments, and the
prices that emerge coordinate procurement, production and sales. New for
2026: three deterministic double-auction rounds per day, reconciliation of
allocations into complete bundles of components and cycles, the blend of
internal and book value, and every limit and constant (margins, supplier
caps, open-unit and inventory bounds, debt floor, endgame cut-offs).

## Learned components

None.

## Known limitations

- Developed under profile `tac-scm-2026-draft2` on an earlier engine release
  and plays draft3 unchanged; its risk limits (32 open customer units, 256
  units of position) are small for the 2026 factory.
- Some of its supplier RFQs (mostly in the first 20 days) ask for delivery
  too soon and are rejected as out of range; harmless.
- The 2003 game did not penalise inventory as the 2026 rules do; historical
  buffers are not copied.
