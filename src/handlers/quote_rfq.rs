use std::{collections::HashSet, env, str::FromStr, sync::Arc, time::Duration};

use axum::{extract::State, Json};
use num_bigint::BigUint;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::models::{
    messages::{
        AmountOutRequest, QuoteFailure, QuoteFailureKind, QuoteMeta, QuoteResult, QuoteStatus,
    },
    state::AppState,
};
use alloy::{
    eips::BlockNumberOrTag,
    network::{Ethereum, EthereumWallet},
    primitives::{Address, Bytes as AlloyBytes, Keccak256, Signature, TxKind, B256, U256},
    providers::{
        fillers::{FillProvider, JoinFill, WalletFiller},
        Identity, Provider, ProviderBuilder, RootProvider,
    },
    rpc::types::{
        simulate::{SimBlock, SimulatePayload},
        TransactionInput, TransactionRequest,
    },
    signers::{local::PrivateKeySigner, SignerSync},
    sol_types::{eip712_domain, SolStruct, SolValue},
};
use alloy_chains::NamedChain;

use tycho_simulation::{
    evm::protocol::u256_num::biguint_to_u256,
    protocol::models::{ProtocolComponent, Update},
    rfq::{
        protocols::{
            bebop::{client_builder::BebopClientBuilder, state::BebopState},
            hashflow::{client_builder::HashflowClientBuilder, state::HashflowState},
        },
        stream::RFQStreamBuilder,
    },
    utils::{get_default_url, load_all_tokens},
};

use tycho_common::models::Chain;
use tycho_common::{models::token::Token, simulation::protocol_sim::ProtocolSim, Bytes};

use tycho_execution::encoding::{
    errors::EncodingError,
    evm::{
        approvals::permit2::PermitSingle, encoder_builders::TychoRouterEncoderBuilder,
        swap_encoder::swap_encoder_registry::SwapEncoderRegistry,
    },
    models,
    models::{EncodedSolution, Solution, Swap, Transaction, UserTransferType},
};
struct CancelOnDrop {
    token: CancellationToken,
    armed: bool,
}

