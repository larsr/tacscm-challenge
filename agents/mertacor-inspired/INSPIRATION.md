# Mertacor-inspired house agent

Id: `mertacor-inspired-v1` (`tac_scm_gallery/mertacor.py`,
`mertacor_features.py`, `mertacor_model_v1.json`, with the
Botticelli-inspired v1 plumbing of `botticelli.py`).

A historical team name identifies a role model only. This agent is
independently written for the 2026 rules: it is not the Mertacor team's
agent, is not endorsed by that team or its institution, and is not claimed to
behave like it.

## Published work followed

- K. C. Chatzidimitriou, A. L. Symeonidis and P. A. Mitkas. *Policy Search
  through Adaptive Function Approximation for Bidding in TAC SCM.* In
  Agent-Mediated Electronic Commerce and Trading Agent Design and Analysis
  (AMEC/TADA 2012), Springer, 2013 (doi:10.1007/978-3-642-40864-9_2).

## Claim level

Publication-inspired, trained; a partial (customer-bidding) reconstruction.
No historical code, networks, weights or training logs were used.

## Reconstructed versus new

From the paper: bidding decomposed into price selection, logistic acceptance
probability and capacity-aware selection of offers by expected utility per
manufacturing cycle; a control state of won cycles, queued cycles and RFQ
quantity; sixteen price factors from 0.90 to 1.05; a reward on deviations
from one day of factory capacity; policy search over adaptive recurrent
function approximators. New for 2026: a small recurrent reservoir policy
found by bounded structural and weight search (not the paper's full
neuro-evolution), an added margin term in the reward, legal and economic
masks. Procurement, production and delivery come from the
Botticelli-inspired v1 plumbing and the common 2026 completion layer.

## Learned components and training

`mertacor_model_v1.json`: a regularised logistic acceptance model and a
three-unit recurrent policy, integer-quantised. Fitted on 24 train-only games
in which an exploring version of the agent labelled only its own offers by
its own later orders, among gallery agents, on engine 0.3.1 under profile
`tac-scm-2026-draft2`, before the organiser's agent existed. The policy was
trained on observed (not model-predicted) rewards. Weights never change
during a game.

## Known limitations

- The learned policy uses only two of its sixteen price factors (1.00 and
  1.03).
- Developed under draft2 on an earlier engine and plays draft3 unchanged.
