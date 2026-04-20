use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use crate::models::tokens::TokenStore;
use anyhow::{bail, Result};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::info;
use tycho_simulation::{
    evm::{
        engine_db::tycho_db::PreCachedDB,
        protocol::{
            aerodrome_slipstreams::state::AerodromeSlipstreamsState,
            ekubo::state::EkuboState,
            ekubo_v3::state::EkuboV3State,
            erc4626::state::ERC4626State,
            filters::{balancer_v2_pool_filter, erc4626_filter, fluid_v1_paused_pools_filter},
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
            liquorice::{client_builder::LiquoriceClientBuilder, state::LiquoriceState},
        },
        stream::RFQStreamBuilder,
    },
    tycho_client::feed::component_tracker::ComponentFilter,
    tycho_common::{
        models::{token::Token, Chain},
        Bytes,
    },
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
        item.map_err(|err| -> Box<dyn std::error::Error + Send + Sync + 'static> { Box::new(err) })
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
        "rocketpool" => Ok(builder.exchange::<RocketpoolState>(protocol, tvl_filter.clone(), None)),
        "ekubo_v3" => Ok(builder.exchange::<EkuboV3State>(protocol, tvl_filter.clone(), None)),
        "aerodrome_slipstreams" => {
            Ok(builder.exchange::<AerodromeSlipstreamsState>(protocol, tvl_filter.clone(), None))
        }
        "erc4626" => Ok(builder.exchange::<ERC4626State>(
            protocol,
            tvl_filter.clone(),
            Some(erc4626_filter),
        )),
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
        item.map_err(|err| -> Box<dyn std::error::Error + Send + Sync + 'static> { Box::new(err) })
    }))
}

#[derive(Clone)]
pub struct RFQConfig {
    pub bebop_user: String,
    pub bebop_key: String,
    pub hashflow_user: String,
    pub hashflow_key: String,
    pub liquorice_user: String,
    pub liquorice_key: String,
}

#[derive(Clone)]
pub struct RFQTokenStores {
    pub tokens: Arc<TokenStore>,
    pub bebop: Arc<TokenStore>,
    pub hashflow: Arc<TokenStore>,
    pub liquorice: Arc<TokenStore>,
}

pub async fn build_rfq_stream(
    tvl_add_threshold: f64,
    token_stores: RFQTokenStores,
    chain: Chain,
    protocols: &[String],
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
    let mut rfq_builder = RFQStreamBuilder::new();

    if rfq_protocol_enabled(protocols, "rfq:bebop") {
        info!("Setting up Bebop RFQ client...\n");
        let (user, key) = (rfq_config.bebop_user.clone(), rfq_config.bebop_key.clone());
        let mut rfq_tokens_bebop = HashSet::new();
        for bebop_token_addr in token_stores.bebop.snapshot().await.keys().clone() {
            rfq_tokens_bebop.insert(bebop_token_addr.clone());
        }
        let bebop_client = BebopClientBuilder::new(chain, user, key)
            .tokens(rfq_tokens_bebop)
            .tvl_threshold(tvl_add_threshold)
            .build()
            .map_err(|err| anyhow::anyhow!("failed to create Bebop RFQ client: {err}"))?;
        rfq_builder = rfq_builder.add_client::<BebopState>("bebop", Box::new(bebop_client));
    }

    if rfq_protocol_enabled(protocols, "rfq:hashflow") {
        info!("Setting up Hashflow RFQ client...\n");
        let (user, key) = (
            rfq_config.hashflow_user.clone(),
            rfq_config.hashflow_key.clone(),
        );
        let mut rfq_tokens_hashflow = HashSet::new();
        for hashflow_token_addr in token_stores.hashflow.snapshot().await.keys() {
            rfq_tokens_hashflow.insert(hashflow_token_addr.clone());
        }
        let hashflow_client = HashflowClientBuilder::new(chain, user, key)
            .tokens(rfq_tokens_hashflow)
            .tvl_threshold(tvl_add_threshold)
            .poll_time(Duration::from_secs(5))
            .build()
            .map_err(|err| anyhow::anyhow!("failed to create Hashflow RFQ client: {err}"))?;
        rfq_builder =
            rfq_builder.add_client::<HashflowState>("hashflow", Box::new(hashflow_client));
    }

    if rfq_protocol_enabled(protocols, "rfq:liquorice") {
        info!("Setting up Liquorice RFQ client...\n");
        let (user, key) = (
            rfq_config.liquorice_user.clone(),
            rfq_config.liquorice_key.clone(),
        );
        let mut rfq_tokens_liquorice = HashSet::new();
        for liquorice_token_addr in token_stores.liquorice.snapshot().await.keys() {
            rfq_tokens_liquorice.insert(liquorice_token_addr.clone());
        }
        let liquorice_client = LiquoriceClientBuilder::new(chain, user, key)
            .tokens(rfq_tokens_liquorice)
            .tvl_threshold(tvl_add_threshold)
            .poll_time(Duration::from_secs(5))
            .build()
            .map_err(|err| anyhow::anyhow!("failed to create Liquorice RFQ client: {err}"))?;
        rfq_builder =
            rfq_builder.add_client::<LiquoriceState>("liquorice", Box::new(liquorice_client));
    }

    info!("Building RFQ Stream...\n");
    let (tx, /* mut */ rx) = mpsc::channel::<Update>(100);

    let decoder_tokens = rfq_decoder_tokens(
        &token_stores.tokens,
        &token_stores.bebop,
        &token_stores.hashflow,
        &token_stores.liquorice,
    )
    .await;
    rfq_builder = rfq_builder.set_tokens(decoder_tokens).await;
    tokio::spawn(rfq_builder.build(tx));
    info!("Connected to RFQs! Streaming live price levels...\n");

    // todo consider implementing register_rfq_protocol...

    Ok(ReceiverStream::new(rx).map(Ok).boxed())
}

