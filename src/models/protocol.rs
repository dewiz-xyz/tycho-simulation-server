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
        let type_name = normalize_proto_name(&component.protocol_type_name);
        let system_name = normalize_proto_name(&component.protocol_system);

        match type_name.as_str() {
            "uniswap_v2" | "uniswapv2" => return Some(ProtocolKind::UniswapV2),
            "uniswap_v3" | "uniswapv3" => return Some(ProtocolKind::UniswapV3),
            "uniswap_v4" | "uniswapv4" => return Some(ProtocolKind::UniswapV4),
            "curve_pool" => return Some(ProtocolKind::Curve),
            "pancakeswap_v2" | "pancakeswapv2" => return Some(ProtocolKind::PancakeswapV2),
            "pancakeswap_v3" | "pancakeswapv3" => return Some(ProtocolKind::PancakeswapV3),
            "ekubo_v2" | "ekubov2" => return Some(ProtocolKind::EkuboV2),
            "sushiswap_v2" | "sushiswapv2" => return Some(ProtocolKind::SushiswapV2),
            "maverick_v2" | "maverickv2" | "vm:maverick_v2" => {
                return Some(ProtocolKind::MaverickV2)
            }
            "balancer_v2" | "balancerv2_pool" => return Some(ProtocolKind::BalancerV2),
            "fluid_v1" | "fluidv1" => return Some(ProtocolKind::FluidV1),
            "rocketpool" => return Some(ProtocolKind::Rocketpool),
            _ => {}
        }

        match system_name.as_str() {
            "uniswap_v2" | "uniswapv2" => Some(ProtocolKind::UniswapV2),
            "uniswap_v3" | "uniswapv3" => Some(ProtocolKind::UniswapV3),
            "uniswap_v4" | "uniswapv4" => Some(ProtocolKind::UniswapV4),
            "vm:curve" => Some(ProtocolKind::Curve),
            "pancakeswap_v2" | "pancakeswapv2" => Some(ProtocolKind::PancakeswapV2),
            "pancakeswap_v3" | "pancakeswapv3" => Some(ProtocolKind::PancakeswapV3),
            "ekubo_v2" | "ekubov2" => Some(ProtocolKind::EkuboV2),
            "sushiswap_v2" | "sushiswapv2" => Some(ProtocolKind::SushiswapV2),
            "maverick_v2" | "maverickv2" | "vm:maverick_v2" => Some(ProtocolKind::MaverickV2),
            "balancer_v2" | "vm:balancer_v2" => Some(ProtocolKind::BalancerV2),
            "fluid_v1" | "fluidv1" => Some(ProtocolKind::FluidV1),
            "rocketpool" => Some(ProtocolKind::Rocketpool),
            _ => None,
        }
    }

    pub fn from_sync_state_key(protocol: &str) -> Option<Self> {
        let name = normalize_proto_name(protocol);

        match name.as_str() {
            "uniswap_v2" | "uniswapv2" => Some(ProtocolKind::UniswapV2),
            "uniswap_v3" | "uniswapv3" => Some(ProtocolKind::UniswapV3),
            "uniswap_v4" | "uniswapv4" => Some(ProtocolKind::UniswapV4),
            "curve" | "curve_pool" | "vm:curve" => Some(ProtocolKind::Curve),
            "pancakeswap_v2" | "pancakeswapv2" => Some(ProtocolKind::PancakeswapV2),
            "pancakeswap_v3" | "pancakeswapv3" => Some(ProtocolKind::PancakeswapV3),
            "ekubo_v2" | "ekubov2" => Some(ProtocolKind::EkuboV2),
            "sushiswap_v2" | "sushiswapv2" => Some(ProtocolKind::SushiswapV2),
            "maverick_v2" | "maverickv2" | "vm:maverick_v2" => Some(ProtocolKind::MaverickV2),
            "balancer_v2" | "vm:balancer_v2" => Some(ProtocolKind::BalancerV2),
            "fluid_v1" | "fluidv1" => Some(ProtocolKind::FluidV1),
            "rocketpool" => Some(ProtocolKind::Rocketpool),
            _ => None,
        }
    }
}

impl fmt::Display for ProtocolKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn normalize_proto_name(name: &str) -> String {
    name.to_ascii_lowercase().replace(['-', ' '], "_")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tycho_simulation::tycho_common::models::{token::Token, Chain};
    use tycho_simulation::tycho_common::Bytes;

    use super::*;

    #[test]
    fn recognizes_vm_curve_component() {
        let component = ProtocolComponent::new(
            Bytes::default(),
            "vm:curve".to_string(),
            "curve_pool".to_string(),
            Chain::Ethereum,
            Vec::<Token>::new(),
            Vec::<Bytes>::new(),
            HashMap::<String, Bytes>::new(),
            Bytes::default(),
            Default::default(),
        );

        assert_eq!(
            ProtocolKind::from_component(&component),
            Some(ProtocolKind::Curve)
        );
    }

    #[test]
    fn recognizes_vm_maverick_component() {
        let component = ProtocolComponent::new(
            Bytes::default(),
            "vm:maverick_v2".to_string(),
            "maverick_v2".to_string(),
            Chain::Ethereum,
            Vec::<Token>::new(),
            Vec::<Bytes>::new(),
            HashMap::<String, Bytes>::new(),
            Bytes::default(),
            Default::default(),
        );

        assert_eq!(
            ProtocolKind::from_component(&component),
            Some(ProtocolKind::MaverickV2)
        );
    }

    #[test]
    fn recognizes_vm_maverick_sync_key() {
        assert_eq!(
            ProtocolKind::from_sync_state_key("vm:maverick_v2"),
            Some(ProtocolKind::MaverickV2)
        );
    }

    #[test]
    fn does_not_recognize_v4_hooks_sync_key() {
        assert_eq!(ProtocolKind::from_sync_state_key("uniswap_v4_hooks"), None);
    }
}