impl CancelOnDrop {
    fn new(token: CancellationToken) -> Self {
        Self { token, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

pub async fn simulate_rfq(
    State(state): State<AppState>,
    Json(request): Json<AmountOutRequest>,
    // ) -> Json<QuoteResult> {
) -> () {
    info!("simulate rfq");
    info!("Step 1: Encoding the permit2 transaction...");
    info!("amounts len: {}", request.amounts.len());

    let approve_function_signature = "approve(address,uint256)";
    let swapper_pk = env::var("PRIVATE_KEY").ok();

    let cancel_token = CancellationToken::new();
    let mut cancel_guard = CancelOnDrop::new(cancel_token.clone());
    let request_timeout: std::time::Duration = state.request_timeout();

    let state_for_computation = state.clone();
    let request_for_computation = request.clone();
    // let computation_future = get_amounts_out(
    //     state_for_computation,
    //     request_for_computation,
    //     Some(cancel_token.clone()),
    // );

    let pk_str = swapper_pk.as_ref().unwrap();
    if swapper_pk.is_none() {
        info!("\nSigner private key was not provided. Skipping simulation/execution. Set PRIVATE_KEY env variable to perform simulation/execution.\n");
    }
    let chain = Chain::Ethereum;
    // Initialize the encoder
    let swap_encoder_registry = SwapEncoderRegistry::new(chain)
        .add_default_encoders(None)
        .expect("Failed to get default SwapEncoderRegistry");
    let encoder = TychoRouterEncoderBuilder::new()
        .chain(chain)
        .user_transfer_type(UserTransferType::TransferFromPermit2)
        .swap_encoder_registry(swap_encoder_registry)
        .build()
        .expect("Failed to build encoder");

    let pk = B256::from_str(pk_str).expect("Failed to convert swapper pk to B256");
    let signer = PrivateKeySigner::from_bytes(&pk).expect("Failed to create PrivateKeySigner");
    let tx_signer = EthereumWallet::from(signer.clone());

    let provider: FillProvider<_, RootProvider<Ethereum>, Ethereum> = ProviderBuilder::default()
        .with_chain(NamedChain::try_from(Chain::Ethereum.id()).expect("Invalid chain"))
        .wallet(tx_signer)
        .connect(&env::var("RPC_URL").expect("RPC_URL env var not set"))
        .await
        .expect("Failed to connect provider");

    for amount in request.amounts {
        println!(
            "Looking for RFQ quotes for {amount} {sell_symbol} -> {buy_symbol} on {chain:?}",
            amount = amount,
            sell_symbol = request.token_in,
            buy_symbol = request.token_out
        );
        println!("amount: {}", amount);
        let amount_biguint = BigUint::from_str(&amount).expect("Couldn't convert to BigUint");
        let args = (
            Address::from_str(&request.token_in).expect("Couldn't convert to address"),
            biguint_to_u256(&amount_biguint),
        );
        let approval_data = encode_input(approve_function_signature, args.abi_encode());

        let nonce = provider
            .get_transaction_count(signer.address())
            .await
            .expect("Failed to get nonce");
        let block = provider
            .get_block_by_number(BlockNumberOrTag::Latest)
            .await
            .expect("Failed to fetch latest block")
            .expect("Block not found");
        let base_fee = block
            .header
            .base_fee_per_gas
            .expect("Base fee not available");
        let max_priority_fee_per_gas = 1_000_000_000u64;
        let max_fee_per_gas = base_fee + max_priority_fee_per_gas;

        let approval_request = TransactionRequest {
            to: Some(TxKind::Call(
                Address::from_str(&request.token_in).expect("Couldn't convert to address"),
            )),
            from: Some(signer.address()),
            value: None,
            input: TransactionInput {
                input: Some(AlloyBytes::from(approval_data)),
                data: None,
            },
            gas: Some(100_000u64),
            chain_id: Some(chain.id()),
            max_fee_per_gas: Some(max_fee_per_gas.into()),
            max_priority_fee_per_gas: Some(max_priority_fee_per_gas.into()),
            nonce: Some(nonce),
            ..Default::default()
        };
        info!("Step 2: Encoding the solution transaction...");

        // info!("total states {}", state.state_store.total_states().await);
        // info!("current_block {}", state.state_store.current_block().await);
        // state
        //     .state_store
        //     .wait_until_ready(Duration::from_secs(10))
        //     .await;
        // let solution = create_solution(
        //     component.clone(),
        //     Arc::from(state.clone_box()),
        //     sell_token.clone(),
        //     buy_token.clone(),
        //     amount_in.clone(),
        //     Bytes::from(signer.address().to_vec()),
        //     amount_out.clone(),
        // );
    }
}

/// Encodes the input data for a function call to the given function selector.
pub fn encode_input(selector: &str, mut encoded_args: Vec<u8>) -> Vec<u8> {
    let mut hasher = Keccak256::new();
    hasher.update(selector.as_bytes());
    let selector_bytes = &hasher.finalize()[..4];
    let mut call_data = selector_bytes.to_vec();
    // Remove extra prefix if present (32 bytes for dynamic data)
    // Alloy encoding is including a prefix for dynamic data indicating the offset or length
    // but at this point we don't want that
    if encoded_args.len() > 32
        && encoded_args[..32]
            == [0u8; 31]
                .into_iter()
                .chain([32].to_vec())
                .collect::<Vec<u8>>()
    {
        encoded_args = encoded_args[32..].to_vec();
    }
    call_data.extend(encoded_args);
    call_data
}

#[allow(clippy::too_many_arguments)]
fn create_solution(
    component: ProtocolComponent,
    state: Arc<dyn ProtocolSim>,
    sell_token: Token,
    buy_token: Token,
    sell_amount: BigUint,
    user_address: Bytes,
    expected_amount: BigUint,
) -> Solution {
    // Prepare data to encode. First we need to create a swap object
    // TODO fix type
    // let simple_swap = Swap::new(
    //     component,
    //     sell_token.address.clone(),
    //     buy_token.address.clone(),
    // )
    // .protocol_state(state)
    // .estimated_amount_in(sell_amount.clone());

    // Compute a minimum amount out
    //
    // # ⚠️ Important Responsibility Note
    // For maximum security, in production code, this minimum amount out should be computed
    // from a third-party source.
    let slippage = 0.0025; // 0.25% slippage
    let bps = BigUint::from(10_000u32);
    let slippage_percent = BigUint::from((slippage * 10000.0) as u32);
    let multiplier = &bps - slippage_percent;
    let min_amount_out = (expected_amount * &multiplier) / &bps;

    // Then we create a solution object with the previous swap
    Solution {
        sender: user_address.clone(),
        receiver: user_address,
        given_token: sell_token.address,
        given_amount: sell_amount,
        checked_token: buy_token.address,
        exact_out: false, // it's an exact in solution
        checked_amount: min_amount_out,
        // swaps: vec![simple_swap],
        ..Default::default()
    }
}
