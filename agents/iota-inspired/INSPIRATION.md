# Iota-inspired house agent

Id: `iota-inspired-v1` (`tac_scm_gallery/iota.py`, `iota_features.py`,
`iota_model_v1.json`, with the Botticelli-inspired v1 plumbing of
`botticelli.py`).

A historical team name identifies a role model only. This agent is
independently written for the 2026 rules: it is not the Iota team's agent, is
not endorsed by that team or its institution, and is not claimed to behave
like it.

## Published work followed

- A. Toshniwal, K. Porwal and K. Karlapalem. *Classification Driven Detection
  of Opportunistic Bids in TAC SCM.* AAAI Workshop on Trading Agent Design and
  Analysis (TADA 2013), 2013.

## Claim level

Publication-inspired, train-calibrated; a partial (customer-bidding)
reconstruction. No source, historical thresholds or training labels were
available or used.

## Reconstructed versus new

From the paper: a five-day market window with exponentially weighted public
price ranges, demand classes at 130 and 220 daily RFQs, the decision tree
over profit per assembly cycle, due-date lead and own acceptance rate with
the outcomes Singles, Fours, Sixes and Prune, dynamically adapted thresholds
with delayed updates, class-priority sorting and resource-based demotion.
New for 2026: the initial thresholds and acceptance bands (calibrated, below),
a five-day demand trend in place of the unpublished future-demand model, the
prices of the three bid classes, and the margin, debt and endgame bounds.
Procurement, production and delivery come from the Botticelli-inspired v1
plumbing and the common 2026 completion layer.

## Learned components and training

`iota_model_v1.json`: initial profit-per-cycle thresholds and acceptance
bands, calibrated on 24 train-only games in four roster contexts of gallery
agents, from an exploring version's own offers and outcomes, on engine 0.3.1
under profile `tac-scm-2026-draft2`, before the organiser's agent existed.
The thresholds then adapt during each game by the paper's update rules;
nothing is carried between games.

## Known limitations

- Developed under draft2 on an earlier engine and plays draft3 unchanged.
