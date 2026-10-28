"""The bench's chart theme: dark, recessive axes, three categorical hues validated together, every series on every
graph.

The hues are the dataviz reference palette's first three dark steps (blue, orange, aqua): all-pairs CVD
ΔE ≥ 9.4, normal-vision ΔE ≥ 20.9, each ≥ 3:1 on the surface. Each series also has its own marker, so identity is
never colour alone. Text is in text inks, never a series colour.
"""

import matplotlib as mpl

SURFACE = "#1a1a19"
TEXT = "#f0efec"
TEXT_2 = "#c3c2b7"
MUTED = "#898781"
GRID = "#2c2c2a"
AXIS = "#383835"
SERIES = ["#3987e5", "#d95926", "#199e70"]
MARKERS = ["o", "s", "^"]


def apply():
    mpl.rcParams.update(
        {
            "figure.facecolor": SURFACE,
            "axes.facecolor": SURFACE,
            "savefig.facecolor": SURFACE,
            "text.color": TEXT,
            "axes.labelcolor": TEXT_2,
            "axes.titlecolor": TEXT,
            "axes.edgecolor": AXIS,
            "xtick.color": MUTED,
            "ytick.color": MUTED,
            "xtick.labelcolor": TEXT_2,
            "ytick.labelcolor": TEXT_2,
            "axes.grid": True,
            "grid.color": GRID,
            "grid.linewidth": 0.8,
            "axes.axisbelow": True,
            "axes.spines.top": False,
            "axes.spines.right": False,
            "axes.prop_cycle": mpl.cycler(color=SERIES),
            "font.size": 10,
            "axes.titlesize": 12,
            "axes.titleweight": "bold",
            "axes.titlelocation": "left",
            "axes.titlepad": 10,
            "axes.labelsize": 10,
            "legend.fontsize": 9,
            "legend.frameon": False,
            "legend.labelcolor": TEXT_2,
            "lines.linewidth": 2,
            "lines.markersize": 7,
            "figure.dpi": 100,
            "savefig.dpi": 150,
            "savefig.bbox": "tight",
            "savefig.pad_inches": 0.25,
        }
    )


def note(fig, text):
    """A muted footnote under the figure: what was measured and where."""
    fig.text(0.01, -0.02, text, color=MUTED, fontsize=8, ha="left", va="top")
