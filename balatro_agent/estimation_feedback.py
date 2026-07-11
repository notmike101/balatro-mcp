"""Estimation feedback loop - compare controller estimates vs actual scores,
track errors, analyze systemic issues, and suggest fixes."""
from __future__ import annotations

import json
import sqlite3
from collections import defaultdict
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

ESTIMATION_DB_PATH = Path(__file__).resolve().parent.parent / "agent" / "estimation_errors.db"


def _compute_error_pct(estimated: int, actual: int) -> float:
    """Compute percentage error between estimated and actual score."""
    if actual != 0:
        return abs(estimated - actual) / max(1, actual) * 100
    return 0.0 if estimated == 0 else 100.0


@dataclass
class EstimationRecord:
    hand_type: str
    estimated: int
    actual: int
    jokers: list[str] = field(default_factory=list)
    blind_effect: str | None = None
    ante: int = 0
    round_num: int = 0
    blind_name: str = ""
    error_pct: float = 0.0
    timestamp: str = ""
    root_cause: str | None = None

    def __post_init__(self):
        if not self.timestamp:
            self.timestamp = datetime.now(timezone.utc).isoformat()
        self.error_pct = _compute_error_pct(self.estimated, self.actual)


def _ensure_db():
    ESTIMATION_DB_PATH.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(ESTIMATION_DB_PATH))
    conn.execute("""CREATE TABLE IF NOT EXISTS estimation_errors (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        hand_type TEXT, estimated INTEGER, actual INTEGER,
        jokers TEXT, blind_effect TEXT, ante INTEGER,
        round_num INTEGER, blind_name TEXT, error_pct REAL,
        timestamp TEXT, root_cause TEXT
    )""")
    conn.execute("CREATE INDEX IF NOT EXISTS idx_hand_type ON estimation_errors(hand_type)")
    conn.execute("CREATE INDEX IF NOT EXISTS idx_ante ON estimation_errors(ante)")
    conn.execute("CREATE INDEX IF NOT EXISTS idx_error_pct ON estimation_errors(error_pct)")
    conn.execute("CREATE INDEX IF NOT EXISTS idx_root_cause ON estimation_errors(root_cause)")
    conn.commit()
    return conn


def record_estimation_error(hand_type, estimated, actual, jokers=None, blind_effect=None, ante=0, round_num=0, blind_name="", root_cause=None):
    record = EstimationRecord(hand_type=hand_type, estimated=int(estimated), actual=int(actual), jokers=jokers or [], blind_effect=blind_effect, ante=int(ante), round_num=int(round_num), blind_name=blind_name, root_cause=root_cause)
    conn = _ensure_db()
    conn.execute("INSERT INTO estimation_errors (hand_type, estimated, actual, jokers, blind_effect, ante, round_num, blind_name, error_pct, timestamp, root_cause) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)", (record.hand_type, record.estimated, record.actual, json.dumps(record.jokers), record.blind_effect, record.ante, record.round_num, record.blind_name, record.error_pct, record.timestamp, record.root_cause))
    conn.commit()
    conn.close()
    return record


def get_errors(ante=None, hand_type=None, min_error_pct=0.0, limit=100):
    conn = _ensure_db()
    query = "SELECT * FROM estimation_errors WHERE 1=1"
    params = []
    if ante is not None:
        query += " AND ante = ?"
        params.append(ante)
    if hand_type is not None:
        query += " AND hand_type = ?"
        params.append(hand_type)
    if min_error_pct > 0:
        query += " AND error_pct > ?"
        params.append(min_error_pct)
    query += " ORDER BY error_pct DESC LIMIT ?"
    params.append(limit)
    rows = conn.execute(query, params).fetchall()
    conn.close()
    records = []
    for row in rows:
        records.append(EstimationRecord(hand_type=row[1], estimated=row[2], actual=row[3], jokers=json.loads(row[4]) if row[4] else [], blind_effect=row[5], ante=row[6], round_num=row[7], blind_name=row[8], error_pct=row[9], timestamp=row[10], root_cause=row[11]))
    return records


def _auto_detect_root_cause(estimated, actual, hand_type, jokers=None):
    """Heuristic root-cause detection for common estimation errors."""
    if estimated == 0 and actual > 0:
        return "estimated_score is zero - hand may be blocked by blind debuff or no scoring cards"
    if actual == 0 and estimated > 0:
        return "actual is zero - blind may have nullified the hand (e.g. suit removal, no-suit blind)"

    ratio = _compute_error_pct(estimated, actual)
    if ratio <= 5:
        return None

    joker_keys = []
    if jokers:
        for j in jokers:
            if isinstance(j, dict):
                joker_keys.append(j.get("center_key", j.get("name", "")))
            elif isinstance(j, str):
                joker_keys.append(j)

    if "j_vampire" in joker_keys:
        return "Vampire Xmult not modeled - controller uses static config Xmult instead of tracking enhanced cards played"
    if "j_raised_fist" in joker_keys:
        return "Raised Fist may use wrong lowest-rank card - verify cards_for_jokers contains only scoring cards"
    if "j_hanging_chad" in joker_keys:
        return "Hanging Chad +100 chips may be double-counted or missing depending on trigger order"
    if "j_banner" in joker_keys:
        return "Banner chips depend on discards_used this round - verify discards_left is correct"
    if "j_triboulet" in joker_keys:
        return "Triboulet x2 per K/Q may not account for enhanced face cards correctly"
    if "j_mystic_summit" in joker_keys:
        return "Mystic Summit +15 mult requires 0 discards - verify discards_left"

    return f"unmodeled effect - error {ratio:.0f}% on {hand_type}; check seals, blind debuffs, or unhandled jokers"


