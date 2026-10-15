"""Botticelli v4's acceptance model and DeepMaize v4's forecast parts."""

from __future__ import annotations

import numpy as np

from tac_scm_gallery.botticelli_v4 import _hull, fit_linear_cdf
from tac_scm_gallery.deepmaize_v4_forecast import (
    POINTS,
    AffineCalibration,
    monotone,
    relative_grid,
    relative_index,
    win_curve,
)


def test_linear_cdf_through_min_and_max() -> None:
    a, b = fit_linear_cdf([(1000.0, 1.0), (2000.0, 0.0)])  # type: ignore[misc]
    assert abs(a + b * 1000 - 1) < 1e-9 and abs(a + b * 2000) < 1e-9


def test_linear_cdf_rejects_rising_or_degenerate_fits() -> None:
    assert fit_linear_cdf([(1000.0, 0.0), (2000.0, 1.0)]) is None
    assert fit_linear_cdf([(1000.0, 1.0), (1000.0, 0.0)]) is None


def test_hull_reaches_the_expected_profit_maximum() -> None:
    # win = (2000 - x) / 1000, cost 900, quantity 10, 4 cycles per unit
    options = []
    for x in range(1000, 2001, 50):
        units = 10 * (2000 - x) / 1000
        if units > 0:
            options.append((units * 4, units * (x - 900), x))
    hull = _hull(options)
    assert hull[-1][2] == 1450  # argmax of (2000 - x)(x - 900) on the 50-grid
    efficiencies = [(hull[i][1] - hull[i - 1][1]) / (hull[i][0] - hull[i - 1][0]) for i in range(1, len(hull))]
    assert all(e1 >= e2 for e1, e2 in zip(efficiencies, efficiencies[1:]))


def test_relative_grid_and_index() -> None:
    grid = relative_grid(1000.0, 1200.0)
    assert len(grid) == POINTS and np.all(np.diff(grid) > 0)
    assert grid[20] == 1000.0 and abs(grid[80] - 1200.0) < 1e-9
    assert relative_index(800.0, grid) == -1.0 and relative_index(2000.0, grid) == POINTS
    assert abs(relative_index(1100.0, grid) - 50.0) < 1e-9


def test_win_curve_is_monotone_fraction_above_point() -> None:
    curve = monotone(win_curve(np.array([10.5, 50.5, float(POINTS)])))
    assert curve[0] == 1.0 and abs(curve[30] - 2 / 3) < 1e-9 and abs(curve[99] - 1 / 3) < 1e-9
    assert np.all(np.diff(curve) <= 0)


def test_affine_calibration_moves_toward_outcomes_and_resets() -> None:
    calibration = AffineCalibration()
    for i in range(40):
        calibration.record_bid(f"r{i}", 10, 0.8, 40.0 + i % 10)
    calibration.observe(11, set())  # all lost: predictions were too high
    curve = np.full(POINTS, 0.8)
    adjusted = calibration.apply(curve)
    assert adjusted[45] < 0.8 and adjusted[0] == 0.8 and adjusted[99] <= adjusted[0]
    calibration.observe(30, set())  # outside the window: back to identity
    assert (calibration.a, calibration.c) == (1.0, 0.0)
