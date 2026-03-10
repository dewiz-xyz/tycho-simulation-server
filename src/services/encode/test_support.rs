use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDateTime;
use num_bigint::BigUint;
use num_traits::Zero;
use tokio::sync::{RwLock, Semaphore};
use tycho_execution::encoding::errors::EncodingError;
use tycho_execution::encoding::models::{EncodedSolution, Solution, Transaction};
use tycho_execution::encoding::tycho_encoder::TychoEncoder;
use tycho_simulation::protocol::models::ProtocolComponent;
use tycho_simulation::tycho_common::models::token::Token;
use tycho_simulation::tycho_common::models::Chain;
use tycho_simulation::tycho_common::simulation::protocol_sim::ProtocolSim;
use tycho_simulation::tycho_common::Bytes;

use crate::models::messages::PoolRef;
use crate::models::state::{AppState, StateStore, VmStreamStatus};
use crate::models::stream_health::StreamHealth;
use crate::models::tokens::TokenStore;

pub(super) fn pool_ref(id: &str) -> PoolRef {
    PoolRef {
        protocol: "uniswap_v2".to_string(),
        component_id: id.to_string(),
        pool_address: None,
    }
}

pub(super) fn dummy_component() -> ProtocolComponent {
    component_with_tokens(
        "0x0000000000000000000000000000000000000009",
        vec![
            dummy_token("0x0000000000000000000000000000000000000001"),
            dummy_token("0x0000000000000000000000000000000000000002"),
        ],
    )
}

pub(super) fn component_with_protocol(
    id: &str,
    protocol_system: &str,
    protocol_type_name: &str,
    tokens: Vec<Token>,
) -> ProtocolComponent {
    ProtocolComponent::new(
        Bytes::from_str(id).unwrap(),
        protocol_system.to_string(),
        protocol_type_name.to_string(),
        Chain::Ethereum,
        tokens,
        Vec::new(),
        HashMap::new(),
        Bytes::default(),
        NaiveDateTime::default(),
    )
}

pub(super) fn component_with_tokens(id: &str, tokens: Vec<Token>) -> ProtocolComponent {
    component_with_protocol(id, "uniswap_v2", "uniswap_v2", tokens)
}

pub(super) fn dummy_token(address: &str) -> Token {
    let bytes = Bytes::from_str(address).unwrap();
    Token::new(&bytes, "TKN", 18, 0, &[], Chain::Ethereum, 100)
}

