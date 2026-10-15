# SouthamptonSCM-inspired house agent

Id: `southampton-inspired-v1` (`tac_scm_gallery/southampton.py`, with the
Botticelli-inspired v1 plumbing of `botticelli.py`).

A historical team name identifies a role model only. This agent is
independently written for the 2026 rules: it is not the SouthamptonSCM
team's agent, is not endorsed by that team or its institution, and is not
claimed to behave like it.

## Published work followed

- M. He, A. Rogers, E. David and N. R. Jennings. *Designing and Evaluating an
  Adaptive Trading Agent for Supply Chain Management Applications.* 2005
  (University of Southampton ePrints 260846).

## Claim level

Publication-inspired. The paper does not publish enough membership functions
or rules for an exact reconstruction; no historical source or parameters were
used.

## Reconstructed versus new

From the paper: fuzzy customer pricing and coordinated qualitative adaptation
of inventory, procurement, factory and delivery decisions. New for 2026:
integer membership functions, minimum conjunction, zero-order Sugeno
weighted-average defuzzification and a compact new rule base mapping public
demand, inventory, cash, urgency, capacity and endgame signals to the sales
margin, procurement buffer and offer capacity. Production, delivery,
supplier selection and safety come from the Botticelli-inspired v1 plumbing
and the common 2026 completion layer.

## Learned components

None.

## Known limitations

- Its fuzzy control changes prices smoothly, but in its own evaluation it
  did not outscore a static midpoint policy in every context.
- Developed under profile `tac-scm-2026-draft2` on an earlier engine release
  and plays draft3 unchanged.
