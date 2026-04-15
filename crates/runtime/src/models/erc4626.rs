use tycho_simulation::{protocol::models::ProtocolComponent, tycho_common::Bytes};

use super::protocol::ProtocolKind;

struct SupportedErc4626Pair {
    asset_symbol: &'static str,
    share_symbol: &'static str,
    asset: &'static str,
    share: &'static str,
    allow_asset_to_share: bool,
    allow_share_to_asset: bool,
}

impl SupportedErc4626Pair {
    fn supports_direction(
        &self,
        token_in: &Bytes,
        token_out: &Bytes,
        deposits_enabled: bool,
    ) -> bool {
        let token_in = token_in.to_string();
        let token_out = token_out.to_string();
        (deposits_enabled
            && self.allow_asset_to_share
            && token_in.eq_ignore_ascii_case(self.asset)
            && token_out.eq_ignore_ascii_case(self.share))
            || (self.allow_share_to_asset
                && token_in.eq_ignore_ascii_case(self.share)
                && token_out.eq_ignore_ascii_case(self.asset))
    }

    fn component_matches(&self, component: &ProtocolComponent) -> bool {
        component.id.to_string().eq_ignore_ascii_case(self.share)
            && component.tokens.len() == 2
            && component
                .tokens
                .iter()
                .any(|token| token.address.to_string().eq_ignore_ascii_case(self.asset))
            && component
                .tokens
                .iter()
                .any(|token| token.address.to_string().eq_ignore_ascii_case(self.share))
    }

    fn direction_label(&self, token_in: &Bytes, token_out: &Bytes) -> Option<String> {
        if token_in.to_string().eq_ignore_ascii_case(self.asset)
            && token_out.to_string().eq_ignore_ascii_case(self.share)
        {
            return Some(format!("{} -> {}", self.asset_symbol, self.share_symbol));
        }
        if token_in.to_string().eq_ignore_ascii_case(self.share)
            && token_out.to_string().eq_ignore_ascii_case(self.asset)
        {
            return Some(format!("{} -> {}", self.share_symbol, self.asset_symbol));
        }
        None
    }
}

fn supported_pairs() -> &'static [SupportedErc4626Pair] {
    const SUPPORTED: &[SupportedErc4626Pair] = &[
        SupportedErc4626Pair {
            asset_symbol: "USDS",
            share_symbol: "sUSDS",
            asset: "0xdC035D45d973E3EC169d2276DDab16f1e407384F",
            share: "0xa3931d71877c0e7a3148cb7eb4463524fec27fbd",
            allow_asset_to_share: true,
            allow_share_to_asset: true,
        },
        SupportedErc4626Pair {
            asset_symbol: "USDC",
            share_symbol: "sUSDC",
            asset: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            share: "0xBc65ad17c5C0a2A4D159fa5a503f4992c7B545FE",
            allow_asset_to_share: true,
            allow_share_to_asset: true,
        },
        SupportedErc4626Pair {
            asset_symbol: "PYUSD",
            share_symbol: "spPYUSD",
            asset: "0x6c3ea9036406852006290770BEdFcAbA0e23A0e8",
            share: "0x80128DbB9f07b93DDE62A6daeadb69ED14a7D354",
            allow_asset_to_share: true,
            allow_share_to_asset: true,
        },
    ];
    SUPPORTED
}

fn normalize_protocol_id(protocol: &str) -> String {
    protocol
        .trim()
        .to_ascii_lowercase()
        .replace(['-', ' '], "_")
}

pub(crate) fn is_erc4626_protocol(protocol: &str) -> bool {
    normalize_protocol_id(protocol) == "erc4626"
}

pub(crate) fn component_is_erc4626(component: &ProtocolComponent) -> bool {
    ProtocolKind::from_component(component) == Some(ProtocolKind::ERC4626)
}

pub(crate) fn request_direction_supported(
    protocol: &str,
    token_in: &Bytes,
    token_out: &Bytes,
    deposits_enabled: bool,
) -> bool {
    !is_erc4626_protocol(protocol)
        || supported_pairs()
            .iter()
            .any(|pair| pair.supports_direction(token_in, token_out, deposits_enabled))
}

