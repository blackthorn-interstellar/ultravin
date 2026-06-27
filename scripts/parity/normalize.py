"""Canonicalize ultravin + oracle decode output into one comparable shape and diff.

Both producers emit one row per (Element, value) with the same 15 logical columns
(`vpic.spvindecode` returns: groupname, variable, value, itempatternid,
itemvinschemaid, itemkeys, itemelementid, itemattributeid, itemcreatedon,
itemwmiid, code, datatype, decode, itemsource, itemtobeqced). ultravin's Python
dict uses snake_case names and a microsecond-epoch `created_on`. We normalize to
a single dict per row keyed by stable field names, then diff as multisets keyed
by element id.
"""

from __future__ import annotations

from datetime import datetime
from typing import Any

# The 15 spvindecode columns, as canonical field names.
FIELDS = (
    "group_name",
    "variable",
    "value",
    "pattern_id",
    "vin_schema_id",
    "keys",
    "element_id",
    "attribute_id",
    "created_on",
    "wmi_id",
    "code",
    "data_type",
    "decode",
    "source",
    "to_be_qced",
)

# Elements that carry error/correction state rather than vehicle attributes.
ERROR_ELEMENTS = {142, 143, 144, 156, 191}
# Displacement conversions (CI/L derived from CC) — the priority-100 Conversion pass.
CONVERSION_ELEMENTS = {12, 13}
_EPOCH = datetime(1970, 1, 1)  # noqa: DTZ001 (oracle timestamps are naive UTC)


def _ts_to_micros(value: Any) -> int | None:
    """Oracle `timestamp without time zone` (naive, UTC) -> microsecond epoch int."""
    if value is None:
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, datetime):
        delta = value - _EPOCH
        return delta.days * 86_400_000_000 + delta.seconds * 1_000_000 + delta.microseconds
    return value


def _norm(value: Any) -> Any:
    """Treat NULL and empty-string alike, and strip surrounding whitespace on text."""
    if value is None:
        return ""
    if isinstance(value, str):
        return value.strip()
    return value


def from_oracle(row: dict[str, Any]) -> dict[str, Any]:
    """A psycopg dict-row from spvindecode -> canonical row."""
    return {
        "group_name": _norm(row["groupname"]),
        "variable": _norm(row["variable"]),
        "value": _norm(row["value"]),
        "pattern_id": row["itempatternid"],
        "vin_schema_id": row["itemvinschemaid"],
        "keys": _norm(row["itemkeys"]),
        "element_id": row["itemelementid"],
        "attribute_id": _norm(row["itemattributeid"]),
        "created_on": _ts_to_micros(row["itemcreatedon"]),
        "wmi_id": row["itemwmiid"],
        "code": _norm(row["code"]),
        "data_type": _norm(row["datatype"]),
        "decode": _norm(row["decode"]),
        "source": _norm(row["itemsource"]),
        "to_be_qced": bool(row["itemtobeqced"]) if row["itemtobeqced"] is not None else False,
    }


def from_ultravin(elem: dict[str, Any]) -> dict[str, Any]:
    """An ultravin `elements[]` dict -> canonical row."""
    return {
        "group_name": _norm(elem.get("group_name")),
        "variable": _norm(elem.get("variable")),
        "value": _norm(elem.get("value")),
        "pattern_id": elem.get("pattern_id"),
        "vin_schema_id": elem.get("vin_schema_id"),
        "keys": _norm(elem.get("keys")),
        "element_id": elem.get("element_id"),
        "attribute_id": _norm(elem.get("attribute_id")),
        "created_on": _ts_to_micros(elem.get("created_on")),
        "wmi_id": elem.get("wmi_id"),
        "code": _norm(elem.get("code")),
        "data_type": _norm(elem.get("data_type")),
        "decode": _norm(elem.get("decode")),
        "source": _norm(elem.get("source")),
        "to_be_qced": bool(elem.get("to_be_qced")) if elem.get("to_be_qced") is not None else False,
    }


def ultravin_rows(result: dict[str, Any]) -> list[dict[str, Any]]:
    """All canonical rows from an ultravin decode result, in emission order."""
    return [from_ultravin(e) for e in result.get("elements", [])]


