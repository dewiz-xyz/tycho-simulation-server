use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures::{stream, StreamExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::info;
use tycho_simulation::protocol::models::Update;
use tycho_simulation::rfq::protocols::bebop::client_builder::BebopClientBuilder;
use tycho_simulation::rfq::protocols::bebop::state::BebopState;
use tycho_simulation::rfq::protocols::hashflow::client_builder::HashflowClientBuilder;
use tycho_simulation::rfq::protocols::hashflow::state::HashflowState;
use tycho_simulation::rfq::stream::RFQStreamBuilder;
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

use crate::models::rfq::RFQConfig;
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
    enable_vm_pools: bool,
    enable_rfq_pools: bool,
    rfq_config: RFQConfig,
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

    let chain = Chain::Ethereum;

    let mut builder = ProtocolStreamBuilder::new(tycho_url, chain)
        .latency_buffer(15)
        .auth_key(Some(api_key.to_string()))
        .skip_state_decode_failures(true);

    builder = builder.exchange::<UniswapV2State>("uniswap_v2", tvl_filter.clone(), None);
    builder = builder.exchange::<UniswapV2State>("sushiswap_v2", tvl_filter.clone(), None);
    builder = builder.exchange::<PancakeswapV2State>("pancakeswap_v2", tvl_filter.clone(), None);
    builder = builder.exchange::<UniswapV3State>("uniswap_v3", tvl_filter.clone(), None);
    builder = builder.exchange::<UniswapV3State>("pancakeswap_v3", tvl_filter.clone(), None);
    builder = builder.exchange::<UniswapV4State>("uniswap_v4", tvl_filter.clone(), None);
    builder = builder.exchange::<UniswapV4State>("uniswap_v4_hooks", tvl_filter.clone(), None);
    builder = builder.exchange::<EkuboState>("ekubo_v2", tvl_filter.clone(), None);
    builder = builder.exchange::<FluidV1>(
        "fluid_v1",
        tvl_filter.clone(),
        Some(fluid_v1_paused_pools_filter),
    );
    builder = builder.exchange::<RocketpoolState>("rocketpool", tvl_filter.clone(), None);

    if enable_vm_pools {
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
        info!("VM pool feeds enabled");
    } else {
        info!("VM pool feeds disabled");
    }

    // COMING SOON!
    // builder = builder.exchange::<UniswapV4State>("uniswap_v4_hooks", tvl_filter.clone(), None);
    // builder = builder.exchange::<EVMPoolState<PreCachedDB>>("vm:maverick_v2", tvl_filter.clone(), None);

    let snapshot = tokens.snapshot().await;

    // The RFQStreamBuilder handles registration of multiple RFQ clients and merges their message streams
    //  It merges updates from one or more RFQ clients and decodes them into Update messages:
    let mut rfq_builder;

    let rfq_stream = if enable_rfq_pools {
        // TODO Set up RFQ client using the builder pattern
        // let mut rfq_tokens = HashSet::new();
        //rfq_tokens.insert(sell_token_address.clone());
        // rfq_tokens.insert(buy_token_address.clone());

        rfq_builder = RFQStreamBuilder::new().set_tokens(snapshot.clone()).await;
        let (user, key) = (rfq_config.bebop_user, rfq_config.bebop_key);
        println!("Setting up Bebop RFQ client...\n");
        let bebop_client = BebopClientBuilder::new(chain, user, key)
            // .tokens(rfq_tokens.clone())
            .tvl_threshold(tvl_add_threshold)
            .build()
            .expect("Failed to create Bebop RFQ client");
        rfq_builder = rfq_builder.add_client::<BebopState>("bebop", Box::new(bebop_client));
        let (user, key) = (rfq_config.hashflow_user, rfq_config.hashflow_key);
        println!("Setting up Hashflow RFQ client...\n");
        let hashflow_client = HashflowClientBuilder::new(chain, user, key)
            //.tokens(rfq_tokens)
            .tvl_threshold(tvl_add_threshold)
            .poll_time(Duration::from_secs(5))
            .build()
            .expect("Failed to create Hashflow RFQ client");
        rfq_builder =
            rfq_builder.add_client::<HashflowState>("hashflow", Box::new(hashflow_client));
        let (tx, rx) = mpsc::channel::<Update>(100);
        rfq_builder = rfq_builder.set_tokens(snapshot.clone()).await;
        info!("RFQ pool feeds enabled");
        tokio::spawn(rfq_builder.build(tx));
        info!("Connected to RFQs! Streaming live price levels...\n");

        ReceiverStream::new(rx).map(Ok).boxed()
    } else {
        info!("RFQ pool feeds disabled");
        stream::empty().boxed()
    };

    let stream = builder.set_tokens(snapshot).await.build().await?;
    let merged = stream::select(stream.boxed(), rfq_stream);

    Ok(merged.map(|item| {
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
