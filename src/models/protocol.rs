use std::fmt;

use tycho_simulation::protocol::models::ProtocolComponent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtocolKind {
    UniswapV2,
    UniswapV3,
    UniswapV4,
    Curve,
}

impl ProtocolKind {
    pub const ALL: [ProtocolKind; 4] = [
        ProtocolKind::UniswapV2,
        ProtocolKind::UniswapV3,
        ProtocolKind::UniswapV4,
        ProtocolKind::Curve,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            ProtocolKind::UniswapV2 => "uniswap_v2",
            ProtocolKind::UniswapV3 => "uniswap_v3",
            ProtocolKind::UniswapV4 => "uniswap_v4",
            ProtocolKind::Curve => "curve",
        }
    }

    pub fn from_component(component: &ProtocolComponent) -> Option<Self> {
        let type_name = normalize_proto_name(&component.protocol_type_name);
        let system_name = normalize_proto_name(&component.protocol_system);

        match type_name.as_str() {
            "uniswap_v2" | "uniswapv2" => return Some(ProtocolKind::UniswapV2),
            "uniswap_v3" | "uniswapv3" => return Some(ProtocolKind::UniswapV3),
            "uniswap_v4" | "uniswapv4" => return Some(ProtocolKind::UniswapV4),
            "curve" | "curve_pool" | "curvefinance" => return Some(ProtocolKind::Curve),
            _ => {}
        }

        match system_name.as_str() {
            "uniswap_v2" | "uniswapv2" => Some(ProtocolKind::UniswapV2),
            "uniswap_v3" | "uniswapv3" => Some(ProtocolKind::UniswapV3),
            "uniswap_v4" | "uniswapv4" => Some(ProtocolKind::UniswapV4),
            "curve" | "curvefinance" => Some(ProtocolKind::Curve),
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