# spvindecode's final ORDER BY is *only* this GroupName CASE (no secondary key),
# so rows within one group are returned in Postgres-executor order — a non-spec,
# data-dependent artifact (verified to vary per VIN). Parity therefore checks the
# GroupName-rank ordering, not the unspecified intra-group permutation.
_GROUP_RANK = {
    "": 0,
    "General": 1,
    "Exterior / Body": 2,
    "Exterior / Dimension": 3,
    "Exterior / Truck": 4,
    "Exterior / Trailer": 5,
    "Exterior / Wheel tire": 6,
    "Exterior / Motorcycle": 7,
    "Exterior / Bus": 8,
    "Interior": 9,
    "Interior / Seat": 10,
    "Mechanical / Transmission": 11,
    "Mechanical / Drivetrain": 12,
    "Mechanical / Brake": 13,
    "Mechanical / Battery": 14,
    "Mechanical / Battery / Charger": 15,
    "Engine": 16,
    "Passive Safety System": 17,
    "Passive Safety System / Air Bag Location": 18,
    "Active Safety System": 19,
    "Active Safety System / Maintaining Safe Distance": 20,
    "Active Safety System / Forward Collision Prevention": 21,
    "Active Safety System / Lane and Side Assist": 22,
    "Active Safety System / Backing Up and Parking": 23,
    "Active Safety System / 911 Notification": 24,
    "Active Safety System / Lighting Technologies": 25,
    "Internal": 26,
}


def _group_rank(group_name: Any) -> int:
    return _GROUP_RANK.get(str(group_name), 99)


def _feature(element_id: int | None) -> str:
    if element_id in ERROR_ELEMENTS:
        return "error"
    if element_id in CONVERSION_ELEMENTS:
        return "conversion"
    if element_id == 29:
        return "year"
    return "pattern"


def _by_element(rows: list[dict[str, Any]]) -> dict[Any, list[dict[str, Any]]]:
    out: dict[Any, list[dict[str, Any]]] = {}
    for r in rows:
        out.setdefault(r["element_id"], []).append(r)
    return out


def _sort_key(r: dict[str, Any]) -> tuple[str, str, str]:
    return (str(r["value"]), str(r["attribute_id"]), str(r["pattern_id"]))


def diff_rows(
    oracle: list[dict[str, Any]],
    mine: list[dict[str, Any]],
) -> dict[str, Any]:
    """Field-for-field diff of two canonical row lists.

    Returns a dict with: per-field mismatch list, missing rows (in oracle, not
    ours), extra rows (ours, not oracle's), an ordering flag, and feature tags.
    """
    o_by = _by_element(oracle)
    m_by = _by_element(mine)
    field_diffs: list[dict[str, Any]] = []
    missing: list[dict[str, Any]] = []
    extra: list[dict[str, Any]] = []
    features: dict[str, int] = {}

    def bump(eid: int | None) -> None:
        f = _feature(eid)
        features[f] = features.get(f, 0) + 1

    for eid in sorted(set(o_by) | set(m_by), key=lambda x: (x is None, x)):
        o_rows = sorted(o_by.get(eid, []), key=_sort_key)
        m_rows = sorted(m_by.get(eid, []), key=_sort_key)
        for i in range(max(len(o_rows), len(m_rows))):
            if i >= len(m_rows):
                missing.append(o_rows[i])
                bump(eid)
                continue
            if i >= len(o_rows):
                extra.append(m_rows[i])
                bump(eid)
                continue
            o_r, m_r = o_rows[i], m_rows[i]
            for f in FIELDS:
                if o_r[f] != m_r[f]:
                    field_diffs.append(
                        {
                            "element_id": eid,
                            "field": f,
                            "oracle": o_r[f],
                            "ultravin": m_r[f],
                            "feature": _feature(eid),
                        }
                    )
                    bump(eid)

    # Compare the GroupName-rank sequence (the spec's only ORDER BY key), not the
    # raw row sequence: the intra-group permutation is unspecified Postgres output.
    order_ok = [_group_rank(r["group_name"]) for r in oracle] == [_group_rank(r["group_name"]) for r in mine]
    return {
        "field_diffs": field_diffs,
        "missing": missing,
        "extra": extra,
        "order_ok": order_ok,
        "features": features,
        "ok": not field_diffs and not missing and not extra and order_ok,
    }


def fingerprint(diff: dict[str, Any]) -> dict[str, Any]:
    """A stable, JSON-comparable digest of a diff (for the frozen regression baseline)."""
    return {
        "field_diffs": sorted(
            [fd["element_id"], fd["field"], fd["oracle"], fd["ultravin"]] for fd in diff["field_diffs"]
        ),
        "missing": sorted([r["element_id"], r["value"]] for r in diff["missing"]),
        "extra": sorted([r["element_id"], r["value"]] for r in diff["extra"]),
        "order_ok": diff["order_ok"],
    }
