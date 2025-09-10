use tycho_simulation::{
    evm::{
        protocol::{
            uniswap_v2::state::UniswapV2State,
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
        .unwrap_or((add_tvl * 0.2).max(1000.0));
    let tvl_filter = ComponentFilter::with_tvl_range(add_tvl, keep_tvl);

    let stream = ProtocolStreamBuilder::new(tycho_url, Chain::Ethereum.into())
        // Focus on Uniswap V2 to validate ingestion without tick_liquidity requirement
        .exchange::<UniswapV2State>("uniswap_v2", tvl_filter.clone(), None)
        .timeout(15) // seconds
        .auth_key(Some(api_key.to_string()))
        .skip_state_decode_failures(true)
        .set_tokens(tokens)
        .await
        .build()
        .await?;

    Ok(stream)
}
