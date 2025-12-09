use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use futures::StreamExt;
use tracing::info;
use tycho_simulation::{
    evm::{
        engine_db::tycho_db::PreCachedDB,
        protocol::{
            ekubo::state::EkuboState, filters::curve_pool_filter,
            pancakeswap_v2::state::PancakeswapV2State, uniswap_v2::state::UniswapV2State,
            uniswap_v3::state::UniswapV3State, uniswap_v4::state::UniswapV4State,
            vm::state::EVMPoolState,
        },
        stream::ProtocolStreamBuilder,
    },
    tycho_client::feed::component_tracker::ComponentFilter,
    tycho_common::models::Chain,
};

use crate::models::tokens::TokenStore;

static UNISWAP_V4_ACCEPTED: AtomicUsize = AtomicUsize::new(0);
static UNISWAP_V4_FILTERED: AtomicUsize = AtomicUsize::new(0);
static UNISWAP_V4_LOGGED: AtomicBool = AtomicBool::new(false);

pub async fn build_merged_streams(
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
    UNISWAP_V4_ACCEPTED.store(0, Ordering::Relaxed);
    UNISWAP_V4_FILTERED.store(0, Ordering::Relaxed);
    UNISWAP_V4_LOGGED.store(false, Ordering::Relaxed);

    let add_tvl = tvl_add_threshold;
    let keep_tvl = tvl_keep_threshold.min(add_tvl);
    info!(
        "Using TVL thresholds: remove/keep={} add={}",
        keep_tvl, add_tvl
    );
    let tvl_filter = ComponentFilter::with_tvl_range(keep_tvl, add_tvl);

    let mut builder = ProtocolStreamBuilder::new(tycho_url, Chain::Ethereum)
        .latency_buffer(15)
        .auth_key(Some(api_key.to_string()))
        .skip_state_decode_failures(true);

    builder = builder.exchange::<UniswapV2State>("uniswap_v2", tvl_filter.clone(), None);
    builder = builder.exchange::<UniswapV2State>("sushiswap_v2", tvl_filter.clone(), None);
    builder = builder.exchange::<PancakeswapV2State>("pancakeswap_v2", tvl_filter.clone(), None);
    builder = builder.exchange::<UniswapV3State>("uniswap_v3", tvl_filter.clone(), None);
    builder = builder.exchange::<UniswapV3State>("pancakeswap_v3", tvl_filter.clone(), None);
    builder = builder.exchange::<UniswapV4State>("uniswap_v4", tvl_filter.clone(), None);
    builder = builder.exchange::<EkuboState>("ekubo_v2", tvl_filter.clone(), None);
    // builder = builder.exchange::<EVMPoolState<PreCachedDB>>(
    //     "vm:curve",
    //     tvl_filter.clone(),
    //     Some(curve_pool_filter),
    // );
    // builder = builder.exchange::<EVMPoolState<PreCachedDB>>(
    //     "vm:balancer_v2",
    //     tvl_filter.clone(),
    //     Some(balancer_v2_pool_filter),
    // );

    // COMING SOON!
    // builder = builder.exchange::<UniswapV4State>("uniswap_v4_hooks", tvl_filter.clone(), None);
    // builder = builder.exchange::<EVMPoolState<PreCachedDB>>("vm:maverick_v2", tvl_filter.clone(), None);

    let snapshot = tokens.snapshot().await;
    let stream = builder.set_tokens(snapshot).await.build().await?;

    Ok(stream.map(|item| {
        if UNISWAP_V4_LOGGED
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            let accepted = UNISWAP_V4_ACCEPTED.load(Ordering::Relaxed);
            let filtered = UNISWAP_V4_FILTERED.load(Ordering::Relaxed);
            info!(
                protocol = "uniswap_v4",
                accepted_pools = accepted,
                filtered_pools = filtered,
                "RegisteredUniswapV4HookFilter"
            );
        }
        item.map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync + 'static>)
    }))
}
