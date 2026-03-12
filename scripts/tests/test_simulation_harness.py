import sys
import unittest
from pathlib import Path


SCRIPTS_DIR = Path(__file__).resolve().parents[1]
RUN_SUITE_PATH = SCRIPTS_DIR / "run_suite.sh"
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

from coverage_sweep import collect_candidate_protocols, protocol_from_fields, resolve_allowed_statuses
from presets import (
    RETH_ETH_TARGET_BASE_UNITS,
    TOKENS,
    amounts_for_pair,
    default_amounts_for_token,
    suite_pairs,
)


class PresetsTest(unittest.TestCase):
    def test_amounts_for_pair_uses_pair_override(self) -> None:
        self.assertEqual(
            amounts_for_pair("WETH", "USDC"),
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
        actual = amounts_for_pair("RETH", "ETH")
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
            amounts_for_pair("LINK", "WETH"),
            default_amounts_for_token("LINK"),
        )

    def test_amounts_for_pair_supports_addresses(self) -> None:
        self.assertEqual(
            amounts_for_pair(TOKENS["USDC"], TOKENS["GHO"]),
            ["1000000", "5000000", "10000000", "50000000", "100000000", "500000000"],
        )

    def test_amounts_for_pair_uses_erc4626_pair_overrides(self) -> None:
        self.assertEqual(
            amounts_for_pair("USDC", "SUSDC"),
            ["1000000", "5000000", "10000000", "50000000", "100000000", "500000000"],
        )
        self.assertEqual(
            amounts_for_pair("SUSDS", "USDS"),
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
            [(pair.token_in, pair.token_out) for pair in suite_pairs("erc4626_allowlisted")],
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
            [(pair.token_in, pair.token_out) for pair in suite_pairs("erc4626_negative")],
            [(TOKENS["SUSDE"], TOKENS["USDE"])],
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

    def test_vm_enabled_runs_require_vm_ready_and_pool_count(self) -> None:
        self.assertIn("--require-vm-ready", self.run_suite_text)
        self.assertIn("--require-vm-pools-min 1", self.run_suite_text)

    def test_wait_vm_ready_flag_is_absent_after_hard_cutover(self) -> None:
        self.assertNotIn("--wait-vm-ready", self.run_suite_text)
        self.assertNotIn('wait_vm_ready="', self.run_suite_text)
        self.assertNotIn("Wait VM ready:", self.run_suite_text)


if __name__ == "__main__":
    unittest.main()
