use std::{sync::Arc, time::Duration};

use crate::models::tokens::TokenStore;
use anyhow::Result;
use futures::{FutureExt, StreamExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::info;
use tycho_simulation::{
    evm::{
        decoder::StreamDecodeError,
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
    protocol::models::Update,
    rfq::{
        protocols::{
            bebop::{client_builder::BebopClientBuilder, state::BebopState},
            hashflow::{client_builder::HashflowClientBuilder, state::HashflowState},
        },
        stream::RFQStreamBuilder,
    },
    tycho_client::feed::component_tracker::ComponentFilter,
    tycho_common::models::Chain,
};

#[derive(Clone, Copy)]
enum StreamDecodePolicy {
    Native,
    Vm,
}

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
    let (mut builder, tvl_filter) = base_builder(
        tycho_url,
        api_key,
        tvl_add_threshold,
        tvl_keep_threshold,
        decode_skip_state_failures(StreamDecodePolicy::Native),
    );

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
    // builder = builder.exchange::<RocketpoolState>("rocketpool", tvl_filter.clone(), None);

    let snapshot = tokens.snapshot().await;
    let stream = builder.set_tokens(snapshot).await.build().await?;

    Ok(stream.map(|item| {
        item.map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync + 'static>)
    }))
}

pub struct RFQConfig {
    pub bebop_user: String,
    pub bebop_key: String,
    pub hashflow_user: String,
    pub hashflow_key: String,
}

pub async fn build_rfq_stream(
    tycho_url: &str,
    api_key: &str,
    tvl_add_threshold: f64,
    tvl_keep_threshold: f64,
    tokens: Arc<TokenStore>,
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
    let mut rfq_builder;
    let snapshot = tokens.snapshot().await;

    // TODO Set up RFQ client using the builder pattern
    // let mut rfq_tokens = HashSet::new();
    //rfq_tokens.insert(sell_token_address.clone());
    // rfq_tokens.insert(buy_token_address.clone());

    rfq_builder = RFQStreamBuilder::new().set_tokens(snapshot.clone()).await;
    let (user, key) = (rfq_config.bebop_user, rfq_config.bebop_key);
    println!("Setting up Bebop RFQ client...\n");
    let bebop_client = BebopClientBuilder::new(Chain::Ethereum, user, key)
        // .tokens(rfq_tokens.clone())
        .tvl_threshold(tvl_add_threshold)
        .build()
        .expect("Failed to create Bebop RFQ client");
    rfq_builder = rfq_builder.add_client::<BebopState>("bebop", Box::new(bebop_client));
    let (user, key) = (rfq_config.hashflow_user, rfq_config.hashflow_key);
    println!("Setting up Hashflow RFQ client...\n");
    let hashflow_client = HashflowClientBuilder::new(Chain::Ethereum, user, key)
        //.tokens(rfq_tokens)
        .tvl_threshold(tvl_add_threshold)
        .poll_time(Duration::from_secs(5))
        .build()
        .expect("Failed to create Hashflow RFQ client");
    rfq_builder = rfq_builder.add_client::<HashflowState>("hashflow", Box::new(hashflow_client));
    let (tx, mut rx) = mpsc::channel::<Update>(100);
    rfq_builder = rfq_builder.set_tokens(snapshot.clone()).await;
    info!("RFQ pool feeds enabled");
    tokio::spawn(rfq_builder.build(tx));
    info!("Connected to RFQs! Streaming live price levels...\n");

    //let rfq_stream = ReceiverStream::new(rx).map(Ok).boxed();

    // Ok(rx.recv().map(|item| {
    //      item.map_err(|err: StreamDecodeError| {
    //          Box::new(err) as Box<dyn std::error::Error + Send + Sync + 'static>
    //      })))

    Ok(ReceiverStream::new(rx).map(Ok).boxed())

    //let stream = builder.set_tokens(snapshot).await.build().await?;
    // Ok(rfq_stream.map(|item| {
    //     item.map_err(|err: StreamDecodeError| {
    //         Box::new(err) as Box<dyn std::error::Error + Send + Sync + 'static>
    //     })
    // }))
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
    let (mut builder, tvl_filter) = base_builder(
        tycho_url,
        api_key,
        tvl_add_threshold,
        tvl_keep_threshold,
        decode_skip_state_failures(StreamDecodePolicy::Vm),
    );

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
    skip_state_decode_failures: bool,
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
        .skip_state_decode_failures(skip_state_decode_failures);

    (builder, tvl_filter)
}

fn decode_skip_state_failures(policy: StreamDecodePolicy) -> bool {
    match policy {
        StreamDecodePolicy::Native => true,
        StreamDecodePolicy::Vm => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_skip_state_failures, StreamDecodePolicy};

    #[test]
    fn native_stream_keeps_decode_skip_enabled() {
        assert!(decode_skip_state_failures(StreamDecodePolicy::Native));
    }

    #[test]
    fn vm_stream_uses_strict_decode_failures() {
        assert!(!decode_skip_state_failures(StreamDecodePolicy::Vm));
    }
}
