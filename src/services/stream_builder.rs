use tycho_simulation::{
    evm::{
        engine_db::tycho_db::PreCachedDB,
        protocol::{
            filters::{curve_pool_filter},
            uniswap_v2::state::UniswapV2State,
            uniswap_v3::state::UniswapV3State,
            uniswap_v4::state::UniswapV4State,
            vm::state::EVMPoolState,
        },
        stream::ProtocolStreamBuilder,
    },
    models::Token,
    tycho_client::feed::component_tracker::ComponentFilter,
    tycho_core::dto::Chain,
};

use anyhow::Result;
use std::collections::HashMap;
use tycho_simulation::tycho_core::Bytes;

pub async fn build_protocol_stream(
    tycho_url: &str,
    api_key: &str,
    tvl_threshold: f64,
    tokens: HashMap<Bytes, Token>,
) -> Result<
    impl futures::Stream<
            Item = Result<tycho_simulation::protocol::models::BlockUpdate, impl std::error::Error>,
        > + Unpin
        + Send,
> {
    // Use configured thresholds to bound component set
    let add_tvl = tvl_threshold;
    let keep_tvl = std::env::var("TVL_KEEP_THRESHOLD")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        // Keep threshold should not exceed add threshold to avoid impossible filters
        .map(|v| v.min(add_tvl))
        .unwrap_or((add_tvl * 0.2).min(add_tvl));
    let tvl_filter = ComponentFilter::with_tvl_range(add_tvl, keep_tvl);

    // Build the protocol stream with sound, bounded filters
    let mut builder = ProtocolStreamBuilder::new(tycho_url, Chain::Ethereum.into())
        .timeout(15) // seconds
        .auth_key(Some(api_key.to_string()))
        .skip_state_decode_failures(true);

    // Always include Uniswap V2 (no tick_liquidity requirement)
    builder = builder.exchange::<UniswapV2State>("uniswap_v2", tvl_filter.clone(), None);

    // Include Uniswap V3/V4 unconditionally with the same bounded filter
    builder = builder.exchange::<UniswapV3State>("uniswap_v3", tvl_filter.clone(), None);
    builder = builder.exchange::<UniswapV4State>("uniswap_v4", tvl_filter.clone(), None);

    // Include Curve via generic EVM VM with its pool filter
    builder = builder.exchange::<EVMPoolState<PreCachedDB>>(
        "vm:curve",
        tvl_filter.clone(),
        Some(curve_pool_filter),
    );

    let stream = builder.set_tokens(tokens).await.build().await?;

    Ok(stream)
}
