"""Framework-neutral building blocks for TAC SCM gallery agents."""

from .agent import GalleryAgent
from .contracts import (
    DecisionExplanation,
    EpisodeDescriptor,
    FeatureVector,
    GalleryKernel,
    KernelDecision,
)
from .history import PublicHistory
from .safety import SafetyProjection, project_safe_action

__all__ = [
    "DecisionExplanation",
    "EpisodeDescriptor",
    "FeatureVector",
    "GalleryAgent",
    "GalleryKernel",
    "KernelDecision",
    "PublicHistory",
    "SafetyProjection",
    "project_safe_action",
]
