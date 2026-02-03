use std::fmt;

use tycho_simulation::protocol::models::ProtocolComponent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtocolKind {
    UniswapV2,
    UniswapV3,
    UniswapV4,
    Curve,
    PancakeswapV2,
    PancakeswapV3,
    EkuboV2,
    SushiswapV2,
    MaverickV2,
    BalancerV2,
    FluidV1,
    Rocketpool,
}

const PROTOCOL_TYPE_NAME_ALIASES: [(&str, ProtocolKind); 6] = [
    ("uniswap_v2_pool", ProtocolKind::UniswapV2),
    ("uniswap_v3_pool", ProtocolKind::UniswapV3),
    ("uniswap_v4_pool", ProtocolKind::UniswapV4),
    ("sushiswap_v2_pool", ProtocolKind::SushiswapV2),
    ("pancakeswap_v3_pool", ProtocolKind::PancakeswapV3),
    ("balancer_v2_pool", ProtocolKind::BalancerV2),
];

const PROTOCOL_SYSTEM_ALIASES: [(&str, ProtocolKind); 4] = [
    ("vm:curve", ProtocolKind::Curve),
    ("vm:balancer_v2", ProtocolKind::BalancerV2),
    ("uniswap_v4_hooks", ProtocolKind::UniswapV4),
    ("vm:maverick_v2", ProtocolKind::MaverickV2),
];

impl ProtocolKind {
    pub const ALL: [ProtocolKind; 12] = [
        ProtocolKind::UniswapV2,
        ProtocolKind::UniswapV3,
        ProtocolKind::UniswapV4,
        ProtocolKind::Curve,
        ProtocolKind::PancakeswapV2,
        ProtocolKind::PancakeswapV3,
        ProtocolKind::EkuboV2,
        ProtocolKind::SushiswapV2,
        ProtocolKind::MaverickV2,
        ProtocolKind::BalancerV2,
        ProtocolKind::FluidV1,
        ProtocolKind::Rocketpool,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            ProtocolKind::UniswapV2 => "uniswap_v2",
            ProtocolKind::UniswapV3 => "uniswap_v3",
            ProtocolKind::UniswapV4 => "uniswap_v4",
            ProtocolKind::Curve => "curve",
            ProtocolKind::PancakeswapV2 => "pancakeswap_v2",
            ProtocolKind::PancakeswapV3 => "pancakeswap_v3",
            ProtocolKind::EkuboV2 => "ekubo_v2",
            ProtocolKind::SushiswapV2 => "sushiswap_v2",
            ProtocolKind::MaverickV2 => "maverick_v2",
            ProtocolKind::BalancerV2 => "balancer_v2",
            ProtocolKind::FluidV1 => "fluid_v1",
            ProtocolKind::Rocketpool => "rocketpool",
        }
    }

    pub fn from_component(component: &ProtocolComponent) -> Option<Self> {
        let protocol_type_name = normalize_protocol_name(&component.protocol_type_name);
        if let Some(kind) = protocol_kind_from_protocol_type_name(&protocol_type_name) {
            return Some(kind);
        }

        let protocol_system_name = normalize_protocol_name(&component.protocol_system);
        protocol_kind_from_protocol_system_name(&protocol_system_name)
    }
}

impl fmt::Display for ProtocolKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn protocol_kind_from_protocol_type_name(protocol_type_name: &str) -> Option<ProtocolKind> {
    protocol_kind_from_aliases(protocol_type_name, &PROTOCOL_TYPE_NAME_ALIASES)
}

fn protocol_kind_from_protocol_system_name(protocol_system_name: &str) -> Option<ProtocolKind> {
    ProtocolKind::ALL
        .iter()
        .copied()
        .find(|kind| kind.as_str() == protocol_system_name)
        .or_else(|| protocol_kind_from_aliases(protocol_system_name, &PROTOCOL_SYSTEM_ALIASES))
}

fn protocol_kind_from_aliases(
    name: &str,
    aliases: &[(&'static str, ProtocolKind)],
) -> Option<ProtocolKind> {
    aliases
        .iter()
        .find_map(|(alias, kind)| (*alias == name).then_some(*kind))
}

fn normalize_protocol_name(name: &str) -> String {
    name.to_ascii_lowercase().replace(['-', ' '], "_")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tycho_simulation::tycho_common::models::{token::Token, Chain};
    use tycho_simulation::tycho_common::Bytes;

    use super::*;

    fn protocol_component(protocol_system: &str, protocol_type_name: &str) -> ProtocolComponent {
        ProtocolComponent::new(
            Bytes::default(),
            protocol_system.to_string(),
            protocol_type_name.to_string(),
            Chain::Ethereum,
            Vec::<Token>::new(),
            Vec::<Bytes>::new(),
            HashMap::<String, Bytes>::new(),
            Bytes::default(),
            Default::default(),
        )
    }

    #[test]
    fn recognizes_protocol_type_names() {
        let cases = [
            ("uniswap_v2_pool", ProtocolKind::UniswapV2),
            ("uniswap_v3_pool", ProtocolKind::UniswapV3),
            ("uniswap_v4_pool", ProtocolKind::UniswapV4),
            ("sushiswap_v2_pool", ProtocolKind::SushiswapV2),
            ("pancakeswap_v3_pool", ProtocolKind::PancakeswapV3),
            ("balancer_v2_pool", ProtocolKind::BalancerV2),
        ];

        for (protocol_type_name, expected) in cases {
            let component = protocol_component("", protocol_type_name);
            assert_eq!(ProtocolKind::from_component(&component), Some(expected));
        }
    }

    #[test]
    fn recognizes_protocol_system_names() {
        for kind in ProtocolKind::ALL {
            let component = protocol_component(kind.as_str(), "");
            assert_eq!(ProtocolKind::from_component(&component), Some(kind));
        }
    }

    #[test]
    fn recognizes_protocol_system_aliases() {
        let cases = [
            ("vm:curve", ProtocolKind::Curve),
            ("vm:balancer_v2", ProtocolKind::BalancerV2),
            ("uniswap_v4_hooks", ProtocolKind::UniswapV4),
            ("vm:maverick_v2", ProtocolKind::MaverickV2),
        ];

        for (protocol_system, expected) in cases {
            let component = protocol_component(protocol_system, "");
            assert_eq!(ProtocolKind::from_component(&component), Some(expected));
        }
    }
}
