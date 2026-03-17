import importlib.util
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


SCRIPTS_DIR = Path(__file__).resolve().parents[1]
REPO_ROOT = SCRIPTS_DIR.parent
RUN_SUITE_PATH = SCRIPTS_DIR / "run_suite.sh"
SIMULATION_SKILL_SCRIPTS_DIR = REPO_ROOT / "skills/simulation-service-tests/scripts"
FALLBACK_PRESETS_PATH = REPO_ROOT / "skills/simulation-service-tests/scripts/presets.py"
CLOUDWATCH_QUERY_PATH = REPO_ROOT / "skills/tycho-cloudwatch-logs/scripts/cw_query.zsh"
DOC_PATHS_WITHOUT_LIVE_PARTIAL_SUCCESS = [
    REPO_ROOT / "README.md",
    REPO_ROOT / "STRESS_TEST_README.md",
    REPO_ROOT / "skills/simulation-service-tests/SKILL.md",
    REPO_ROOT / "skills/simulation-service-tests/references/project.md",
    REPO_ROOT / "skills/simulation-service-tests/references/protocols.md",
    REPO_ROOT / "skills/simulation-service-tests/references/upgrade.md",
    REPO_ROOT / "skills/tycho-cloudwatch-logs/SKILL.md",
    REPO_ROOT / "skills/tycho-cloudwatch-logs/references/queries.md",
]
if str(SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIR))

