# CrocodileAgent-inspired house agent

Id: `crocodile-inspired-v1` (`tac_scm_gallery/crocodile.py`).

A historical team name identifies a role model only. This agent is
independently written for the 2026 rules: it is not the CrocodileAgent team's
agent, is not endorsed by that team or its institution, and is not claimed to
behave like it.

## Published work followed

- V. Podobnik, A. Petric and G. Jezic. *An Agent-Based Solution for Dynamic
  Supply Chain Management.* Journal of Universal Computer Science 14(7),
  2008 (doi:10.3217/jucs-014-07-1080).

## Claim level

Publication-inspired. No historical source was inspected or used; no
behavioural equivalence or exact reproduction is claimed.

## Reconstructed versus new

A strategy kernel on the common 2026 completion layer. From the paper:
initial component acquisition, discounted long-horizon purchasing,
demand-responsive short and medium replenishment, distance weighting of dated
supply, inventory and endgame limits, staged sales modes, order-first
production with a little speculative stock. New for 2026: the bootstrap and
six scheduled early long-purchase days, every quantity, price ceiling, cap
and day (wind-down from day 184), and the mode thresholds. The historical
knowledge-base architecture, cross-game learning, customer forecasting and
supplier evaluation are not reconstructed.

## Learned components

None.

## Known limitations

- In its own evaluation most of its value came from responsive
  replenishment; its long-horizon part alone lost money.
- Developed under profile `tac-scm-2026-draft2` on an earlier engine release
  and plays draft3 unchanged; its limits (224 units of position, 32 customer
  units) are small for the 2026 factory.
