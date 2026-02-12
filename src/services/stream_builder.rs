use std::sync::Arc;

use anyhow::Result;
use futures::StreamExt;
use tracing::info;
use tycho_simulation::{
    evm::{
        engine_db::tycho_db::PreCachedDB,
        protocol::{
            ekubo::state::EkuboState,
            filters::{balancer_v2_pool_filter, curve_pool_filter, fluid_v1_paused_pools_filter},
            fluid::FluidV1,
            pancakeswap_v2::state::PancakeswapV2State,
            rocketpool::state::RocketpoolState,
            uniswap_v2::state::UniswapV2State,
            uniswap_v3::state::UniswapV3State,
            uniswap_v4::state::UniswapV4State,
            vm::state::EVMPoolState,
        },
        stream::ProtocolStreamBuilder,
    },
    tycho_client::feed::component_tracker::ComponentFilter,
    tycho_common::models::Chain,
};

use crate::models::tokens::TokenStore;

pub async fn build_native_stream(
    tycho_url: &str,
    api_key: &str,
    tvl_add_threshold: f64,
    tvl_keep_threshold: f64,
    tokens: Arc<TokenStore>,
) -> Result<
    impl futures::Stream<
            Item = Result<
                tycho_simulation::protocol::models::Update,
                Box<dyn std::error::Error + Send + Sync + 'static>,
            >,
        > + Unpin
        + Send,
> {
    let (mut builder, tvl_filter) =
        base_builder(tycho_url, api_key, tvl_add_threshold, tvl_keep_threshold);

    builder = builder.exchange::<UniswapV2State>("uniswap_v2", tvl_filter.clone(), None);
    builder = builder.exchange::<UniswapV2State>("sushiswap_v2", tvl_filter.clone(), None);
    builder = builder.exchange::<PancakeswapV2State>("pancakeswap_v2", tvl_filter.clone(), None);
    builder = builder.exchange::<UniswapV3State>("uniswap_v3", tvl_filter.clone(), None);
    builder = builder.exchange::<UniswapV3State>("pancakeswap_v3", tvl_filter.clone(), None);
    builder = builder.exchange::<UniswapV4State>("uniswap_v4", tvl_filter.clone(), None);
    builder = builder.exchange::<EkuboState>("ekubo_v2", tvl_filter.clone(), None);
    builder = builder.exchange::<FluidV1>(
        "fluid_v1",
        tvl_filter.clone(),
        Some(fluid_v1_paused_pools_filter),
    );
    builder = builder.exchange::<RocketpoolState>("rocketpool", tvl_filter.clone(), None);

    let snapshot = tokens.snapshot().await;
    let stream = builder.set_tokens(snapshot).await.build().await?;

    Ok(stream.map(|item| {
        item.map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync + 'static>)
    }))
}

pub async fn build_vm_stream(
    tycho_url: &str,
    api_key: &str,
    tvl_add_threshold: f64,
    tvl_keep_threshold: f64,
    tokens: Arc<TokenStore>,
) -> Result<
    impl futures::Stream<
            Item = Result<
                tycho_simulation::protocol::models::Update,
                Box<dyn std::error::Error + Send + Sync + 'static>,
            >,
        > + Unpin
        + Send,
> {
    let (mut builder, tvl_filter) =
        base_builder(tycho_url, api_key, tvl_add_threshold, tvl_keep_threshold);

    builder = builder.exchange::<EVMPoolState<PreCachedDB>>(
        "vm:curve",
        tvl_filter.clone(),
        Some(curve_pool_filter),
    );
    builder = builder.exchange::<EVMPoolState<PreCachedDB>>(
        "vm:balancer_v2",
        tvl_filter.clone(),
        Some(balancer_v2_pool_filter),
    );
    builder =
        builder.exchange::<EVMPoolState<PreCachedDB>>("vm:maverick_v2", tvl_filter.clone(), None);

    let snapshot = tokens.snapshot().await;
    let stream = builder.set_tokens(snapshot).await.build().await?;

    Ok(stream.map(|item| {
        item.map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync + 'static>)
    }))
}

fn base_builder(
    tycho_url: &str,
    api_key: &str,
    tvl_add_threshold: f64,
    tvl_keep_threshold: f64,
) -> (ProtocolStreamBuilder, ComponentFilter) {
    let add_tvl = tvl_add_threshold;
    let keep_tvl = tvl_keep_threshold.min(add_tvl);
    info!(
        "Using TVL thresholds: remove/keep={} add={}",
        keep_tvl, add_tvl
    );
    let tvl_filter = ComponentFilter::with_tvl_range(keep_tvl, add_tvl);

    let builder = ProtocolStreamBuilder::new(tycho_url, Chain::Ethereum)
        .latency_buffer(15)
        .auth_key(Some(api_key.to_string()))
        .skip_state_decode_failures(true);

    (builder, tvl_filter)
}