fn rfq_protocol_enabled(protocols: &[String], protocol: &str) -> bool {
    protocols.iter().any(|configured| configured == protocol)
}

async fn rfq_decoder_tokens(
    tokens: &TokenStore,
    bebop_tokens: &TokenStore,
    hashflow_tokens: &TokenStore,
    liquorice_tokens: &TokenStore,
) -> HashMap<Bytes, Token> {
    let mut snapshot = tokens.snapshot().await;
    merge_missing_tokens(&mut snapshot, bebop_tokens.snapshot().await);
    merge_missing_tokens(&mut snapshot, hashflow_tokens.snapshot().await);
    merge_missing_tokens(&mut snapshot, liquorice_tokens.snapshot().await);
    snapshot
}

fn merge_missing_tokens(tokens: &mut HashMap<Bytes, Token>, extra: HashMap<Bytes, Token>) {
    for (address, token) in extra {
        tokens.entry(address).or_insert(token);
    }
}

fn register_vm_protocol(
    builder: ProtocolStreamBuilder,
    protocol: &str,
    tvl_filter: &ComponentFilter,
) -> Result<ProtocolStreamBuilder> {
    match protocol {
        "vm:balancer_v2" => Ok(builder.exchange::<EVMPoolState<PreCachedDB>>(
            protocol,
            tvl_filter.clone(),
            Some(balancer_v2_pool_filter),
        )),
        "vm:curve" | "vm:maverick_v2" => {
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
    matches!(policy, StreamDecodePolicy::Native | StreamDecodePolicy::Vm)
}

#[cfg(test)]
#[expect(
    clippy::panic,
    reason = "stream builder tests use explicit panics for invalid fixture setup"
)]
mod tests {
    use std::{
        collections::{BTreeSet, HashMap},
        str::FromStr,
        sync::Arc,
        time::Duration,
    };

    use super::{
        decode_skip_state_failures, merge_missing_tokens, register_native_protocol,
        register_vm_protocol, rfq_decoder_tokens, StreamDecodePolicy,
    };
    use crate::config::{
        BASE_NATIVE_PROTOCOLS, BASE_VM_PROTOCOLS, ETHEREUM_NATIVE_PROTOCOLS, ETHEREUM_VM_PROTOCOLS,
    };
    use tycho_simulation::{
        evm::stream::ProtocolStreamBuilder,
        tycho_client::feed::component_tracker::ComponentFilter,
        tycho_common::{
            models::{token::Token, Chain},
            Bytes,
        },
    };

    use crate::models::tokens::TokenStore;

    fn test_builder() -> ProtocolStreamBuilder {
        ProtocolStreamBuilder::new(
            "localhost",
            tycho_simulation::tycho_common::models::Chain::Ethereum,
        )
    }

    fn test_filter() -> ComponentFilter {
        ComponentFilter::with_tvl_range(0.0, 1.0)
    }