pub(crate) fn component_direction_supported(
    component: &ProtocolComponent,
    token_in: &Bytes,
    token_out: &Bytes,
    deposits_enabled: bool,
) -> bool {
    !component_is_erc4626(component)
        || supported_pairs().iter().any(|pair| {
            pair.supports_direction(token_in, token_out, deposits_enabled)
                && pair.component_matches(component)
        })
}

pub(crate) fn unsupported_direction_message(
    token_in: &Bytes,
    token_out: &Bytes,
    deposits_enabled: bool,
) -> String {
    let requested = supported_pairs()
        .iter()
        .find_map(|pair| pair.direction_label(token_in, token_out))
        .unwrap_or_else(|| format!("{token_in} -> {token_out}"));
    let supported = supported_direction_labels(deposits_enabled).join(", ");
    format!(
        "ERC4626 direction {requested} is not currently supported by this server; supported directions are [{supported}]"
    )
}

pub(crate) fn supported_direction_labels(deposits_enabled: bool) -> Vec<String> {
    let mut directions = Vec::new();
    for pair in supported_pairs() {
        if deposits_enabled && pair.allow_asset_to_share {
            directions.push(format!("{} -> {}", pair.asset_symbol, pair.share_symbol));
        }
        if pair.allow_share_to_asset {
            directions.push(format!("{} -> {}", pair.share_symbol, pair.asset_symbol));
        }
    }
    directions
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "ERC4626 support tests use fixed addresses with deterministic parsing"
)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;

    use chrono::NaiveDateTime;
    use tycho_simulation::tycho_common::models::{token::Token, Chain};

    use super::*;

    fn token(address: &str, symbol: &str, decimals: u32) -> Token {
        Token::new(
            &Bytes::from_str(address).unwrap(),
            symbol,
            decimals,
            0,
            &[],
            Chain::Ethereum,
            100,
        )
    }

    fn component(
        share: &str,
        protocol_system: &str,
        protocol_type_name: &str,
        tokens: Vec<Token>,
    ) -> ProtocolComponent {
        ProtocolComponent::new(
            Bytes::from_str(share).unwrap(),
            protocol_system.to_string(),
            protocol_type_name.to_string(),
            Chain::Ethereum,
            tokens,
            Vec::new(),
            HashMap::new(),
            Bytes::default(),
            NaiveDateTime::default(),
        )
    }

    #[test]
    fn supports_all_allowlisted_request_directions() {
        for (protocol, token_in, token_out) in [
            (
                "erc4626",
                "0xdC035D45d973E3EC169d2276DDab16f1e407384F",
                "0xa3931d71877c0e7a3148cb7eb4463524fec27fbd",
            ),
            (
                "erc4626",
                "0xa3931d71877c0e7a3148cb7eb4463524fec27fbd",
                "0xdC035D45d973E3EC169d2276DDab16f1e407384F",
            ),
            (
                "erc4626",
                "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
                "0xBc65ad17c5C0a2A4D159fa5a503f4992c7B545FE",
            ),
            (
                "erc4626",
                "0xBc65ad17c5C0a2A4D159fa5a503f4992c7B545FE",
                "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            ),
            (
                "erc4626",
                "0x6c3ea9036406852006290770BEdFcAbA0e23A0e8",
                "0x80128DbB9f07b93DDE62A6daeadb69ED14a7D354",
            ),
            (
                "erc4626",
                "0x80128DbB9f07b93DDE62A6daeadb69ED14a7D354",
                "0x6c3ea9036406852006290770BEdFcAbA0e23A0e8",
            ),
        ] {
            assert!(request_direction_supported(
                protocol,
                &Bytes::from_str(token_in).unwrap(),
                &Bytes::from_str(token_out).unwrap(),
                true,
            ));
        }
    }

    #[test]
    fn disables_allowlisted_deposit_directions_without_rpc_capability() {
        for (protocol, token_in, token_out) in [
            (
                "erc4626",
                "0xdC035D45d973E3EC169d2276DDab16f1e407384F",
                "0xa3931d71877c0e7a3148cb7eb4463524fec27fbd",
            ),
            (
                "erc4626",
                "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
                "0xBc65ad17c5C0a2A4D159fa5a503f4992c7B545FE",
            ),
            (
                "erc4626",
                "0x6c3ea9036406852006290770BEdFcAbA0e23A0e8",
                "0x80128DbB9f07b93DDE62A6daeadb69ED14a7D354",
            ),
        ] {
            assert!(!request_direction_supported(
                protocol,
                &Bytes::from_str(token_in).unwrap(),
                &Bytes::from_str(token_out).unwrap(),
                false,
            ));
        }
    }

    #[test]
    fn keeps_allowlisted_redeem_directions_without_rpc_capability() {
        for (protocol, token_in, token_out) in [
            (
                "erc4626",
                "0xa3931d71877c0e7a3148cb7eb4463524fec27fbd",
                "0xdC035D45d973E3EC169d2276DDab16f1e407384F",
            ),
            (
                "erc4626",
                "0xBc65ad17c5C0a2A4D159fa5a503f4992c7B545FE",
                "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            ),
            (
                "erc4626",
                "0x80128DbB9f07b93DDE62A6daeadb69ED14a7D354",
                "0x6c3ea9036406852006290770BEdFcAbA0e23A0e8",
            ),
        ] {
            assert!(request_direction_supported(
                protocol,
                &Bytes::from_str(token_in).unwrap(),
                &Bytes::from_str(token_out).unwrap(),
                false,
            ));
        }
    }

    #[test]
    fn rejects_non_allowlisted_request_directions() {
        for (protocol, token_in, token_out) in [
            (
                "erc4626",
                "0x9d39a5de30e57443bff2a8307a4256c8797a3497",
                "0x4c9EDD5852cd905f086C759E8383e09bff1E68B3",
            ),
            (
                "erc4626",
                "0x4c9EDD5852cd905f086C759E8383e09bff1E68B3",
                "0x9d39a5de30e57443bff2a8307a4256c8797a3497",
            ),
            (
                "erc4626",
                "0xdC035D45d973E3EC169d2276DDab16f1e407384F",
                "0x80128DbB9f07b93DDE62A6daeadb69ED14a7D354",
            ),
        ] {
            assert!(!request_direction_supported(
                protocol,
                &Bytes::from_str(token_in).unwrap(),
                &Bytes::from_str(token_out).unwrap(),
                true,
            ));
        }
    }

    #[test]
    fn component_support_requires_matching_share_token() {
        let usds = token("0xdC035D45d973E3EC169d2276DDab16f1e407384F", "USDS", 18);
        let susds = token("0xa3931d71877c0e7a3148cb7eb4463524fec27fbd", "sUSDS", 18);
        let wrong_share = token("0x9d39a5de30e57443bff2a8307a4256c8797a3497", "sUSDe", 18);
        let token_in = usds.address.clone();
        let token_out = susds.address.clone();

        let supported_component = component(
            "0xa3931d71877c0e7a3148cb7eb4463524fec27fbd",
            "erc4626",
            "erc4626_pool",
            vec![usds.clone(), susds],
        );
        assert!(component_direction_supported(
            &supported_component,
            &token_in,
            &token_out,
            true,
        ));

        let unsupported_component = component(
            "0x9d39a5de30e57443bff2a8307a4256c8797a3497",
            "erc4626",
            "erc4626_pool",
            vec![usds, wrong_share],
        );
        assert!(!component_direction_supported(
            &unsupported_component,
            &token_in,
            &token_out,
            true,
        ));
    }

    #[test]
    fn recognizes_erc4626_component_by_protocol_type_name() {
        let usds = token("0xdC035D45d973E3EC169d2276DDab16f1e407384F", "USDS", 18);
        let susds = token("0xa3931d71877c0e7a3148cb7eb4463524fec27fbd", "sUSDS", 18);
        let token_in = usds.address.clone();
        let token_out = susds.address.clone();

        let component = component(
            "0xa3931d71877c0e7a3148cb7eb4463524fec27fbd",
            "",
            "erc4626_pool",
            vec![usds, susds],
        );

        assert!(component_is_erc4626(&component));
        assert!(component_direction_supported(
            &component, &token_in, &token_out, true,
        ));
        assert!(!component_direction_supported(
            &component, &token_in, &token_out, false,
        ));
    }

    #[test]
    fn supported_direction_labels_reflect_deposit_capability() {
        assert_eq!(
            supported_direction_labels(false),
            vec![
                "sUSDS -> USDS".to_string(),
                "sUSDC -> USDC".to_string(),
                "spPYUSD -> PYUSD".to_string(),
            ]
        );
    }
}
