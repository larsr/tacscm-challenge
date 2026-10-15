# Socrates-inspired house agent

Id: `socrates-inspired-v1` (`tac_scm_gallery/socrates.py`,
`socrates_features.py`, `socrates_model_v1.json`, with the
Botticelli-inspired v1 plumbing of `botticelli.py`).

A historical team name identifies a role model only. This agent is
independently written for the 2026 rules: it is not the Socrates team's
agent, is not endorsed by that team or its institution, and is not claimed to
behave like it.

## Published work followed

- Y. Kovalchuk and M. Fasli. *Evaluating Adaptive Customer Strategies in TAC
  SCM.* AAAI Workshop on Trading Agent Design and Analysis (TADA 2008),
  technical report WS-08-12, 2008.

## Claim level

Publication-inspired, trained; a partial (customer-bidding) reconstruction.
No historical network weights, source or bid data were used.

## Reconstructed versus new

From the paper: separate per-competitor feed-forward networks with thirteen
environmental inputs and seven hidden units predicting competitor prices,
bidding just below the lowest prediction, a twelve-day production lookahead
and a profit filter. New for 2026: because competitors' bids and identities
are not public, each network predicts participation and price from public
next-day price reports, learned in controlled training games with a single
active opponent; participation-weighted mixing of predicted prices with the
reserve; a pooled network for unknown opponents. Procurement, production and
delivery come from the Botticelli-inspired v1 plumbing and the common 2026
completion layer.

## Learned components and training

`socrates_model_v1.json`: eight per-identity networks (for the RedAgent-,
DeepMaize-, PackaTAC-, Crocodile-, PhantAgent-, MinneTAC-, Botticelli- and
NaRC-inspired v1 agents) and one pooled network, 13 inputs, 7 ReLU units, 2
outputs, integer-quantised. Fitted on 48 train-only games, each with a no-bid
collector, one active gallery agent and four passive agents, on engine 0.3.1
under profile `tac-scm-2026-draft2`, before the organiser's agent existed.
Weights never change during a game.

## Known limitations

- Opponent identities reach the agent only through explicit launch labels.
  The kit's runner supplies none, so in the kit it always uses its **pooled**
  network.
- Developed under draft2 on an earlier engine and plays draft3 unchanged.