pub(super) fn token_store_with_tokens(tokens: impl IntoIterator<Item = Token>) -> Arc<TokenStore> {
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

pub(super) fn test_state_stores(
    token_store: &Arc<TokenStore>,
) -> (Arc<StateStore>, Arc<StateStore>) {
    (
        Arc::new(StateStore::new(Arc::clone(token_store))),
        Arc::new(StateStore::new(Arc::clone(token_store))),
    )
}

pub(super) struct TestAppStateConfig {
    pub(super) enable_vm_pools: bool,
    pub(super) quote_timeout: Duration,
    pub(super) pool_timeout_native: Duration,
    pub(super) pool_timeout_vm: Duration,
    pub(super) request_timeout: Duration,
}

impl Default for TestAppStateConfig {
    fn default() -> Self {
        Self {
            enable_vm_pools: false,
            quote_timeout: Duration::from_millis(10),
            pool_timeout_native: Duration::from_millis(10),
            pool_timeout_vm: Duration::from_millis(10),
            request_timeout: Duration::from_millis(10),
        }
    }
}

pub(super) fn test_app_state(
    token_store: Arc<TokenStore>,
    native_state_store: Arc<StateStore>,
    vm_state_store: Arc<StateStore>,
    config: TestAppStateConfig,
) -> AppState {
    AppState {
        tokens: token_store,
        native_state_store,
        vm_state_store,
        native_stream_health: Arc::new(StreamHealth::new()),
        vm_stream_health: Arc::new(StreamHealth::new()),
        vm_stream: Arc::new(RwLock::new(VmStreamStatus::default())),
        latest_native_gas_price_wei: Arc::new(RwLock::new(None)),
        native_gas_price_reporting_enabled: Arc::new(RwLock::new(false)),
        enable_vm_pools: config.enable_vm_pools,
        readiness_stale: Duration::from_secs(120),
        quote_timeout: config.quote_timeout,
        pool_timeout_native: config.pool_timeout_native,
        pool_timeout_vm: config.pool_timeout_vm,
        request_timeout: config.request_timeout,
        native_sim_semaphore: Arc::new(Semaphore::new(1)),
        vm_sim_semaphore: Arc::new(Semaphore::new(1)),
        reset_allowance_tokens: Arc::new(HashMap::new()),
        native_sim_concurrency: 1,
        vm_sim_concurrency: 1,
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(super) struct MockProtocolSim;

#[typetag::serde]
impl ProtocolSim for MockProtocolSim {
    fn fee(&self) -> f64 {
        0.0
    }

    fn spot_price(
        &self,
        _base: &Token,
        _quote: &Token,
    ) -> Result<f64, tycho_simulation::tycho_common::simulation::errors::SimulationError> {
        Ok(0.0)
    }

    fn get_amount_out(
        &self,
        amount_in: BigUint,
        _token_in: &Token,
        _token_out: &Token,
    ) -> Result<
        tycho_simulation::tycho_common::simulation::protocol_sim::GetAmountOutResult,
        tycho_simulation::tycho_common::simulation::errors::SimulationError,
    > {
        Ok(
            tycho_simulation::tycho_common::simulation::protocol_sim::GetAmountOutResult::new(
                amount_in,
                BigUint::zero(),
                self.clone_box(),
            ),
        )
    }

    fn get_limits(
        &self,
        _sell_token: Bytes,
        _buy_token: Bytes,
    ) -> Result<
        (BigUint, BigUint),
        tycho_simulation::tycho_common::simulation::errors::SimulationError,
    > {
        Ok((BigUint::zero(), BigUint::zero()))
    }

    fn delta_transition(
        &mut self,
        _delta: tycho_simulation::tycho_common::dto::ProtocolStateDelta,
        _tokens: &HashMap<Bytes, Token>,
        _balances: &tycho_simulation::tycho_common::simulation::protocol_sim::Balances,
    ) -> Result<(), tycho_simulation::tycho_common::simulation::errors::TransitionError<String>>
    {
        Ok(())
    }

    fn clone_box(&self) -> Box<dyn ProtocolSim> {
        Box::new(Self {})
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn eq(&self, _other: &dyn ProtocolSim) -> bool {
        true
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(super) struct StepProtocolSim {
    pub(super) multiplier: u32,
}

#[typetag::serde]
impl ProtocolSim for StepProtocolSim {
    fn fee(&self) -> f64 {
        0.0
    }

    fn spot_price(
        &self,
        _base: &Token,
        _quote: &Token,
    ) -> Result<f64, tycho_simulation::tycho_common::simulation::errors::SimulationError> {
        Ok(0.0)
    }

    fn get_amount_out(
        &self,
        amount_in: BigUint,
        _token_in: &Token,
        _token_out: &Token,
    ) -> Result<
        tycho_simulation::tycho_common::simulation::protocol_sim::GetAmountOutResult,
        tycho_simulation::tycho_common::simulation::errors::SimulationError,
    > {
        let amount_out = amount_in * BigUint::from(self.multiplier);
        let next = StepProtocolSim {
            multiplier: self.multiplier + 1,
        };
        Ok(
            tycho_simulation::tycho_common::simulation::protocol_sim::GetAmountOutResult::new(
                amount_out,
                BigUint::zero(),
                Box::new(next),
            ),
        )
    }

    fn get_limits(
        &self,
        _sell_token: Bytes,
        _buy_token: Bytes,
    ) -> Result<
        (BigUint, BigUint),
        tycho_simulation::tycho_common::simulation::errors::SimulationError,
    > {
        Ok((BigUint::zero(), BigUint::zero()))
    }

    fn delta_transition(
        &mut self,
        _delta: tycho_simulation::tycho_common::dto::ProtocolStateDelta,
        _tokens: &HashMap<Bytes, Token>,
        _balances: &tycho_simulation::tycho_common::simulation::protocol_sim::Balances,
    ) -> Result<(), tycho_simulation::tycho_common::simulation::errors::TransitionError<String>>
    {
        Ok(())
    }

    fn clone_box(&self) -> Box<dyn ProtocolSim> {
        Box::new(Self {
            multiplier: self.multiplier,
        })
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn eq(&self, other: &dyn ProtocolSim) -> bool {
        other
            .as_any()
            .downcast_ref::<StepProtocolSim>()
            .is_some_and(|rhs| rhs.multiplier == self.multiplier)
    }
}

pub(super) fn step_multiplier(state: &Arc<dyn ProtocolSim>) -> u32 {
    state
        .as_any()
        .downcast_ref::<StepProtocolSim>()
        .expect("Expected StepProtocolSim")
        .multiplier
}

pub(super) struct MockTychoEncoder {
    signature: String,
    router: Bytes,
    swaps: Vec<u8>,
}

impl MockTychoEncoder {
    pub(super) fn new(signature: &str, router: Bytes) -> Self {
        Self {
            signature: signature.to_string(),
            router,
            swaps: vec![0x01, 0x02],
        }
    }
}

impl TychoEncoder for MockTychoEncoder {
    fn encode_solutions(
        &self,
        _solutions: Vec<Solution>,
    ) -> Result<Vec<EncodedSolution>, EncodingError> {
        Ok(vec![EncodedSolution {
            swaps: self.swaps.clone(),
            interacting_with: self.router.clone(),
            function_signature: self.signature.clone(),
            n_tokens: 0,
            permit: None,
        }])
    }

    fn encode_full_calldata(
        &self,
        _solutions: Vec<Solution>,
    ) -> Result<Vec<Transaction>, EncodingError> {
        Err(EncodingError::FatalError(
            "encode_full_calldata not supported in tests".to_string(),
        ))
    }

    fn validate_solution(&self, _solution: &Solution) -> Result<(), EncodingError> {
        Ok(())
    }
}
