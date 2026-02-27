"""Curated tokens + pair suites for simulation testing."""

from __future__ import annotations

import os
from dataclasses import dataclass
from typing import Tuple

SUPPORTED_CHAIN_IDS = (1, 8453)


def parse_chain_id(raw_value: str) -> int:
    try:
        chain_id = int(raw_value)
    except ValueError as exc:
        raise ValueError(f"Invalid chain id: {raw_value!r}. Expected one of {SUPPORTED_CHAIN_IDS}.") from exc
    if chain_id not in SUPPORTED_CHAIN_IDS:
        raise ValueError(f"Unsupported chain id: {chain_id}. Supported values: {SUPPORTED_CHAIN_IDS}.")
    return chain_id


def resolve_chain_id(chain_id_arg: str | None) -> int:
    raw = chain_id_arg
    if raw is None:
        raw = os.environ.get("CHAIN_ID")
    if raw is None or not raw.strip():
        raise ValueError(
            "Missing chain id. Pass --chain-id or set CHAIN_ID in the environment/.env."
        )
    return parse_chain_id(raw.strip())


def chain_label(chain_id: int) -> str:
    return {1: "ethereum", 8453: "base"}.get(chain_id, f"chain-{chain_id}")


# Ethereum mainnet token addresses (lowercased).
ETHEREUM_TOKENS: dict[str, str] = {
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
    # LSTs
    "STETH": "0xae7ab96520de3a18e5e111b5eaab095312d7fe84",
    "WSTETH": "0x7f39c581f595b53c5cb19bd0b3f8da6c935e2ca0",
    "RETH": "0xae78736cd615f374d3085123a210448e74fc6393",
    "CBETH": "0xbe9895146f7af43049ca1c1ae358b0541ea49704",
    "FRXETH": "0x5e8422345238f34275888049021821e8e08caa1f",
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

# Base token addresses (lowercased).
BASE_TOKENS: dict[str, str] = {
    "ETH": "0x0000000000000000000000000000000000000000",
    "WETH": "0x4200000000000000000000000000000000000006",
    "USDC": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
    "DAI": "0x50c5725949a6f0c72e6c4a641f24049a917db0cb",
}

TOKENS_BY_CHAIN: dict[int, dict[str, str]] = {
    1: ETHEREUM_TOKENS,
    8453: BASE_TOKENS,
}

# Backward-compatible alias used by older callers.
TOKENS: dict[str, str] = TOKENS_BY_CHAIN[1]

TOKEN_DECIMALS_BY_CHAIN: dict[int, dict[str, int]] = {
    1: {
        "USDC": 6,
        "USDT": 6,
        "WBTC": 8,
    },
    8453: {
        "USDC": 6,
    },
}

TOKEN_BASE_UNITS_BY_CHAIN: dict[int, dict[str, list[int]]] = {
    1: {
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
    },
    8453: {
        "WETH": [
            10_000_000_000_000_000,
            50_000_000_000_000_000,
            100_000_000_000_000_000,
            500_000_000_000_000_000,
            1_000_000_000_000_000_000,
            2_000_000_000_000_000_000,
            5_000_000_000_000_000_000,
            10_000_000_000_000_000_000,
            20_000_000_000_000_000_000,
            50_000_000_000_000_000_000,
        ],
        "ETH": [
            10_000_000_000_000_000,
            50_000_000_000_000_000,
            100_000_000_000_000_000,
            500_000_000_000_000_000,
            1_000_000_000_000_000_000,
            2_000_000_000_000_000_000,
            5_000_000_000_000_000_000,
            10_000_000_000_000_000_000,
            20_000_000_000_000_000_000,
            50_000_000_000_000_000_000,
        ],
    },
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

Pair = Tuple[str, str]

PAIR_SUITES_BY_CHAIN: dict[int, dict[str, list[Pair]]] = {
    1: {
        "smoke": [
            ("DAI", "USDC"),
            ("WETH", "USDC"),
            ("USDC", "USDT"),
            ("WBTC", "WETH"),
            ("STETH", "WETH"),
        ],
        "core": [
            ("DAI", "USDC"),
            ("USDC", "USDT"),
            ("GHO", "USDC"),
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
            ("CBETH", "WETH"),
            ("UNI", "WETH"),
            ("LINK", "WETH"),
            ("AAVE", "WETH"),
            ("COMP", "WETH"),
            ("MKR", "WETH"),
            ("SUSHI", "WETH"),
            ("LDO", "WETH"),
        ],
        "extended": [
            ("DAI", "USDC"),
            ("USDC", "USDT"),
            ("GHO", "USDC"),
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
            ("CBETH", "WETH"),
            ("UNI", "WETH"),
            ("LINK", "WETH"),
            ("AAVE", "WETH"),
            ("COMP", "WETH"),
            ("MKR", "WETH"),
            ("SUSHI", "WETH"),
            ("LDO", "WETH"),
            ("LUSD", "USDC"),
            ("FRXETH", "WETH"),
            ("CRV", "WETH"),
            ("CVX", "WETH"),
            ("BAL", "WETH"),
            ("RPL", "WETH"),
            ("SNX", "WETH"),
            ("YFI", "WETH"),
            ("FXS", "WETH"),
        ],
        "stables": [
            ("DAI", "USDC"),
            ("DAI", "USDT"),
            ("USDC", "USDT"),
            ("GHO", "USDC"),
            ("FRAX", "USDC"),
        ],
        "lst": [
            ("STETH", "WETH"),
            ("WSTETH", "WETH"),
            ("RETH", "WETH"),
            ("CBETH", "WETH"),
        ],
        "governance": [
            ("UNI", "WETH"),
            ("LINK", "WETH"),
            ("AAVE", "WETH"),
            ("COMP", "WETH"),
            ("MKR", "WETH"),
            ("SUSHI", "WETH"),
            ("LDO", "WETH"),
        ],
        "v4_candidates": [
            ("WETH", "USDC"),
            ("WBTC", "USDC"),
            ("ETH", "USDC"),
        ],
    },
    8453: {
        "smoke": [
            ("USDC", "WETH"),
            ("WETH", "USDC"),
            ("ETH", "USDC"),
            ("USDC", "ETH"),
        ],
        "core": [
            ("USDC", "WETH"),
            ("WETH", "USDC"),
            ("ETH", "USDC"),
            ("USDC", "ETH"),
            ("DAI", "USDC"),
            ("USDC", "DAI"),
        ],
        "extended": [
            ("USDC", "WETH"),
            ("WETH", "USDC"),
            ("ETH", "USDC"),
            ("USDC", "ETH"),
            ("DAI", "USDC"),
            ("USDC", "DAI"),
        ],
        "stables": [
            ("DAI", "USDC"),
            ("USDC", "DAI"),
        ],
        "lst": [
            ("ETH", "WETH"),
            ("WETH", "ETH"),
        ],
        "governance": [
            ("WETH", "USDC"),
        ],
        "v4_candidates": [
            ("WETH", "USDC"),
            ("ETH", "USDC"),
        ],
    },
}

DEFAULT_ENCODE_ROUTE_BY_CHAIN: dict[int, tuple[str, str, str]] = {
    1: ("DAI", "USDC", "USDT"),
    8453: ("USDC", "WETH", "USDC"),
}


@dataclass(frozen=True)
class ResolvedPair:
    token_in: str
    token_out: str


def tokens_for_chain(chain_id: int) -> dict[str, str]:
    return TOKENS_BY_CHAIN[chain_id]


def default_encode_route(chain_id: int) -> tuple[str, str, str]:
    route = DEFAULT_ENCODE_ROUTE_BY_CHAIN.get(chain_id)
    if route is None:
        raise ValueError(f"No default encode route configured for chain {chain_id}")
    return route


def resolve_token(token_or_symbol: str, chain_id: int) -> str:
    token_or_symbol = token_or_symbol.strip()
    if token_or_symbol.lower().startswith("0x"):
        return token_or_symbol.lower()

    symbol = token_or_symbol.upper()
    address = TOKENS_BY_CHAIN[chain_id].get(symbol)
    if not address:
        raise ValueError(
            f"Unknown token symbol for chain {chain_id} ({chain_label(chain_id)}): {token_or_symbol}"
        )
    return address


def default_amounts(decimals: int) -> list[str]:
    scale = 10**decimals
    return [str(amount * scale) for amount in BASE_AMOUNTS]


def token_decimals(token_or_symbol: str, chain_id: int) -> int:
    token_or_symbol = token_or_symbol.strip()
    if token_or_symbol.lower().startswith("0x"):
        symbol = None
        token_address = token_or_symbol.lower()
    else:
        symbol = token_or_symbol.upper()
        token_address = TOKENS_BY_CHAIN[chain_id].get(symbol, "").lower()

    decimals_by_symbol = TOKEN_DECIMALS_BY_CHAIN.get(chain_id, {})
    if symbol and symbol in decimals_by_symbol:
        return decimals_by_symbol[symbol]

    for known_symbol, known_address in TOKENS_BY_CHAIN[chain_id].items():
        if known_address.lower() == token_address:
            return decimals_by_symbol.get(known_symbol, 18)

    return 18


def default_amounts_for_token(token_or_symbol: str, chain_id: int) -> list[str]:
    token_or_symbol = token_or_symbol.strip()
    if token_or_symbol.lower().startswith("0x"):
        address = token_or_symbol.lower()
        symbol = None
    else:
        symbol = token_or_symbol.upper()
        address = TOKENS_BY_CHAIN[chain_id].get(symbol, "").lower()

    base_units = None
    if symbol:
        base_units = TOKEN_BASE_UNITS_BY_CHAIN.get(chain_id, {}).get(symbol)
    if base_units is None and address:
        for known_symbol, known_address in TOKENS_BY_CHAIN[chain_id].items():
            if known_address.lower() == address:
                base_units = TOKEN_BASE_UNITS_BY_CHAIN.get(chain_id, {}).get(known_symbol)
                break

    if base_units is not None:
        return [str(value) for value in base_units]

    return default_amounts(token_decimals(token_or_symbol, chain_id))


def parse_amounts(amounts_csv: str | None) -> list[str]:
    if not amounts_csv:
        return default_amounts(18)
    amounts = [amount.strip() for amount in amounts_csv.split(",") if amount.strip()]
    if not amounts:
        raise ValueError("No amounts provided")
    return amounts


def parse_pairs(pair_args: list[str] | None, pairs_csv: str | None, chain_id: int) -> list[ResolvedPair]:
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
        resolved.append(ResolvedPair(resolve_token(left, chain_id), resolve_token(right, chain_id)))
    return resolved


def suite_pairs(name: str, chain_id: int) -> list[ResolvedPair]:
    suites = PAIR_SUITES_BY_CHAIN.get(chain_id, {})
    suite = suites.get(name)
    if suite is None:
        known = ", ".join(sorted(suites.keys()))
        raise ValueError(
            f"Unknown suite for chain {chain_id} ({chain_label(chain_id)}): {name}. Known suites: {known}"
        )
    return [ResolvedPair(resolve_token(a, chain_id), resolve_token(b, chain_id)) for (a, b) in suite]


def list_suites(chain_id: int) -> list[str]:
    return sorted(PAIR_SUITES_BY_CHAIN.get(chain_id, {}).keys())


def list_tokens(chain_id: int) -> list[tuple[str, str]]:
    return sorted(TOKENS_BY_CHAIN.get(chain_id, {}).items())
