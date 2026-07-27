"""ultravin — pure-Rust NHTSA vPIC VIN decoder (CLI + library).

The decode logic lives in the compiled `ultravin._ultravin` extension; this
package only re-exports it. No logic here.
"""

from typing import Any

from ultravin import _ultravin
from ultravin._ultravin import (
    __version__,
    cover_vins,
    decode,
    decode_batch,
    decode_batch_json,
    decode_json,
    generate,
    pairwise,
    seeded,
    sweep,
)

#: Variable names whose `flat=True` value is always a list, even at length one —
#: the vPIC elements allowed to repeat within one decode (free-text notes).
MULTI_VALUED: frozenset[str]
#: Static element metadata keyed by variable name: `element_id`, `group_name`,
#: `code`, `data_type`, `decode`. Pin to `element_id` if you need a key that
#: survives NHTSA renaming a variable between data releases.
ELEMENTS: dict[str, dict[str, Any]]

__all__ = [
    "ELEMENTS",
    "MULTI_VALUED",
    "__version__",
    "cover_vins",
    "decode",
    "decode_batch",
    "decode_batch_json",
    "decode_json",
    "generate",
    "pairwise",
    "seeded",
    "sweep",
]


def __getattr__(name: str) -> Any:
    """Build the static tables on first use (PEP 562).

    Both read the embedded archive, and doing that at import time would make a
    bare `import ultravin` pay the artifact load — the cold-start figure in
    docs/BENCHMARKS.md is measured from a fresh process, so it stays lazy.
    """
    if name == "MULTI_VALUED":
        value: Any = frozenset(_ultravin.multi_valued())
    elif name == "ELEMENTS":
        value = {e["variable"]: e for e in _ultravin.elements()}
    else:
        msg = f"module {__name__!r} has no attribute {name!r}"
        raise AttributeError(msg)
    globals()[name] = value  # __getattr__ only runs on a miss, so this caches it
    return value
