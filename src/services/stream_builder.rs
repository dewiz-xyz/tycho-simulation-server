use std::sync::Arc;

use anyhow::{bail, Result};
use futures::StreamExt;
use tracing::info;
use tycho_simulation::{
    evm::{
        engine_db::tycho_db::PreCachedDB,
        protocol::{
            ekubo::state::EkuboState,
            ekubo_v3::state::EkuboV3State,
            filters::{balancer_v2_pool_filter, curve_pool_filter, fluid_v1_paused_pools_filter},
            fluid::FluidV1,
            pancakeswap_v2::state::PancakeswapV2State,
            // rocketpool::state::RocketpoolState,
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
    chain: Chain,
    protocols: &[String],
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
        chain,
    );

    for protocol in protocols {
        builder = register_native_protocol(builder, protocol, &tvl_filter)?;
    }

    let snapshot = tokens.snapshot().await;
    let stream = builder.set_tokens(snapshot).await.build().await?;

    Ok(stream.map(|item| {
        item.map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync + 'static>)
    }))
}

fn register_native_protocol(
    builder: ProtocolStreamBuilder,
    protocol: &str,
    tvl_filter: &ComponentFilter,
) -> Result<ProtocolStreamBuilder> {
    match protocol {
        "uniswap_v2" | "sushiswap_v2" => {
            Ok(builder.exchange::<UniswapV2State>(protocol, tvl_filter.clone(), None))
        }
        "pancakeswap_v2" => {
            Ok(builder.exchange::<PancakeswapV2State>(protocol, tvl_filter.clone(), None))
        }
        "uniswap_v3" | "pancakeswap_v3" => {
            Ok(builder.exchange::<UniswapV3State>(protocol, tvl_filter.clone(), None))
        }
        "uniswap_v4" => Ok(builder.exchange::<UniswapV4State>(protocol, tvl_filter.clone(), None)),
        "ekubo_v2" => Ok(builder.exchange::<EkuboState>(protocol, tvl_filter.clone(), None)),
        "fluid_v1" => Ok(builder.exchange::<FluidV1>(
            protocol,
            tvl_filter.clone(),
            Some(fluid_v1_paused_pools_filter),
        )),
        "ekubo_v3" => Ok(builder.exchange::<EkuboV3State>(protocol, tvl_filter.clone(), None)),
        other => bail!("Unknown native protocol in chain profile: {}", other),
    }
}

pub async fn build_vm_stream(
    tycho_url: &str,
    api_key: &str,
    tvl_add_threshold: f64,
    tvl_keep_threshold: f64,
    tokens: Arc<TokenStore>,
    chain: Chain,
    protocols: &[String],
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
        chain,
    );

    for protocol in protocols {
        builder = register_vm_protocol(builder, protocol, &tvl_filter)?;
    }

    let snapshot = tokens.snapshot().await;
    let stream = builder.set_tokens(snapshot).await.build().await?;

    Ok(stream.map(|item| {
        item.map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync + 'static>)
    }))
}

fn register_vm_protocol(
    builder: ProtocolStreamBuilder,
    protocol: &str,
    tvl_filter: &ComponentFilter,
) -> Result<ProtocolStreamBuilder> {
    match protocol {
        "vm:curve" => Ok(builder.exchange::<EVMPoolState<PreCachedDB>>(
            protocol,
            tvl_filter.clone(),
            Some(curve_pool_filter),
        )),
        "vm:balancer_v2" => Ok(builder.exchange::<EVMPoolState<PreCachedDB>>(
            protocol,
            tvl_filter.clone(),
            Some(balancer_v2_pool_filter),
        )),
        "vm:maverick_v2" => {
            Ok(builder.exchange::<EVMPoolState<PreCachedDB>>(protocol, tvl_filter.clone(), None))
        }
        other => bail!("Unknown VM protocol in chain profile: {}", other),
    }
}

fn base_builder(
    tycho_url: &str,
    api_key: &str,
    tvl_add_threshold: f64,
    tvl_keep_threshold: f64,
    skip_state_decode_failures: bool,
    chain: Chain,
) -> (ProtocolStreamBuilder, ComponentFilter) {
    let add_tvl = tvl_add_threshold;
    let keep_tvl = tvl_keep_threshold.min(add_tvl);
    info!(
        "Using TVL thresholds: remove/keep={} add={}",
        keep_tvl, add_tvl
    );
    let tvl_filter = ComponentFilter::with_tvl_range(keep_tvl, add_tvl);

    let builder = ProtocolStreamBuilder::new(tycho_url, chain)
        .latency_buffer(15)
        .auth_key(Some(api_key.to_string()))
        .skip_state_decode_failures(skip_state_decode_failures);

    (builder, tvl_filter)
}

fn decode_skip_state_failures(policy: StreamDecodePolicy) -> bool {
    match policy {
        StreamDecodePolicy::Native => true,
        StreamDecodePolicy::Vm => true,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        decode_skip_state_failures, register_native_protocol, register_vm_protocol,
        StreamDecodePolicy,
    };
    use tycho_simulation::{
        evm::stream::ProtocolStreamBuilder, tycho_client::feed::component_tracker::ComponentFilter,
    };

    #[test]
    fn native_stream_keeps_decode_skip_enabled() {
        assert!(decode_skip_state_failures(StreamDecodePolicy::Native));
    }

    #[test]
    fn vm_stream_keeps_decode_skip_enabled() {
        assert!(decode_skip_state_failures(StreamDecodePolicy::Vm));
    }

    #[test]
    fn unknown_native_protocol_returns_error() {
        let builder = ProtocolStreamBuilder::new(
            "localhost",
            tycho_simulation::tycho_common::models::Chain::Ethereum,
        );
        let filter = ComponentFilter::with_tvl_range(0.0, 1.0);
        let result = register_native_protocol(builder, "unknown_protocol", &filter);
        assert!(result.is_err());
        let err = result
            .err()
            .expect("expected error for unknown native protocol");
        assert!(err.to_string().contains("Unknown native protocol"));
    }

    #[test]
    fn unknown_vm_protocol_returns_error() {
        let builder = ProtocolStreamBuilder::new(
            "localhost",
            tycho_simulation::tycho_common::models::Chain::Ethereum,
        );
        let filter = ComponentFilter::with_tvl_range(0.0, 1.0);
        let result = register_vm_protocol(builder, "vm:unknown", &filter);
        assert!(result.is_err());
        let err = result
            .err()
            .expect("expected error for unknown VM protocol");
        assert!(err.to_string().contains("Unknown VM protocol"));
    }
}
