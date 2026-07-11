from __future__ import annotations

import io
import json
import tempfile
import unittest
from argparse import Namespace
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

from balatro_agent import ALLOWED_SEED, POLICY_SCHEMA
from balatro_agent.controller import build_parser, build_policy_state, classify_cards, cmd_hand_values, score_breakdown
from balatro_agent.ipc import to_lua
from balatro_agent.policy import select_safe_transition
from balatro_agent.reliability import (
    StaleDecisionError,
    add_strategy_rule,
    current_run_from_observation,
    decision_id,
    enhance_policy_state,
    next_decision,
    render_session,
    render_strategy,
    validate_decision_id,
)
from balatro_agent.runtime import SafetyError, guard_command, validate_runtime, validate_seed
from balatro_agent.storage import atomic_write_json, load_json
from balatro_agent.strategy import active_directives, default_strategy, record_evidence
from balatro_agent.scoring import POKER_HAND_KEYS, resolve_hand_key


def card(index: int, rank: int, nominal: int, suit: str) -> dict:
    return {
        "index": index,
        "id": index + 100,
        "base": {"id": rank, "nominal": nominal, "suit": suit, "name": f"{rank} {suit}"},
    }


def poker_hands_contract(source: str = "live_run", valid: bool = True) -> dict:
    base = {
        "Flush Five": (160, 16, 1),
        "Flush House": (140, 14, 2),
        "Five of a Kind": (120, 12, 3),
        "Straight Flush": (100, 8, 4),
        "Four of a Kind": (60, 7, 5),
        "Full House": (40, 4, 6),
        "Flush": (35, 4, 7),
        "Straight": (30, 4, 8),
        "Three of a Kind": (30, 3, 9),
        "Two Pair": (20, 2, 10),
        "Pair": (10, 2, 11),
        "High Card": (5, 1, 12),
    }
    return {
        "schema": "balatro-poker-hand-values/v1",
        "source": source,
        "source_seed": ALLOWED_SEED if source != "menu_defaults" else None,
        "valid_for_scoring": valid,
        "values": {
            key: {
                "key": key,
                "display_name": key,
                "order": order,
                "visible": True,
                "level": 1,
                "chips": chips,
                "mult": mult,
                "base_chips": chips,
                "base_mult": mult,
                "chips_per_level": 1,
                "mult_per_level": 1,
                "played": 0,
                "played_this_round": 0,
                "played_this_ante": 0,
            }
            for key, (chips, mult, order) in base.items()
        },
    }


def observation(state: str = "MENU") -> dict:
    return {
        "bridge": {
            "loaded": True,
            "version": "0.6.0",
            "session_id": "session-test",
            "observation_seq": 7,
            "last_response": {"ok": True, "action": "observe", "response_seq": 2},
        },
        "game": {"state_name": state, "stage_name": "RUN"},
        "ready": {"saved_game_present": True, "saved_game_seed": ALLOWED_SEED},
        "round": {
            "seed": ALLOWED_SEED,
            "ante": 1,
            "round": 1,
            "hands_left": 4,
            "discards_left": 3,
            "dollars": 4,
            "chips": 0,
            "blind_on_deck": "Small",
            "most_played_poker_hand": "High Card",
        },
        "poker_hands": poker_hands_contract(),
        "blind": {"key": "bl_small", "name": "Small Blind", "chips": 300},
        "areas": {"hand": [], "jokers": [], "consumeables": []},
    }


