"""Public catalog facts of the game (products, suppliers, nominal prices).

The same in every 2026 profile; see rules/PROFILE.md.
"""

from __future__ import annotations


PRODUCTS: dict[str, tuple[tuple[str, str, str, str], int]] = {
    "p01": (("cpu-pintel-2", "board-pintel", "memory-1", "disk-300"), 4),
    "p02": (("cpu-pintel-2", "board-pintel", "memory-1", "disk-500"), 5),
    "p03": (("cpu-pintel-2", "board-pintel", "memory-2", "disk-300"), 5),
    "p04": (("cpu-pintel-2", "board-pintel", "memory-2", "disk-500"), 6),
    "p05": (("cpu-pintel-5", "board-pintel", "memory-1", "disk-300"), 5),
    "p06": (("cpu-pintel-5", "board-pintel", "memory-1", "disk-500"), 6),
    "p07": (("cpu-pintel-5", "board-pintel", "memory-2", "disk-300"), 6),
    "p08": (("cpu-pintel-5", "board-pintel", "memory-2", "disk-500"), 7),
    "p09": (("cpu-imd-2", "board-imd", "memory-1", "disk-300"), 4),
    "p10": (("cpu-imd-2", "board-imd", "memory-1", "disk-500"), 5),
    "p11": (("cpu-imd-2", "board-imd", "memory-2", "disk-300"), 5),
    "p12": (("cpu-imd-2", "board-imd", "memory-2", "disk-500"), 6),
    "p13": (("cpu-imd-5", "board-imd", "memory-1", "disk-300"), 5),
    "p14": (("cpu-imd-5", "board-imd", "memory-1", "disk-500"), 6),
    "p15": (("cpu-imd-5", "board-imd", "memory-2", "disk-300"), 6),
    "p16": (("cpu-imd-5", "board-imd", "memory-2", "disk-500"), 7),
}

SUPPLIERS: dict[str, tuple[str, ...]] = {
    "cpu-pintel-2": ("pintel",), "cpu-pintel-5": ("pintel",),
    "cpu-imd-2": ("imd",), "cpu-imd-5": ("imd",),
    "board-pintel": ("basus", "macrostar"), "board-imd": ("basus", "macrostar"),
    "memory-1": ("mec", "queenmax"), "memory-2": ("mec", "queenmax"),
    "disk-300": ("watergate", "mintor"), "disk-500": ("watergate", "mintor"),
}

BASE_MICROCREDITS = {
    "cpu-pintel-2": 1_000_000_000, "cpu-pintel-5": 1_500_000_000,
    "cpu-imd-2": 1_000_000_000, "cpu-imd-5": 1_500_000_000,
    "board-pintel": 250_000_000, "board-imd": 250_000_000,
    "memory-1": 100_000_000, "memory-2": 200_000_000,
    "disk-300": 300_000_000, "disk-500": 400_000_000,
}
