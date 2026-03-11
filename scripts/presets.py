"""Curated tokens + pair suites for simulation testing."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Tuple


# Ethereum mainnet token addresses (lowercased).
TOKENS: dict[str, str] = {
    "ETH": "0x0000000000000000000000000000000000000000",
    "WETH": "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
    "WBTC": "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599",
    # Stables
    "USDC": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
    "USDT": "0xdac17f958d2ee523a2206206994597c13d831ec7",
    "DAI": "0x6b175474e89094c44da98b954eedeac495271d0f",
    "GHO": "0x40d16fc0246ad3160ccc09b8d0d3a2cd28ae6c2f",
    "FRAX": "0x853d955acef822db058eb8505911ed77f175b99e",
    "LUSD": "0x5f98805a4e8be255a32880fdec7f6728c6568ba0",
    "PYUSD": "0x6c3ea9036406852006290770bedfcaba0e23a0e8",
    "USDS": "0xdc035d45d973e3ec169d2276ddab16f1e407384f",
    "USDE": "0x4c9edd5852cd905f086c759e8383e09bff1e68b3",
    # LSTs
    "STETH": "0xae7ab96520de3a18e5e111b5eaab095312d7fe84",
    "WSTETH": "0x7f39c581f595b53c5cb19bd0b3f8da6c935e2ca0",
    "RETH": "0xae78736cd615f374d3085123a210448e74fc6393",
    "CBETH": "0xbe9895146f7af43049ca1c1ae358b0541ea49704",
    "FRXETH": "0x5e8422345238f34275888049021821e8e08caa1f",
    # ERC4626 vault shares
    "SPPYUSD": "0x80128dbb9f07b93dde62a6daeadb69ed14a7d354",
    "SUSDC": "0xbc65ad17c5c0a2a4d159fa5a503f4992c7b545fe",
    "SUSDS": "0xa3931d71877c0e7a3148cb7eb4463524fec27fbd",
    "SUSDE": "0x9d39a5de30e57443bff2a8307a4256c8797a3497",
    # Bluechips / governance
    "UNI": "0x1f9840a85d5af5bf1d1762f925bdaddc4201f984",
    "LINK": "0x514910771af9ca656af840dff83e8264ecf986ca",
    "AAVE": "0x7fc66500c84a76ad7e9c93437bfc5ac33e2ddae9",
    "COMP": "0xc00e94cb662c3520282e6f5717214004a7f26888",
    "MKR": "0x9f8f72aa9304c8b593d555f12ef6589cc3a579a2",
    "CRV": "0xd533a949740bb3306d119cc777fa900ba034cd52",
    "CVX": "0x4e3fbd56cd56c3e72c1403e103b45db9da5b9d2b",
    "BAL": "0xba100000625a3754423978a60c9317c58a424e3d",
    "SUSHI": "0x6b3595068778dd592e39a122f4f5a5cf09c90fe2",
    "LDO": "0x5a98fcbea516cf06857215779fd812ca3bef1b32",
    "RPL": "0xd33526068d116ce69f19a9ee46f0bd304f21a51f",
    "SNX": "0xc011a73ee8576fb46f5e1c5751ca3b9fe0af2a6f",
    "YFI": "0x0bc529c00c6401aef6d220be8c6ea1667f6ad93e",
    "FXS": "0x3432b6a60d23ca0dfca7761b7ab56459d9c964d0",
}

TOKEN_DECIMALS: dict[str, int] = {
    "USDC": 6,
    "USDT": 6,
    "WBTC": 8,
    "PYUSD": 6,
    "SPPYUSD": 6,
    "SUSDC": 6,
}

ADDRESS_DECIMALS: dict[str, int] = {
    TOKENS[symbol].lower(): decimals for symbol, decimals in TOKEN_DECIMALS.items()
}

ADDRESS_TO_SYMBOL: dict[str, str] = {
    address.lower(): symbol for symbol, address in TOKENS.items()
}

TOKEN_BASE_UNITS: dict[str, list[int]] = {
    # 0.001 .. 2 WBTC (8 decimals) to avoid pathological ladders at huge sizes.
    "WBTC": [
        100_000,
        500_000,
        1_000_000,
        5_000_000,
        10_000_000,
        25_000_000,
        50_000_000,
        100_000_000,
        200_000_000,
    ],
    # 0.1 .. 500 WETH (18 decimals) to avoid VM pool reverts at very large sizes.
    "WETH": [
        100_000_000_000_000_000,
        500_000_000_000_000_000,
        1_000_000_000_000_000_000,
        2_000_000_000_000_000_000,
        5_000_000_000_000_000_000,
        10_000_000_000_000_000_000,
        20_000_000_000_000_000_000,
        50_000_000_000_000_000_000,
        100_000_000_000_000_000_000,
        500_000_000_000_000_000_000,
    ],
    "ETH": [
        100_000_000_000_000_000,
        500_000_000_000_000_000,
        1_000_000_000_000_000_000,
        2_000_000_000_000_000_000,
        5_000_000_000_000_000_000,
        10_000_000_000_000_000_000,
        20_000_000_000_000_000_000,
        50_000_000_000_000_000_000,
        100_000_000_000_000_000_000,
        500_000_000_000_000_000_000,
    ],
}

ADDRESS_BASE_UNITS: dict[str, list[int]] = {
    TOKENS[symbol].lower(): units for symbol, units in TOKEN_BASE_UNITS.items()
}

Pair = Tuple[str, str]


# Keep Rocket Pool coverage sized in ETH notionals, then convert into rETH
# inputs with a slightly rich fixed rate so the ladder stays conservative
# without adding a live RPC dependency to the harness.
RETH_ETH_TARGET_BASE_UNITS: list[int] = [
    100_000_000_000_000_000,
    500_000_000_000_000_000,
    1_000_000_000_000_000_000,
    2_000_000_000_000_000_000,
    5_000_000_000_000_000_000,
    10_000_000_000_000_000_000,
    20_000_000_000_000_000_000,
    50_000_000_000_000_000_000,
]
RETH_ETH_CONSERVATIVE_RATE_BPS = 11_200


def reth_base_units_for_eth_targets(eth_targets: list[int]) -> list[int]:
    return [(target * 10_000) // RETH_ETH_CONSERVATIVE_RATE_BPS for target in eth_targets]


# Some pairs need tighter ladders than token-wide defaults to keep coverage
# deterministic across live mainnet liquidity and VM pool limits.
PAIR_BASE_UNITS: dict[Pair, list[int]] = {
    ("WETH", "USDC"): [
        100_000_000_000_000_000,
        500_000_000_000_000_000,
        1_000_000_000_000_000_000,
        2_000_000_000_000_000_000,
        5_000_000_000_000_000_000,
        10_000_000_000_000_000_000,
    ],
    ("ETH", "USDC"): [
        100_000_000_000_000_000,
        500_000_000_000_000_000,
        1_000_000_000_000_000_000,
        2_000_000_000_000_000_000,
        5_000_000_000_000_000_000,
        10_000_000_000_000_000_000,
    ],
    ("WETH", "DAI"): [
        100_000_000_000_000_000,
        500_000_000_000_000_000,
        1_000_000_000_000_000_000,
        2_000_000_000_000_000_000,
        5_000_000_000_000_000_000,
        10_000_000_000_000_000_000,
    ],
    ("CBETH", "WETH"): [
        10_000_000_000_000_000,
        50_000_000_000_000_000,
        100_000_000_000_000_000,
        500_000_000_000_000_000,
        1_000_000_000_000_000_000,
    ],
    ("ETH", "CBETH"): [
        10_000_000_000_000_000,
        50_000_000_000_000_000,
        100_000_000_000_000_000,
        500_000_000_000_000_000,
        1_000_000_000_000_000_000,
    ],
    # Rocket Pool withdrawals have a much tighter live liquidity ceiling than
    # token-wide 18-decimal defaults; keep coverage/latency on deterministic sizes.
    ("RETH", "ETH"): reth_base_units_for_eth_targets(RETH_ETH_TARGET_BASE_UNITS),
    ("GHO", "USDC"): [
        1_000_000_000_000_000_000,
        5_000_000_000_000_000_000,
        10_000_000_000_000_000_000,
        50_000_000_000_000_000_000,
        100_000_000_000_000_000_000,
        500_000_000_000_000_000_000,
    ],
    ("USDC", "GHO"): [
        1_000_000,
        5_000_000,
        10_000_000,
        50_000_000,
        100_000_000,
        500_000_000,
    ],
    ("USDS", "SUSDS"): [
        1_000_000_000_000_000_000,
        5_000_000_000_000_000_000,
        10_000_000_000_000_000_000,
        50_000_000_000_000_000_000,
        100_000_000_000_000_000_000,
        500_000_000_000_000_000_000,
    ],
    ("SUSDS", "USDS"): [
        1_000_000_000_000_000_000,
        5_000_000_000_000_000_000,
        10_000_000_000_000_000_000,
        50_000_000_000_000_000_000,
        100_000_000_000_000_000_000,
        500_000_000_000_000_000_000,
    ],
    ("USDC", "SUSDC"): [
        1_000_000,
        5_000_000,
        10_000_000,
        50_000_000,
        100_000_000,
        500_000_000,
    ],
    ("SUSDC", "USDC"): [
        1_000_000,
        5_000_000,
        10_000_000,
        50_000_000,
        100_000_000,
        500_000_000,
    ],
    ("PYUSD", "SPPYUSD"): [
        1_000_000,
        5_000_000,
        10_000_000,
        50_000_000,
        100_000_000,
        500_000_000,
    ],
    ("SPPYUSD", "PYUSD"): [
        1_000_000,
        5_000_000,
        10_000_000,
        50_000_000,
        100_000_000,
        500_000_000,
    ],
    ("SUSHI", "WETH"): [
        10_000_000_000_000_000_000,
        50_000_000_000_000_000_000,
        100_000_000_000_000_000_000,
        500_000_000_000_000_000_000,
        1_000_000_000_000_000_000_000,
        5_000_000_000_000_000_000_000,
        10_000_000_000_000_000_000_000,
    ],
}

BASE_AMOUNTS: list[int] = [
    1,
    5,
    10,
    50,
    100,
    500,
    1_000,
    5_000,
    10_000,
    50_000,
]

# Keep the default coverage suite on pairs that stay liquid in both native-only
# and VM-enabled runs. The VM-specific Maverick check lives in run_suite.sh.
CORE_PAIRS: list[Pair] = [
    ("DAI", "USDC"),
    ("USDC", "USDT"),
    ("WETH", "USDC"),
    ("WETH", "USDT"),
    ("WETH", "DAI"),
    ("WBTC", "USDC"),
    ("WBTC", "WETH"),
    ("FRAX", "USDC"),
    ("STETH", "WETH"),
    ("WSTETH", "WETH"),
    ("RETH", "WETH"),
    ("ETH", "RETH"),
    ("RETH", "ETH"),
    ("UNI", "WETH"),
    ("LINK", "WETH"),
    ("AAVE", "WETH"),
    ("COMP", "WETH"),
    ("MKR", "WETH"),
    ("LDO", "WETH"),
]

LATENCY_CORE_PAIRS: list[Pair] = [
    ("DAI", "USDC"),
    ("WBTC", "USDC"),
    ("WBTC", "WETH"),
    ("STETH", "WETH"),
    ("ETH", "RETH"),
    ("RETH", "ETH"),
    ("LINK", "WETH"),
    ("LDO", "WETH"),
]

COVERAGE_CORE_VM_PAIRS: list[Pair] = [
    ("DAI", "USDC"),
    ("USDC", "USDT"),
    ("WBTC", "USDC"),
    ("FRAX", "USDC"),
    ("STETH", "WETH"),
    ("WSTETH", "WETH"),
    ("RETH", "WETH"),
    ("ETH", "RETH"),
    ("RETH", "ETH"),
    ("UNI", "WETH"),
    ("LINK", "WETH"),
    ("AAVE", "WETH"),
    ("COMP", "WETH"),
    ("MKR", "WETH"),
    ("LDO", "WETH"),
]

LATENCY_CORE_VM_PAIRS: list[Pair] = [
    ("DAI", "USDC"),
    ("WBTC", "USDC"),
    ("WBTC", "WETH"),
    ("STETH", "WETH"),
    ("ETH", "RETH"),
    ("RETH", "ETH"),
    ("LINK", "WETH"),
    ("LDO", "WETH"),
]

LOW_LIQUIDITY_PAIRS: list[Pair] = [
    ("LUSD", "USDC"),
    ("FRXETH", "WETH"),
    ("CRV", "WETH"),
    ("CVX", "WETH"),
    ("BAL", "WETH"),
    ("RPL", "WETH"),
    ("SNX", "WETH"),
    ("YFI", "WETH"),
    ("FXS", "WETH"),
]

PAIR_SUITES: dict[str, list[Pair]] = {
    "smoke": [
        ("DAI", "USDC"),
        ("USDC", "USDT"),
        ("STETH", "WETH"),
        ("LINK", "WETH"),
        ("LDO", "WETH"),
    ],
    "core": CORE_PAIRS,
    "latency_core": LATENCY_CORE_PAIRS,
    "coverage_core_vm": COVERAGE_CORE_VM_PAIRS,
    "latency_core_vm": LATENCY_CORE_VM_PAIRS,
    "extended": CORE_PAIRS + LOW_LIQUIDITY_PAIRS,
    "stables": [
        ("DAI", "USDC"),
        ("DAI", "USDT"),
        ("USDC", "USDT"),
        ("FRAX", "USDC"),
    ],
    "lst": [
        ("STETH", "WETH"),
        ("WSTETH", "WETH"),
        ("RETH", "WETH"),
    ],
    "governance": [
        ("UNI", "WETH"),
        ("LINK", "WETH"),
        ("AAVE", "WETH"),
        ("COMP", "WETH"),
        ("MKR", "WETH"),
        ("LDO", "WETH"),
    ],
    "v4_candidates": [
        ("WETH", "USDC"),
        ("WBTC", "USDC"),
        ("ETH", "USDC"),
    ],
    "exploratory_protocols": [
        ("GHO", "USDC"),
        ("USDC", "GHO"),
        ("CBETH", "WETH"),
        ("ETH", "CBETH"),
        ("SUSHI", "WETH"),
    ],
    "erc4626_allowlisted": [
        ("USDS", "SUSDS"),
        ("SUSDS", "USDS"),
        ("USDC", "SUSDC"),
        ("SUSDC", "USDC"),
        ("PYUSD", "SPPYUSD"),
        ("SPPYUSD", "PYUSD"),
    ],
    "erc4626_negative": [
        ("SUSDE", "USDE"),
    ],
}


@dataclass(frozen=True)
class ResolvedPair:
    token_in: str
    token_out: str


def resolve_token(token_or_symbol: str) -> str:
    token_or_symbol = token_or_symbol.strip()
    if token_or_symbol.lower().startswith("0x"):
        return token_or_symbol.lower()

    symbol = token_or_symbol.upper()
    address = TOKENS.get(symbol)
    if not address:
        raise ValueError(f"Unknown token symbol: {token_or_symbol}")
    return address


def default_amounts(decimals: int) -> list[str]:
    scale = 10 ** decimals
    return [str(amount * scale) for amount in BASE_AMOUNTS]


def token_decimals(token_or_symbol: str) -> int:
    token_or_symbol = token_or_symbol.strip()
    if token_or_symbol.lower().startswith("0x"):
        return ADDRESS_DECIMALS.get(token_or_symbol.lower(), 18)
    symbol = token_or_symbol.upper()
    return TOKEN_DECIMALS.get(symbol, 18)


def default_amounts_for_token(token_or_symbol: str) -> list[str]:
    token_or_symbol = token_or_symbol.strip()
    if token_or_symbol.lower().startswith("0x"):
        base_units = ADDRESS_BASE_UNITS.get(token_or_symbol.lower())
    else:
        base_units = TOKEN_BASE_UNITS.get(token_or_symbol.upper())
    if base_units is not None:
        return [str(value) for value in base_units]
    return default_amounts(token_decimals(token_or_symbol))


def amounts_for_pair(token_in: str, token_out: str) -> list[str]:
    def normalize_pair_token(token: str) -> str:
        normalized = token.strip()
        if normalized.lower().startswith("0x"):
            return ADDRESS_TO_SYMBOL.get(normalized.lower(), normalized.lower())
        return normalized.upper()

    left = normalize_pair_token(token_in)
    right = normalize_pair_token(token_out)

    pair_units = PAIR_BASE_UNITS.get((left, right))
    if pair_units is not None:
        return [str(value) for value in pair_units]
    return default_amounts_for_token(token_in)


def parse_amounts(amounts_csv: str | None) -> list[str]:
    if not amounts_csv:
        return default_amounts(18)
    amounts = [amount.strip() for amount in amounts_csv.split(",") if amount.strip()]
    if not amounts:
        raise ValueError("No amounts provided")
    return amounts


def parse_pairs(pair_args: list[str] | None, pairs_csv: str | None) -> list[ResolvedPair]:
    pairs: list[str] = []
    if pairs_csv:
        pairs.extend([entry.strip() for entry in pairs_csv.split(",") if entry.strip()])
    if pair_args:
        pairs.extend(pair_args)

    resolved: list[ResolvedPair] = []
    for entry in pairs:
        if ":" not in entry:
            raise ValueError(f"Invalid pair format: {entry} (expected token_in:token_out)")
        left, right = entry.split(":", 1)
        resolved.append(ResolvedPair(resolve_token(left), resolve_token(right)))
    return resolved


def suite_pairs(name: str) -> list[ResolvedPair]:
    suite = PAIR_SUITES.get(name)
    if suite is None:
        known = ", ".join(sorted(PAIR_SUITES.keys()))
        raise ValueError(f"Unknown suite: {name}. Known suites: {known}")
    return [ResolvedPair(resolve_token(a), resolve_token(b)) for (a, b) in suite]


def list_suites() -> list[str]:
    return sorted(PAIR_SUITES.keys())


def list_tokens() -> list[tuple[str, str]]:
    return sorted(TOKENS.items())