from coverage_sweep import collect_candidate_protocols, protocol_from_fields, resolve_allowed_statuses
from encode_smoke import (
    DEFAULT_SETTLEMENT_BY_CHAIN,
    DEFAULT_TYCHO_ROUTER_BY_CHAIN,
    default_contract_address,
    resolve_contract_address,
    validate_simulate_meta,
)
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
from simulate_smoke import allows_partial_ladder_prefix, validate_pool_entry

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
    def test_skill_scripts_directory_keeps_only_utilities_and_fallback_presets(self) -> None:
        self.assertTrue(FALLBACK_PRESETS_PATH.is_file())
        actual = {
            path.name for path in SIMULATION_SKILL_SCRIPTS_DIR.iterdir() if path.is_file()
        }
        self.assertEqual(actual, {"memory_diff.py", "presets.py", "run_checks.sh"})

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
        self.assertIn("aerodrome_presence", suites)

    def test_base_aerodrome_presence_suite_matches_expected_pairs(self) -> None:
        self.assertEqual(
            [(pair.token_in, pair.token_out) for pair in suite_pairs("aerodrome_presence", BASE_CHAIN_ID)],
            [
                (
                    "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                    "0x4200000000000000000000000000000000000006",
                ),
                (
                    "0x4200000000000000000000000000000000000006",
                    "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                ),
            ],
        )

    def test_base_lst_suite_is_unavailable_in_repo_and_fallback_presets(self) -> None:
        fallback_presets = load_fallback_presets_module()

        self.assertNotIn("lst", fallback_presets.list_suites(BASE_CHAIN_ID))
        with self.assertRaisesRegex(ValueError, "Unknown suite for chain 8453"):
            suite_pairs("lst", BASE_CHAIN_ID)
        with self.assertRaisesRegex(ValueError, "Unknown suite for chain 8453"):
            fallback_presets.suite_pairs("lst", BASE_CHAIN_ID)

    def test_fallback_presets_match_base_aerodrome_presence_suite(self) -> None:
        fallback_presets = load_fallback_presets_module()

        self.assertIn("aerodrome_presence", fallback_presets.list_suites(BASE_CHAIN_ID))
        self.assertEqual(
            [
                (pair.token_in, pair.token_out)
                for pair in fallback_presets.suite_pairs("aerodrome_presence", BASE_CHAIN_ID)
            ],
            [
                (pair.token_in, pair.token_out)
                for pair in suite_pairs("aerodrome_presence", BASE_CHAIN_ID)
            ],
        )

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
            {"ready", "no_liquidity"},
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


class EncodeSmokeConfigTest(unittest.TestCase):
    def test_default_contract_address_returns_chain_specific_router(self) -> None:
        self.assertEqual(
            default_contract_address(BASE_CHAIN_ID, DEFAULT_TYCHO_ROUTER_BY_CHAIN, "router"),
            "0xea3207778e39EB02D72C9D3c4Eac7E224ac5d369",
        )

    def test_default_contract_address_returns_chain_specific_settlement(self) -> None:
        self.assertEqual(
            default_contract_address(BASE_CHAIN_ID, DEFAULT_SETTLEMENT_BY_CHAIN, "settlement"),
            "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
        )

    def test_resolve_contract_address_prefers_environment_override(self) -> None:
        with tempfile.TemporaryDirectory() as tmp_dir:
            repo = Path(tmp_dir)
            with patch.dict(
                os.environ,
                {"TYCHO_ROUTER_ADDRESS": "0x1111111111111111111111111111111111111111"},
                clear=True,
            ):
                self.assertEqual(
                    resolve_contract_address(
                        repo,
                        "TYCHO_ROUTER_ADDRESS",
                        BASE_CHAIN_ID,
                        DEFAULT_TYCHO_ROUTER_BY_CHAIN,
                        "router",
                    ),
                    "0x1111111111111111111111111111111111111111",
                )

    def test_resolve_contract_address_prefers_dotenv_override(self) -> None:
        with tempfile.TemporaryDirectory() as tmp_dir:
            repo = Path(tmp_dir)
            (repo / ".env").write_text(
                "COW_SETTLEMENT_CONTRACT=0x2222222222222222222222222222222222222222\n"
            )

            with patch.dict(os.environ, {}, clear=True):
                self.assertEqual(
                    resolve_contract_address(
                        repo,
                        "COW_SETTLEMENT_CONTRACT",
                        BASE_CHAIN_ID,
                        DEFAULT_SETTLEMENT_BY_CHAIN,
                        "settlement",
                    ),
                    "0x2222222222222222222222222222222222222222",
                )


class EncodeSmokeMetaValidationTest(unittest.TestCase):
    def test_validate_simulate_meta_accepts_ready_partial_with_partial_kind(self) -> None:
        validate_simulate_meta(
            {
                "status": "ready",
                "result_quality": "partial",
                "partial_kind": "mixed",
                "failures": [],
            },
            label="hop",
            allowed_statuses={"ready"},
            allow_failures=False,
        )

    def test_validate_simulate_meta_rejects_request_level_failure_for_pool_selection(self) -> None:
        with self.assertRaisesRegex(AssertionError, "result_quality"):
            validate_simulate_meta(
                {
                    "status": "ready",
                    "result_quality": "request_level_failure",
                    "failures": [],
                },
                label="hop",
                allowed_statuses={"ready"},
                allow_failures=False,
            )

    def test_resolve_contract_address_parses_exported_and_quoted_dotenv_values(self) -> None:
        with tempfile.TemporaryDirectory() as tmp_dir:
            repo = Path(tmp_dir)
            (repo / ".env").write_text(
                'export TYCHO_ROUTER_ADDRESS="0x3333333333333333333333333333333333333333"\n'
            )

            with patch.dict(os.environ, {}, clear=True):
                self.assertEqual(
                    resolve_contract_address(
                        repo,
                        "TYCHO_ROUTER_ADDRESS",
                        BASE_CHAIN_ID,
                        DEFAULT_TYCHO_ROUTER_BY_CHAIN,
                        "router",
                    ),
                    "0x3333333333333333333333333333333333333333",
                )


class SimulateSmokeValidationTest(unittest.TestCase):
    @staticmethod
    def make_pool_entry(
        amounts_out: list[str],
        gas_used=None,
    ) -> dict:
        return {
            "amounts_out": amounts_out,
            "gas_used": gas_used if gas_used is not None else [21000] * len(amounts_out),
            "gas_in_sell": "123",
            "block_number": 1,
        }

    def test_complete_result_keeps_strict_full_ladder_validation(self) -> None:
        ok, error = validate_pool_entry(
            self.make_pool_entry(["1", "2"]),
            expected_len=3,
            allow_partial_prefix=allows_partial_ladder_prefix("complete", None),
        )

        self.assertFalse(ok)
        self.assertIn("amounts_out length mismatch", error)

    def test_partial_pool_coverage_keeps_strict_full_ladder_validation(self) -> None:
        ok, error = validate_pool_entry(
            self.make_pool_entry(["1", "2"]),
            expected_len=3,
            allow_partial_prefix=allows_partial_ladder_prefix("partial", "pool_coverage"),
        )

        self.assertFalse(ok)
        self.assertIn("amounts_out length mismatch", error)

    def test_partial_amount_ladders_accepts_non_empty_prefix(self) -> None:
        ok, error = validate_pool_entry(
            self.make_pool_entry(["1", "2"]),
            expected_len=3,
            allow_partial_prefix=allows_partial_ladder_prefix("partial", "amount_ladders"),
        )

        self.assertTrue(ok, error)

    def test_partial_mixed_accepts_non_empty_prefix(self) -> None:
        ok, error = validate_pool_entry(
            self.make_pool_entry(["1", "2"]),
            expected_len=3,
            allow_partial_prefix=allows_partial_ladder_prefix("partial", "mixed"),
        )

        self.assertTrue(ok, error)

    def test_validation_rejects_mismatched_gas_length_in_any_mode(self) -> None:
        ok, error = validate_pool_entry(
            self.make_pool_entry(["1", "2"], gas_used=[21000]),
            expected_len=3,
            allow_partial_prefix=allows_partial_ladder_prefix("partial", "amount_ladders"),
        )

        self.assertFalse(ok)
        self.assertIn("gas_used length mismatch", error)

    def test_validation_rejects_empty_prefix_for_partial_amount_ladders(self) -> None:
        ok, error = validate_pool_entry(
            self.make_pool_entry([]),
            expected_len=3,
            allow_partial_prefix=allows_partial_ladder_prefix("partial", "amount_ladders"),
        )

        self.assertFalse(ok)
        self.assertIn("amounts_out length mismatch", error)


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

    def test_base_runner_adds_aerodrome_presence_gate(self) -> None:
        self.assertIn('if [[ "$chain_id" == "8453" ]]; then', self.run_suite_text)
        self.assertIn('echo "Protocol presence checks (Aerodrome Slipstreams)..."', self.run_suite_text)
        self.assertIn('--suite aerodrome_presence \\', self.run_suite_text)
        self.assertIn('--expect-protocols aerodrome_slipstreams \\', self.run_suite_text)
        self.assertIn('coverage_aerodrome_presence.json', self.run_suite_text)

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

    def test_run_suite_remains_strict_without_allow_failures_flag(self) -> None:
        self.assertNotIn("--allow-failures", self.run_suite_text)


class CloudWatchQueryContractTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.cw_query_text = CLOUDWATCH_QUERY_PATH.read_text()

    def test_simulate_successes_filters_quoteable_result_quality(self) -> None:
        self.assertIn(
            'status = "ready" and (result_quality = "complete" or result_quality = "partial")',
            self.cw_query_text,
        )


class DocsContractTest(unittest.TestCase):
    def test_user_facing_docs_do_not_describe_partial_success_as_live_behavior(self) -> None:
        for path in DOC_PATHS_WITHOUT_LIVE_PARTIAL_SUCCESS:
            with self.subTest(path=path):
                self.assertNotIn("partial_success", path.read_text())


if __name__ == "__main__":
    unittest.main()
