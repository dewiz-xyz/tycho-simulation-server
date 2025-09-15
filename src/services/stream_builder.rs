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
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info, warn};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::sleep;
use tycho_simulation::tycho_core::Bytes;

#[derive(Debug)]
struct ForwardError(String);

impl std::fmt::Display for ForwardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ForwardError {}

#[derive(Clone, Copy, Debug)]
enum ProtocolKind {
    UniswapV2,
    UniswapV3,
    UniswapV4,
    Curve,
}

fn protocol_name(kind: ProtocolKind) -> &'static str {
    match kind {
        ProtocolKind::UniswapV2 => "uniswap_v2",
        ProtocolKind::UniswapV3 => "uniswap_v3",
        ProtocolKind::UniswapV4 => "uniswap_v4",
        ProtocolKind::Curve => "vm:curve",
    }
}

async fn build_single_protocol_stream(
    tycho_url: &str,
    api_key: &str,
    kind: ProtocolKind,
    tvl_filter: ComponentFilter,
    tokens: HashMap<Bytes, Token>,
) -> Result<
    impl futures::Stream<
            Item = Result<tycho_simulation::protocol::models::BlockUpdate, impl std::error::Error>,
        > + Unpin
        + Send,
> {
    let mut builder = ProtocolStreamBuilder::new(tycho_url, Chain::Ethereum.into())
        .timeout(15)
        .auth_key(Some(api_key.to_string()))
        .skip_state_decode_failures(true);

    builder = match kind {
        ProtocolKind::UniswapV2 => builder.exchange::<UniswapV2State>(protocol_name(kind), tvl_filter, None),
        ProtocolKind::UniswapV3 => builder.exchange::<UniswapV3State>(protocol_name(kind), tvl_filter, None),
        ProtocolKind::UniswapV4 => builder.exchange::<UniswapV4State>(protocol_name(kind), tvl_filter, None),
        ProtocolKind::Curve => builder.exchange::<EVMPoolState<PreCachedDB>>(protocol_name(kind), tvl_filter, Some(curve_pool_filter)),
    };

    let stream = builder.set_tokens(tokens).await.build().await?;
    Ok(stream)
}

async fn spawn_protocol_task(
    tycho_url: String,
    api_key: String,
    kind: ProtocolKind,
    tvl_filter: ComponentFilter,
    tokens: HashMap<Bytes, Token>,
    tx: mpsc::Sender<
        Result<
            tycho_simulation::protocol::models::BlockUpdate,
            Box<dyn std::error::Error + Send + Sync + 'static>,
        >,
    >,
) {
    let name = protocol_name(kind);
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(30);

    loop {
        info!("{}: connecting stream", name);
        match build_single_protocol_stream(&tycho_url, &api_key, kind, tvl_filter.clone(), tokens.clone()).await {
            Ok(mut stream) => {
                info!("{}: stream connected", name);
                while let Some(item) = stream.next().await {
                    let mapped = item.map_err(|e| {
                        let msg = format!("{}: {}", name, e);
                        Box::new(ForwardError(msg)) as Box<dyn std::error::Error + Send + Sync + 'static>
                    });
                    if tx.send(mapped).await.is_err() {
                        warn!("{}: receiver dropped; stopping forwarder", name);
                        return;
                    }
                }
                warn!("{}: stream ended; will reconnect after backoff", name);
            }
            Err(e) => {
                error!("{}: failed to build stream: {}; will retry after backoff", name, e);
            }
        }
        sleep(backoff).await;
        backoff = std::cmp::min(backoff * 2, max_backoff);
    }
}

pub async fn build_merged_streams(
    tycho_url: &str,
    api_key: &str,
    tvl_add_threshold: f64,
    tvl_keep_threshold: f64,
    tokens: HashMap<Bytes, Token>,
) -> Result<
    impl futures::Stream<
            Item = Result<
                tycho_simulation::protocol::models::BlockUpdate,
                Box<dyn std::error::Error + Send + Sync + 'static>,
            >,
        > + Unpin
        + Send,
> {
    // Use configured thresholds (remove/keep first, add second)
    let add_tvl = tvl_add_threshold;
    let keep_tvl = tvl_keep_threshold.min(add_tvl);
    info!(
        "Using TVL thresholds: remove/keep={} add={}",
        keep_tvl, add_tvl
    );
    let tvl_filter = ComponentFilter::with_tvl_range(keep_tvl, add_tvl);

    // Create a channel and spawn one forwarder per protocol
    let (tx, rx) = mpsc::channel(1024);
    let url = tycho_url.to_string();
    let key = api_key.to_string();

    let kinds = [
        ProtocolKind::UniswapV2,
        ProtocolKind::UniswapV3,
        ProtocolKind::UniswapV4,
        ProtocolKind::Curve,
    ];

    for kind in kinds {
        let tx_clone = tx.clone();
        let url_clone = url.clone();
        let key_clone = key.clone();
        let filter_clone = tvl_filter.clone();
        let tokens_clone = tokens.clone();
        tokio::spawn(async move {
            spawn_protocol_task(
                url_clone,
                key_clone,
                kind,
                filter_clone,
                tokens_clone,
                tx_clone,
            )
            .await;
        });
    }
    drop(tx); // close original sender; tasks hold clones

    Ok(ReceiverStream::new(rx))
}