class ReliabilityTests(unittest.TestCase):
    def test_poker_classification_and_score_are_estimates(self) -> None:
        hand = [card(i + 1, rank, min(rank, 10), "Hearts") for i, rank in enumerate([10, 11, 12, 13, 14])]
        self.assertEqual(classify_cards(hand), "Straight Flush")
        breakdown = score_breakdown(hand, poker_hand_values=poker_hands_contract())
        self.assertEqual(breakdown["hand_name"], "Straight Flush")
        self.assertGreater(breakdown["estimated_score"], 0)

    def test_contract_contains_all_game_defined_hands(self) -> None:
        contract = poker_hands_contract()
        self.assertEqual(set(contract["values"]), set(POKER_HAND_KEYS))
        self.assertEqual(len(contract["values"]), 12)

    def test_upgraded_pair_value_flows_into_score(self) -> None:
        contract = poker_hands_contract()
        contract["values"]["Pair"].update({"level": 2, "chips": 25, "mult": 3})
        pair = [card(1, 14, 11, "Spades"), card(2, 14, 11, "Hearts")]
        breakdown = score_breakdown(pair, poker_hand_values=contract)
        self.assertEqual(breakdown["hand_level"], 2)
        self.assertEqual(breakdown["base_chips"], 25)
        self.assertEqual(breakdown["base_mult"], 3)
        self.assertEqual(breakdown["hand_value_source"], "live_run")

    def test_policy_candidate_uses_upgraded_canonical_pair(self) -> None:
        obs = observation("SELECTING_HAND")
        obs["areas"]["hand"] = [card(1, 14, 11, "Spades"), card(2, 14, 11, "Hearts")]
        obs["poker_hands"]["values"]["Pair"].update({"level": 2, "chips": 25, "mult": 3})
        policy = build_policy_state(obs, play_limit=5, discard_limit=0, target_limit=0)
        pair_candidates = [
            candidate for candidate in policy["hand_analysis"]["play_candidates"]
            if candidate["hand_name"] == "Pair"
        ]
        self.assertTrue(pair_candidates)
        self.assertEqual(pair_candidates[0]["score_breakdown"]["base_chips"], 25)
        self.assertEqual(pair_candidates[0]["score_breakdown"]["base_mult"], 3)
        self.assertEqual(policy["poker_hand_values"]["source"], "live_run")

    def test_hand_value_source_validity_is_explicit(self) -> None:
        for source, valid in (("live_run", True), ("saved_run", True), ("menu_defaults", False)):
            with self.subTest(source=source):
                obs = observation()
                obs["poker_hands"] = poker_hands_contract(source, valid)
                base = {"bridge": {}, "game": {}, "run": {}, "legal_actions": [], "hand_analysis": {}}
                with tempfile.TemporaryDirectory() as directory:
                    with patch("balatro_agent.reliability.STRATEGY_STATE_PATH", Path(directory) / "strategy.json"):
                        enhanced = enhance_policy_state(base, obs)
                self.assertEqual(enhanced["estimate_quality"]["hand_values_valid"], valid)
                self.assertEqual(enhanced["estimate_quality"]["hand_value_source"], source)

    def test_advanced_hand_classification(self) -> None:
        flush_five = [card(index, 7, 7, "Hearts") for index in range(1, 6)]
        five_kind = [
            card(index, 7, 7, suit)
            for index, suit in enumerate(["Hearts", "Spades", "Clubs", "Diamonds", "Hearts"], 1)
        ]
        flush_house = [
            card(1, 7, 7, "Hearts"),
            card(2, 7, 7, "Hearts"),
            card(3, 7, 7, "Hearts"),
            card(4, 4, 4, "Hearts"),
            card(5, 4, 4, "Hearts"),
        ]
        self.assertEqual(classify_cards(flush_five), "Flush Five")
        self.assertEqual(classify_cards(five_kind), "Five of a Kind")
        self.assertEqual(classify_cards(flush_house), "Flush House")
        self.assertEqual(resolve_hand_key("Royal Flush"), "Straight Flush")

    def test_missing_or_invalid_values_fail_visibly(self) -> None:
        high = [card(1, 14, 11, "Spades")]
        missing = score_breakdown(high, poker_hand_values=None)
        self.assertFalse(missing["hand_value_available"])
        self.assertIsNone(missing["estimated_score"])
        invalid = score_breakdown(high, poker_hand_values=poker_hands_contract("menu_defaults", False))
        self.assertIn("not valid for scoring", invalid["hand_value_error"])

    def test_compatibility_alias_fixture_matches_canonical_values(self) -> None:
        contract = poker_hands_contract("saved_run", True)
        legacy = {
            key: {
                "chips": value["chips"],
                "mult": value["mult"],
                "level": value["level"],
                "s_chips": value["base_chips"],
                "s_mult": value["base_mult"],
                "l_chips": value["chips_per_level"],
                "l_mult": value["mult_per_level"],
            }
            for key, value in contract["values"].items()
        }
        self.assertEqual(legacy["Pair"]["chips"], contract["values"]["Pair"]["chips"])
        self.assertEqual(legacy["Pair"]["mult"], contract["values"]["Pair"]["mult"])

    def test_hand_values_cli_human_and_json(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "observation.json"
            path.write_text(json.dumps({"poker_hands": poker_hands_contract()}), encoding="utf-8")
            with patch("balatro_agent.controller.OBSERVATION_PATH", path):
                human = io.StringIO()
                with redirect_stdout(human):
                    self.assertEqual(cmd_hand_values(Namespace(hand="Pair", json=False)), 0)
                self.assertIn("Pair: level 1, 10 chips x 2 mult", human.getvalue())
                machine = io.StringIO()
                with redirect_stdout(machine):
                    self.assertEqual(cmd_hand_values(Namespace(hand="Royal Flush", json=True)), 0)
                self.assertEqual(json.loads(machine.getvalue())["value"]["key"], "Straight Flush")
            path.write_text(json.dumps({"poker_hands": poker_hands_contract("menu_defaults", False)}), encoding="utf-8")
            with patch("balatro_agent.controller.OBSERVATION_PATH", path), redirect_stdout(io.StringIO()):
                self.assertEqual(cmd_hand_values(Namespace(hand=None, json=False)), 1)

    def test_seed_guard(self) -> None:
        validate_seed(observation(), ALLOWED_SEED)
        with self.assertRaises(SafetyError):
            validate_seed(observation(), "WRONG")

    def test_saved_run_blocks_new_run_command(self) -> None:
        with patch("balatro_agent.runtime.validate_runtime"):
            with self.assertRaises(SafetyError):
                guard_command({"action": "start_run", "seed": ALLOWED_SEED}, observation())

    def test_saved_run_allows_seedless_resume(self) -> None:
        with patch("balatro_agent.runtime.validate_runtime"):
            guard_command({"action": "start_run"}, observation())

    def test_saved_run_blocks_setup_new_run(self) -> None:
        with patch("balatro_agent.runtime.validate_runtime"):
            with self.assertRaises(SafetyError):
                guard_command({"action": "setup_new_run"}, observation())

    def test_wrong_seed_save_allows_menu_recovery(self) -> None:
        stale = observation()
        stale["round"]["seed"] = "WRONG"
        stale["ready"]["saved_game_seed"] = "WRONG"
        stale["ready"]["current_setup"] = "New Run"
        with patch("balatro_agent.runtime.validate_runtime") as validate:
            guard_command({"action": "ui_click", "ui_id": "main_menu_play"}, stale)
            validate.assert_called_once_with(stale, allow_seed_mismatch=True)
        with patch("balatro_agent.runtime.validate_runtime") as validate:
            guard_command({"action": "setup_new_run"}, stale)
            validate.assert_called_once_with(stale, allow_seed_mismatch=True)
        with patch("balatro_agent.runtime.validate_runtime") as validate:
            guard_command({"action": "start_run", "seed": ALLOWED_SEED}, stale)
            validate.assert_called_once_with(stale, allow_seed_mismatch=True)

    def test_process_and_staleness_guards(self) -> None:
        validate_runtime(observation(), processes=[{"pid": 1}], age_seconds=0.2)
        with self.assertRaises(SafetyError):
            validate_runtime(observation(), processes=[], age_seconds=0.2)
        with self.assertRaises(SafetyError):
            validate_runtime(observation(), processes=[{"pid": 1}], age_seconds=9.0)

    def test_decision_id_rejects_stale_value(self) -> None:
        state = {"decision_id": "dec-current"}
        self.assertEqual(validate_decision_id(state, "dec-current"), "dec-current")
        with self.assertRaises(StaleDecisionError):
            validate_decision_id(state, "dec-old")

    def test_atomic_json_write(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "nested" / "state.json"
            atomic_write_json(path, {"value": 3})
            self.assertEqual(load_json(path), {"value": 3})
            self.assertFalse(path.with_name("state.json.tmp").exists())

    def test_evidence_promotion_and_rejection(self) -> None:
        strategy = default_strategy()
        candidate = {
            "id": "candidate",
            "kind": "strategy",
            "conditions": {"ante": 1},
            "directive": "Test exact condition",
            "absolute": False,
            "status": "candidate",
            "evidence": [],
            "contradictions": [],
        }
        strategy["rules"].append(candidate)
        for number in range(3):
            strategy = record_evidence(strategy, "candidate", "support", f"evt-{number}", "matched")
        promoted = next(rule for rule in strategy["rules"] if rule["id"] == "candidate")
        self.assertEqual(promoted["status"], "high_confidence")
        strategy = record_evidence(strategy, "candidate", "contradict", "evt-x", "failed")
        strategy = record_evidence(strategy, "candidate", "contradict", "evt-y", "failed again")
        rejected = next(rule for rule in strategy["rules"] if rule["id"] == "candidate")
        self.assertEqual(rejected["status"], "rejected")

    def test_new_strategy_rules_require_unique_conditioned_records(self) -> None:
        # Test relies on policies.db being clean - remove test-rule if it exists
        import sqlite3 as _sq
        from balatro_agent.config import POLICIES_DB as _pdb
        conn = _sq.connect(str(_pdb))
        conn.execute("DELETE FROM strategy_rules WHERE rule_id=?", ("test-rule",))
        conn.commit()
        conn.close()
        result = add_strategy_rule("test-rule", "shop", {"ante": 1}, "Buy only when condition matches")
        self.assertEqual(result["rule"]["status"], "candidate")
        with self.assertRaises(ValueError):
            add_strategy_rule("test-rule", "shop", {"ante": 2}, "duplicate")
        # Clean up
        conn = _sq.connect(str(_pdb))
        conn.execute("DELETE FROM strategy_rules WHERE rule_id=?", ("test-rule",))
        conn.commit()
        conn.close()
    def test_strategy_conditions_must_match_live_context(self) -> None:
        strategy = default_strategy()
        strategy["rules"].append(
            {
                "id": "ante-two",
                "status": "supported",
                "conditions": {"ante": 2},
                "directive": "Only active in ante two",
            }
        )
        self.assertNotIn("ante-two", {item["id"] for item in active_directives(strategy, observation())})
        obs = observation()
        obs["round"]["ante"] = 2
        self.assertIn("ante-two", {item["id"] for item in active_directives(strategy, obs)})

    def test_markdown_generation_is_deterministic(self) -> None:
        strategy = default_strategy()
        current = {"ante": 1}
        self.assertEqual(render_strategy(strategy, current), render_strategy(strategy, current))

    def test_fixture_states_have_explicit_next_decisions(self) -> None:
        states = ["MENU", "BLIND_SELECT", "SELECTING_HAND", "ROUND_EVAL", "SHOP", "GAME_OVER", "TAROT_PACK"]
        for state in states:
            with self.subTest(state=state):
                self.assertTrue(next_decision(observation(state)))

    def test_policy_v2_has_stable_decision_and_estimate_labels(self) -> None:
        obs = observation("SELECTING_HAND")
        base = {
            "schema": "v1",
            "bridge": {},
            "game": {"state": "SELECTING_HAND"},
            "run": {"seed": ALLOWED_SEED},
            "legal_actions": [{"id": "play:1", "action": "play", "estimated_score": 10}],
            "hand_analysis": {"best_play": {"estimated_score": 10}, "play_candidates": [{"estimated_score": 10}]},
        }
        with tempfile.TemporaryDirectory() as directory:
            strategy_path = Path(directory) / "strategy.json"
            with patch("balatro_agent.reliability.STRATEGY_STATE_PATH", strategy_path):
                enhanced = enhance_policy_state(base, obs)
        self.assertEqual(enhanced["schema"], POLICY_SCHEMA)
        self.assertTrue(enhanced["decision_id"].startswith("dec-"))
        self.assertEqual(enhanced["legal_actions"][0]["score_kind"], "estimate")
        self.assertEqual(decision_id(enhanced), enhanced["decision_id"])

    def test_replay_reconstructs_same_current_state(self) -> None:
        obs = observation("SHOP")
        with tempfile.TemporaryDirectory() as directory:
            current_path = Path(directory) / "current.json"
            with patch("balatro_agent.reliability.CURRENT_RUN_PATH", current_path), patch(
                "balatro_agent.reliability.observation_age", return_value=0.1
            ):
                first = current_run_from_observation(obs)
                atomic_write_json(current_path, first)
                second = current_run_from_observation(obs)
        for key in ("identity", "status", "ante", "round", "blind", "resources", "build", "next_decision"):
            self.assertEqual(first[key], second[key])

    def test_cli_contract(self) -> None:
        parser = build_parser()
        args = parser.parse_args(["policy-step", "play:1", "--decision-id", "dec-1"])
        self.assertEqual(args.decision_id, "dec-1")
        self.assertEqual(parser.parse_args(["status", "--json"]).cmd, "status")
        self.assertEqual(parser.parse_args(["advance-safe"]).cmd, "advance-safe")
        hand_values = parser.parse_args(["hand-values", "Royal Flush", "--json"])
        self.assertEqual((hand_values.cmd, hand_values.hand, hand_values.json), ("hand-values", "Royal Flush", True))

    def test_ipc_serialization_has_single_return_prefix_owner(self) -> None:
        payload = to_lua({"action": "observe", "id": 1})
        self.assertNotIn("return", payload)
        self.assertIn('"action"', payload)

    def test_safe_policy_stops_at_decisions(self) -> None:
        decision = {"game": {"state": "SHOP"}, "legal_actions": [{"action": "cash_out"}]}
        self.assertIsNone(select_safe_transition(decision))
        transition = {
            "game": {"state": "ROUND_EVAL"},
            "legal_actions": [{"id": "cash_out", "action": "cash_out"}],
        }
        self.assertEqual(select_safe_transition(transition)["id"], "cash_out")


if __name__ == "__main__":
    unittest.main()
