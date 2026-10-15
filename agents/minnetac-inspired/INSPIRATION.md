# MinneTAC-inspired house agent

Id: `minnetac-inspired-v1` (`tac_scm_gallery/minnetac.py`).

A historical team name identifies a role model only. This agent is
independently written for the 2026 rules: it is not MinneTAC or a port of
it, is not associated with or endorsed by the MinneTAC team or the University
of Minnesota, and is not claimed to behave like MinneTAC.

## Published work followed

- J. Collins, W. Ketter, M. Gini and A. Agovic. *Software Architecture of the
  MinneTAC Supply-Chain Trading Agent.* Technical report, University of
  Minnesota, 2008 (hdl:11299/215774).
- W. Ketter, E. Kryzhnyaya, S. Damer, C. McMillen, A. Agovic, J. Collins and
  M. Gini. *Analysis and Design of Supply-Driven Strategies in TAC SCM.* TADA
  workshop, 2004.

## Claim level

Behaviour-informed (five bounded findings) and publication-inspired. No exact
reproduction is claimed.

## Disclosure

The agent was written from the papers above and five neutrally worded
behavioural findings:

1. public evaluations shared across decision areas;
2. bundle-aware procurement;
3. margin-ranked, capacity-capped sales with lagged yield feedback;
4. obligation-first, contribution-prioritised production;
5. delivery ranked by avoided economic loss.

These findings were obtained by a bounded static review of a retained 2004
MinneTAC source release, under that release's licence, and worded
neutrally. The implementation was written from the worded findings and the
papers only: it contains no code, expressions,
constants, configuration, class structure or learned data from that release.
The same ideas are largely described in the published papers.

## Reconstructed versus new

A strategy kernel on the common 2026 completion layer. From the papers and
findings: an evaluator graph over public quantities (demand pressure,
component cover, urgent units and factory load, lagged yield, cash headroom,
feasible production) and a selector of market regimes, as in the published
configurable architecture and its bottleneck classification. New for 2026:
the regimes `supply-tight`, `balanced`, `demand-tight` and `endgame` (from
day 190), two-day hysteresis, and every buffer, limit, margin and cap of each
regime.

## Learned components

None.

## Known limitations

- The weakest house agent: its mean in mixed fields was negative in the
  gallery's own league.
- Developed under profile `tac-scm-2026-draft2` on an earlier engine release
  and plays draft3 unchanged.