def compare_hand_estimate(hand_type, estimated, actual, jokers=None, blind_effect=None, ante=0, round_num=0, blind_name="", root_cause=None, threshold_pct=5.0):
    """Thin wrapper: record only if error exceeds threshold_pct. Returns True if recorded."""
    if actual != 0:
        error_pct = _compute_error_pct(estimated, actual)
    else:
        error_pct = 0.0 if estimated == 0 else 100.0
    if error_pct > threshold_pct:
        record_estimation_error(hand_type=hand_type, estimated=estimated, actual=actual, jokers=jokers, blind_effect=blind_effect, ante=ante, round_num=round_num, blind_name=blind_name, root_cause=root_cause)
        return True
    return False


def log_estimation_with_root_cause(hand_type, estimated, actual, jokers=None, blind_effect=None, ante=0, round_num=0, blind_name=""):
    """Auto-detect common root causes and record with that info."""
    root_cause = _auto_detect_root_cause(estimated, actual, hand_type, jokers)
    compare_hand_estimate(hand_type=hand_type, estimated=estimated, actual=actual, jokers=jokers, blind_effect=blind_effect, ante=ante, round_num=round_num, blind_name=blind_name, root_cause=root_cause)
    return root_cause


def analyze_errors(ante=None, hand_type=None, min_error_pct=5.0):
    """Aggregate errors by hand_type and joker, compute avg/max error, identify systemic issues."""
    errors = get_errors(ante=ante, hand_type=hand_type, min_error_pct=min_error_pct)
    if not errors:
        return {"total_errors": 0, "by_hand_type": {}, "by_root_cause": {}, "avg_error_pct": 0.0}

    by_hand_type = defaultdict(list)
    by_root_cause = defaultdict(list)
    all_errors = []

    for err in errors:
        by_hand_type[err.hand_type].append(err)
        if err.root_cause:
            by_root_cause[err.root_cause].append(err)
        all_errors.append(err.error_pct)

    summary = {
        "total_errors": len(errors),
        "avg_error_pct": sum(all_errors) / len(all_errors) if all_errors else 0.0,
        "max_error_pct": max(all_errors) if all_errors else 0.0,
        "min_error_pct": min(all_errors) if all_errors else 0.0,
        "by_hand_type": {},
        "by_root_cause": {},
    }

    for ht, errs in by_hand_type.items():
        epcs = [e.error_pct for e in errs]
        summary["by_hand_type"][ht] = {
            "count": len(errs),
            "avg_error_pct": sum(epcs) / len(epcs),
            "max_error_pct": max(epcs),
            "root_causes": list(set(e.root_cause for e in errs if e.root_cause)),
        }

    for rc, errs in by_root_cause.items():
        epcs = [e.error_pct for e in errs]
        summary["by_root_cause"][rc] = {
            "count": len(errs),
            "avg_error_pct": sum(epcs) / len(epcs),
            "affected_hands": list(set(e.hand_type for e in errs)),
        }

    return summary


def suggest_fixes(analysis=None):
    """Generate actionable fix suggestions from analysis results."""
    if analysis is None:
        analysis = analyze_errors()

    suggestions = []
    by_rc = analysis.get("by_root_cause", {})

    for root_cause, info in by_rc.items():
        suggestion = {"root_cause": root_cause, "count": info["count"], "fix": None}

        if "Vampire" in root_cause:
            suggestion["fix"] = ("controller.py: In joker_score_estimate(), track enhanced cards played "
                "during the hand instead of using static config Xmult. "
                "Count cards with center_key != c_base in the scoring set and "
                "multiply Vampire Xmult by 1.5 per enhanced card.")
        elif "Raised Fist" in root_cause:
            suggestion["fix"] = ("controller.py: In joker_score_estimate(), verify that cards_for_jokers "
                "contains only the SCORING cards (not the full hand). "
                "Find the minimum rank_id among scoring cards and use 2 * min_rank for mult.")
        elif "Hanging Chad" in root_cause:
            suggestion["fix"] = ("controller.py: In joker_score_estimate(), Hanging Chad +100 chips "
                "applies only when the first scoring card is retriggered. "
                "Verify retrigger logic is present in the hand.")
        elif "Banner" in root_cause:
            suggestion["fix"] = ("controller.py: In joker_score_estimate(), Banner chips = extra * discards_used. "
                "Verify discards_used is passed correctly in round_data.")
        elif "Triboulet" in root_cause:
            suggestion["fix"] = ("controller.py: In joker_score_estimate(), Triboulet x2 per K/Q scoring card. "
                "Count face cards (rank_id 12, 13) in scoring cards and apply 2^count.")
        elif "Mystic Summit" in root_cause:
            suggestion["fix"] = ("controller.py: In joker_score_estimate(), Mystic Summit +15 mult "
                "requires discards_left <= 0. Verify round_data.discards_left.")
        elif "seal" in root_cause.lower() or "unmodeled" in root_cause.lower():
            suggestion["fix"] = ("controller.py: Add seal scoring bonuses to card_modifier_score(): "
                "Diamond +5 chips, Red +1 mult, Blue +1 chip per card. "
                "Also check for blind debuff effects that may reduce hand levels.")
        else:
            suggestion["fix"] = f"Investigate: {root_cause}. Check controller.py score_breakdown() and joker_score_estimate()."

        suggestions.append(suggestion)

    return suggestions