    fn unique_protocols<'a>(left: &'a [&'a str], right: &'a [&'a str]) -> BTreeSet<&'a str> {
        left.iter().chain(right.iter()).copied().collect()
    }

    fn test_token(address: &str, symbol: &str) -> Token {
        let address = match Bytes::from_str(address) {
            Ok(address) => address,
            Err(err) => panic!("test token address must parse: {err}"),
        };
        Token::new(&address, symbol, 18, 0, &[], Chain::Ethereum, 100)
    }

    fn test_token_store(tokens: impl IntoIterator<Item = Token>) -> Arc<TokenStore> {
        let initial_tokens = tokens
            .into_iter()
            .map(|token| (token.address.clone(), token))
            .collect();
        Arc::new(TokenStore::new(
            initial_tokens,
            "http://localhost".to_string(),
            "test".to_string(),
            Chain::Ethereum,
            Duration::from_millis(10),
        ))
    }

    #[test]
    fn rfq_protocol_enabled_matches_exact_protocol_name() {
        let protocols = vec![
            "rfq:bebop".to_string(),
            "rfq:hashflow".to_string(),
            "rfq:liquorice".to_string(),
        ];

        assert!(super::rfq_protocol_enabled(&protocols, "rfq:bebop"));
        assert!(super::rfq_protocol_enabled(&protocols, "rfq:hashflow"));
        assert!(super::rfq_protocol_enabled(&protocols, "rfq:liquorice"));
        assert!(!super::rfq_protocol_enabled(&protocols, "rfq:other"));
    }

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
        let builder = test_builder();
        let filter = test_filter();
        let result = register_native_protocol(builder, "unknown_protocol", &filter);
        assert!(result.is_err());
        let Err(err) = result else {
            unreachable!("expected error for unknown native protocol");
        };
        assert!(err.to_string().contains("Unknown native protocol"));
    }

    #[test]
    fn unknown_vm_protocol_returns_error() {
        let builder = test_builder();
        let filter = test_filter();
        let result = register_vm_protocol(builder, "vm:unknown", &filter);
        assert!(result.is_err());
        let Err(err) = result else {
            unreachable!("expected error for unknown VM protocol");
        };
        assert!(err.to_string().contains("Unknown VM protocol"));
    }

    #[test]
    fn chain_profile_native_protocols_all_register_successfully() {
        let filter = test_filter();

        for protocol in unique_protocols(ETHEREUM_NATIVE_PROTOCOLS, BASE_NATIVE_PROTOCOLS) {
            let result = register_native_protocol(test_builder(), protocol, &filter);
            assert!(
                result.is_ok(),
                "expected native protocol {protocol} to register"
            );
        }
    }

    #[test]
    fn chain_profile_vm_protocols_all_register_successfully() {
        let filter = test_filter();

        for protocol in unique_protocols(ETHEREUM_VM_PROTOCOLS, BASE_VM_PROTOCOLS) {
            let result = register_vm_protocol(test_builder(), protocol, &filter);
            assert!(
                result.is_ok(),
                "expected VM protocol {protocol} to register"
            );
        }
    }

    #[test]
    fn merge_missing_tokens_keeps_existing_entries() {
        let shared = test_token("0x0000000000000000000000000000000000000001", "BASE");
        let replacement = test_token("0x0000000000000000000000000000000000000001", "RFQ");
        let mut tokens = HashMap::from([(shared.address.clone(), shared.clone())]);

        merge_missing_tokens(
            &mut tokens,
            HashMap::from([(replacement.address.clone(), replacement)]),
        );

        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[&shared.address].symbol, shared.symbol);
    }

    #[tokio::test]
    async fn rfq_decoder_tokens_include_rfq_only_assets() {
        let base = test_token("0x0000000000000000000000000000000000000001", "BASE");
        let bebop_only = test_token("0x0000000000000000000000000000000000000002", "BEBOP");
        let hashflow_only = test_token("0x0000000000000000000000000000000000000003", "HASH");
        let liquorice_only = test_token("0x0000000000000000000000000000000000000004", "LIQ");

        let tokens = test_token_store(vec![base.clone()]);
        let bebop_tokens = test_token_store(vec![bebop_only.clone()]);
        let hashflow_tokens = test_token_store(vec![hashflow_only.clone()]);
        let liquorice_tokens = test_token_store(vec![liquorice_only.clone()]);

        let merged =
            rfq_decoder_tokens(&tokens, &bebop_tokens, &hashflow_tokens, &liquorice_tokens).await;

        assert_eq!(merged.len(), 4);
        assert!(merged.contains_key(&base.address));
        assert!(merged.contains_key(&bebop_only.address));
        assert!(merged.contains_key(&hashflow_only.address));
        assert!(merged.contains_key(&liquorice_only.address));
    }
}
