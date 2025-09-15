use std::sync::Arc;

use anyhow::Result;
use futures::StreamExt;
use tracing::info;
use tycho_simulation::{
    evm::{
        engine_db::tycho_db::PreCachedDB,
        protocol::{
            filters::curve_pool_filter, uniswap_v2::state::UniswapV2State,
            uniswap_v3::state::UniswapV3State, uniswap_v4::state::UniswapV4State,
            vm::state::EVMPoolState,
        },
        stream::ProtocolStreamBuilder,
    },
    tycho_client::feed::component_tracker::ComponentFilter,
    tycho_core::dto::Chain,
};

use crate::models::tokens::TokenStore;

pub async fn build_merged_streams(
    tycho_url: &str,
    api_key: &str,
    tvl_add_threshold: f64,
    tvl_keep_threshold: f64,
    tokens: Arc<TokenStore>,
) -> Result<
    impl futures::Stream<
            Item = Result<
                tycho_simulation::protocol::models::BlockUpdate,
                Box<dyn std::error::Error + Send + Sync + 'static>,
            >,
        > + Unpin
        + Send,
> {
    let add_tvl = tvl_add_threshold;
    let keep_tvl = tvl_keep_threshold.min(add_tvl);
    info!(
        "Using TVL thresholds: remove/keep={} add={}",
        keep_tvl, add_tvl
    );
    let tvl_filter = ComponentFilter::with_tvl_range(keep_tvl, add_tvl);

    let mut builder = ProtocolStreamBuilder::new(tycho_url, Chain::Ethereum.into())
        .timeout(15)
        .auth_key(Some(api_key.to_string()))
        .skip_state_decode_failures(true);

    builder = builder.exchange::<UniswapV2State>("uniswap_v2", tvl_filter.clone(), None);
    builder = builder.exchange::<UniswapV3State>("uniswap_v3", tvl_filter.clone(), None);
    builder = builder.exchange::<UniswapV4State>("uniswap_v4", tvl_filter.clone(), None);
    builder = builder.exchange::<EVMPoolState<PreCachedDB>>(
        "vm:curve",
        tvl_filter,
        Some(curve_pool_filter),
    );

    let snapshot = tokens.snapshot().await;
    let stream = builder.set_tokens(snapshot).await.build().await?;

    Ok(stream.map(|item| {
        item.map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync + 'static>)
    }))
}
