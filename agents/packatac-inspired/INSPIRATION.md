# PackaTAC-inspired house agent

Id: `packatac-inspired-v1` (`tac_scm_gallery/packatac.py`).

A historical team name identifies a role model only. This agent is
independently written for the 2026 rules: it is not the PackaTAC team's
agent, is not endorsed by that team or its institution, and is not claimed to
behave like it.

## Published work followed

- E. Dahlgren and P. R. Wurman. *PackaTAC: A Conservative Trading Agent.*
  ACM SIGecom Exchanges 4(3), 2004.

## Claim level

Publication-inspired. No PackaTAC source, binary behaviour or configuration
was used; no exact reproduction is claimed.

## Reconstructed versus new

A strategy kernel on the common 2026 completion layer. From the paper:
reserve component inventory and dated factory capacity for every customer
bid; do not count promised supplier deliveries as bid cover; bound inventory
and back orders; produce earliest-due orders on time; follow public prices
with an adaptive margin; sell down excess and terminal stock. New for 2026:
the whole numeric risk envelope (2-unit component buffer, 48 units of
position, 8 open customer units, 18% margin floor, 75% supplier price cap,
ten-day bid horizon, debt floor) and the liquidation price floor. The
historical initial order, targets and constants, and the documented
liquidation at negative prices, are not reconstructed.

## Learned components

None.

## Known limitations

- Deliberately very conservative: it protects cash at a large opportunity
  cost and earns little. In its own evaluation it was kept as a diagnostic
  agent, not promoted.
- After its initial purchase it waits for non-negative cash before trading;
  under draft3 small holding costs keep its cash just below zero, and in the
  kit's test games it made no customer offer all game.
- Developed under profile `tac-scm-2026-draft2` on an earlier engine release
  and plays draft3 unchanged.
