use tycho_simulation::{
    evm::{
        engine_db::tycho_db::PreCachedDB,
        protocol::{
            filters::{balancer_pool_filter, curve_pool_filter},
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
    let tvl_filter = ComponentFilter::with_tvl_range(tvl_threshold, tvl_threshold);

    let stream = ProtocolStreamBuilder::new(tycho_url, Chain::Ethereum.into())
        .exchange::<UniswapV2State>("uniswap_v2", tvl_filter.clone(), None)
        .exchange::<UniswapV3State>("uniswap_v3", tvl_filter.clone(), None)
        .exchange::<UniswapV4State>("uniswap_v4", tvl_filter.clone(), None)
        .exchange::<EVMPoolState<PreCachedDB>>(
            "vm:curve",
            tvl_filter.clone(),
            Some(curve_pool_filter),
        )
        .exchange::<EVMPoolState<PreCachedDB>>(
            "vm:balancer_v2",
            tvl_filter.clone(),
            Some(balancer_pool_filter),
        )
        .auth_key(Some(api_key.to_string()))
        .skip_state_decode_failures(true)
        .set_tokens(tokens)
        .await
        .build()
        .await?;

    Ok(stream)
}
