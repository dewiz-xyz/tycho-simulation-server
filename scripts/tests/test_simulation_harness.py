import importlib.util
import os
import sys
import unittest
from pathlib import Path
from unittest.mock import patch


SCRIPTS_DIR = Path(__file__).resolve().parents[1]
REPO_ROOT = SCRIPTS_DIR.parent
RUN_SUITE_PATH = SCRIPTS_DIR / "run_suite.sh"
FALLBACK_PRESETS_PATH = REPO_ROOT / "skills/simulation-service-tests/scripts/presets.py"
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

from coverage_sweep import collect_candidate_protocols, protocol_from_fields, resolve_allowed_statuses
from presets import (
    RETH_ETH_TARGET_BASE_UNITS,
    TOKENS,
    amounts_for_pair,
    default_encode_route,
    default_amounts_for_token,
    list_suites,
    resolve_chain_id,
    suite_pairs,
)

ETHEREUM_CHAIN_ID = 1
BASE_CHAIN_ID = 8453


def load_fallback_presets_module():
    module_name = "simulation_service_tests_fallback_presets"
    spec = importlib.util.spec_from_file_location(module_name, FALLBACK_PRESETS_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Failed to load fallback presets module from {FALLBACK_PRESETS_PATH}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    spec.loader.exec_module(module)
    return module


class PresetsTest(unittest.TestCase):
    def test_amounts_for_pair_uses_pair_override(self) -> None:
        self.assertEqual(
            amounts_for_pair("WETH", "USDC", ETHEREUM_CHAIN_ID),
            [
                "100000000000000000",
                "500000000000000000",
                "1000000000000000000",
                "2000000000000000000",
                "5000000000000000000",
                "10000000000000000000",
            ],
        )

    def test_amounts_for_pair_uses_reth_eth_override(self) -> None:
        actual = amounts_for_pair("RETH", "ETH", ETHEREUM_CHAIN_ID)
        self.assertEqual(
            actual,
            [
                "89285714285714285",
                "446428571428571428",
                "892857142857142857",
                "1785714285714285714",
                "4464285714285714285",
                "8928571428571428571",
                "17857142857142857142",
                "44642857142857142857",
            ],
        )
        self.assertTrue(
            all(int(amount) < target for amount, target in zip(actual, RETH_ETH_TARGET_BASE_UNITS)),
        )

    def test_amounts_for_pair_falls_back_to_token_defaults(self) -> None:
        self.assertEqual(
            amounts_for_pair("LINK", "WETH", ETHEREUM_CHAIN_ID),
            default_amounts_for_token("LINK", ETHEREUM_CHAIN_ID),
        )

    def test_amounts_for_pair_supports_addresses(self) -> None:
        self.assertEqual(
            amounts_for_pair(TOKENS["USDC"], TOKENS["GHO"], ETHEREUM_CHAIN_ID),
            ["1000000", "5000000", "10000000", "50000000", "100000000", "500000000"],
        )

    def test_amounts_for_pair_uses_erc4626_pair_overrides(self) -> None:
        self.assertEqual(
            amounts_for_pair("USDC", "SUSDC", ETHEREUM_CHAIN_ID),
            ["1000000", "5000000", "10000000", "50000000", "100000000", "500000000"],
        )
        self.assertEqual(
            amounts_for_pair("SUSDS", "USDS", ETHEREUM_CHAIN_ID),
            [
                "1000000000000000000",
                "5000000000000000000",
                "10000000000000000000",
                "50000000000000000000",
                "100000000000000000000",
                "500000000000000000000",
            ],
        )

    def test_erc4626_allowlisted_suite_contains_only_supported_pairs(self) -> None:
        self.assertEqual(
            [
                (pair.token_in, pair.token_out)
                for pair in suite_pairs("erc4626_allowlisted", ETHEREUM_CHAIN_ID)
            ],
            [
                (TOKENS["USDS"], TOKENS["SUSDS"]),
                (TOKENS["SUSDS"], TOKENS["USDS"]),
                (TOKENS["USDC"], TOKENS["SUSDC"]),
                (TOKENS["SUSDC"], TOKENS["USDC"]),
                (TOKENS["PYUSD"], TOKENS["SPPYUSD"]),
                (TOKENS["SPPYUSD"], TOKENS["PYUSD"]),
            ],
        )

    def test_erc4626_negative_suite_targets_susde_redeem(self) -> None:
        self.assertEqual(
            [
                (pair.token_in, pair.token_out)
                for pair in suite_pairs("erc4626_negative", ETHEREUM_CHAIN_ID)
            ],
            [(TOKENS["SUSDE"], TOKENS["USDE"])],
        )

    def test_resolve_chain_id_accepts_explicit_base_chain(self) -> None:
        self.assertEqual(resolve_chain_id("8453"), BASE_CHAIN_ID)

    def test_resolve_chain_id_falls_back_to_environment(self) -> None:
        with patch.dict(os.environ, {"CHAIN_ID": "8453"}, clear=True):
            self.assertEqual(resolve_chain_id(None), BASE_CHAIN_ID)

    def test_base_core_suite_matches_expected_pairs(self) -> None:
        self.assertEqual(
            [(pair.token_in, pair.token_out) for pair in suite_pairs("core", BASE_CHAIN_ID)],
            [
                (
                    "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                    "0x4200000000000000000000000000000000000006",
                ),
                (
                    "0x4200000000000000000000000000000000000006",
                    "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                ),
                (
                    "0x0000000000000000000000000000000000000000",
                    "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                ),
                (
                    "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                    "0x0000000000000000000000000000000000000000",
                ),
                (
                    "0x50c5725949a6f0c72e6c4a641f24049a917db0cb",
                    "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                ),
                (
                    "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                    "0x50c5725949a6f0c72e6c4a641f24049a917db0cb",
                ),
            ],
        )

    def test_base_suites_exclude_vm_only_variants(self) -> None:
        suites = list_suites(BASE_CHAIN_ID)

        self.assertNotIn("coverage_core_vm", suites)
        self.assertNotIn("latency_core_vm", suites)
        self.assertNotIn("lst", suites)

    def test_base_lst_suite_is_unavailable_in_repo_and_fallback_presets(self) -> None:
        fallback_presets = load_fallback_presets_module()

        self.assertNotIn("lst", fallback_presets.list_suites(BASE_CHAIN_ID))
        with self.assertRaisesRegex(ValueError, "Unknown suite for chain 8453"):
            suite_pairs("lst", BASE_CHAIN_ID)
        with self.assertRaisesRegex(ValueError, "Unknown suite for chain 8453"):
            fallback_presets.suite_pairs("lst", BASE_CHAIN_ID)

    def test_base_default_encode_route_is_chain_specific(self) -> None:
        self.assertEqual(default_encode_route(BASE_CHAIN_ID), ("USDC", "WETH", "USDC"))

    def test_base_amounts_for_pair_use_base_pair_override(self) -> None:
        self.assertEqual(
            amounts_for_pair("USDC", "WETH", BASE_CHAIN_ID),
            ["1000000", "5000000", "10000000", "50000000", "100000000", "500000000"],
        )

    def test_fallback_presets_match_base_pair_overrides(self) -> None:
        fallback_presets = load_fallback_presets_module()

        self.assertEqual(
            fallback_presets.amounts_for_pair("USDC", "WETH", BASE_CHAIN_ID),
            amounts_for_pair("USDC", "WETH", BASE_CHAIN_ID),
        )
        self.assertEqual(
            fallback_presets.amounts_for_pair("USDC", "ETH", BASE_CHAIN_ID),
            amounts_for_pair("USDC", "ETH", BASE_CHAIN_ID),
        )


class CoverageSweepTest(unittest.TestCase):
    def test_resolve_allowed_statuses_expands_for_relaxed_flags(self) -> None:
        self.assertEqual(
            resolve_allowed_statuses(
                "ready",
                allow_failures=True,
                allow_no_pools=True,
            ),
            {"ready", "partial_success", "no_liquidity"},
        )

    def test_protocol_from_fields_prefers_explicit_protocol(self) -> None:
        self.assertEqual(
            protocol_from_fields("vm:balancer_v2", "3pool"),
            "balancer_v2",
        )

    def test_collect_candidate_protocols_includes_winners_and_pool_results(self) -> None:
        response_json = {
            "data": [
                {
                    "pool_name": "uniswap_v3::WETH/USDC",
                    "protocol": "uniswap_v3",
                }
            ],
            "meta": {
                "pool_results": [
                    {
                        "pool_name": "vm:balancer_v2::USDC/WETH",
                        "protocol": "vm:balancer_v2",
                    },
                    {
                        "pool_name": "fluid_v1::USDC/USDT",
                        "protocol": "fluid_v1",
                    },
                ]
            },
        }

        protocols = collect_candidate_protocols(response_json)

        self.assertEqual(protocols["uniswap_v3"], 1)
        self.assertEqual(protocols["balancer_v2"], 1)
        self.assertEqual(protocols["fluid_v1"], 1)


class RunSuiteContractTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.run_suite_text = RUN_SUITE_PATH.read_text()

    def test_chain_id_env_file_supports_exported_and_quoted_values(self) -> None:
        self.assertIn(
            "^[[:space:]]*(export[[:space:]]+)?CHAIN_ID[[:space:]]*=",
            self.run_suite_text,
        )
        self.assertIn('if [[ "$value" == \\"*\\" ]]; then', self.run_suite_text)
        self.assertIn("elif [[ \"$value\" == \\'*\\' ]]; then", self.run_suite_text)

    def test_vm_enabled_runs_require_vm_ready_and_pool_count(self) -> None:
        self.assertIn("--require-vm-ready", self.run_suite_text)
        self.assertIn("--require-vm-pools-min 1", self.run_suite_text)

    def test_wait_vm_ready_flag_is_absent_after_hard_cutover(self) -> None:
        self.assertNotIn("--wait-vm-ready", self.run_suite_text)
        self.assertNotIn('wait_vm_ready="', self.run_suite_text)
        self.assertNotIn("Wait VM ready:", self.run_suite_text)

    def test_core_suite_vm_remap_is_gated_to_ethereum(self) -> None:
        self.assertIn('if [[ "$suite" == "core" ]] && [[ "$chain_id" == "1" ]]; then', self.run_suite_text)
        self.assertIn('coverage_suite="${COVERAGE_SUITE:-coverage_core_vm}"', self.run_suite_text)
        self.assertIn('latency_suite="${LATENCY_SUITE:-latency_core_vm}"', self.run_suite_text)

    def test_runtime_vm_disabled_path_skips_vm_specific_checks(self) -> None:
        self.assertIn('if [[ "$runtime_vm_enabled" == "true" ]]; then', self.run_suite_text)
        self.assertIn(
            'echo "Skipping VM protocol presence checks (runtime VM is effectively disabled)."',
            self.run_suite_text,
        )

    def test_base_is_not_treated_as_vm_capable(self) -> None:
        self.assertIn('chain_supports_vm="false"', self.run_suite_text)
        self.assertIn('if [[ "$chain_id" == "1" ]]; then', self.run_suite_text)

    def test_balancer_check_guards_empty_coverage_flags(self) -> None:
        self.assertIn(
            '${coverage_flags[@]+"${coverage_flags[@]}"} \\',
            self.run_suite_text,
        )
        self.assertNotIn(
            '"${coverage_flags[@]}" \\\n      --expect-protocols balancer_v2',
            self.run_suite_text,
        )


if __name__ == "__main__":
    unittest.main()
