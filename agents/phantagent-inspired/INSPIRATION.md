# PhantAgent-inspired house agent

Id: `phantagent-inspired-v1` (`tac_scm_gallery/phantagent.py`).

A historical team name identifies a role model only. This agent is
independently written for the 2026 rules: it is not the PhantAgent team's
agent, is not endorsed by that team or its institution, and is not claimed to
behave like it.

## Published work followed

- A. Sardinha, M. Benisch, N. Sadeh, R. Ravichandran, V. Podobnik and
  M. Stan. *The 2007 Procurement Challenge: A Competition to Evaluate Mixed
  Procurement Strategies.* Technical report CMU-ISRI-07-123, Carnegie Mellon
  University, 2007 (the procurement heuristics described there).
- M. Stan, B. Stan and A. M. Florea. *A Dynamic Strategy Agent for Supply
  Chain Management.* SYNASC 2006 (doi:10.1109/SYNASC.2006.1). Only its
  title and metadata were available; it was not reviewed.

## Claim level

Publication-inspired, **explicitly approximate**. The report supports part of
the procurement kernel; the whole-policy strategy selector is a 2026
hypothesis, not recovered PhantAgent behaviour. No historical source was used.

## Reconstructed versus new

A strategy kernel on the common 2026 completion layer. From the report:
forward projection of dated component inventory, a stage-dependent blend of
prior and recent component use, endgame demand reduction, long and one-off
procurement, comparison of several lead times against recent prices. New
for 2026: three coordinated whole-policy modes (conservative, balanced,
aggressive) and their selector over public signals (cash after commitments,
shortages, factory load, demand, margin opportunities, game stage) with
two-day hysteresis and a conservative endgame from day 190, and every limit
of every mode.

## Learned components

None.

## Known limitations

- Weak and volatile: in the gallery's own league its lower tail was negative.
- Developed under profile `tac-scm-2026-draft2` on an earlier engine release
  and plays draft3 unchanged; its limits are small for the 2026 factory.
