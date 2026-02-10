use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use chrono::NaiveDateTime;
use num_bigint::BigUint;
use num_traits::Zero;
use tycho_execution::encoding::errors::EncodingError;
use tycho_execution::encoding::models::{EncodedSolution, Solution, Transaction};
use tycho_execution::encoding::tycho_encoder::TychoEncoder;
use tycho_simulation::protocol::models::ProtocolComponent;
use tycho_simulation::tycho_common::models::token::Token;
use tycho_simulation::tycho_common::models::Chain;
use tycho_simulation::tycho_common::simulation::protocol_sim::ProtocolSim;
use tycho_simulation::tycho_common::Bytes;

use crate::models::messages::PoolRef;

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

pub(super) fn component_with_tokens(id: &str, tokens: Vec<Token>) -> ProtocolComponent {
    ProtocolComponent::new(
        Bytes::from_str(id).unwrap(),
        "uniswap_v2".to_string(),
        "uniswap_v2".to_string(),
        Chain::Ethereum,
        tokens,
        Vec::new(),
        HashMap::new(),
        Bytes::default(),
        NaiveDateTime::default(),
    )
}

pub(super) fn dummy_token(address: &str) -> Token {
    let bytes = Bytes::from_str(address).unwrap();
    Token::new(&bytes, "TKN", 18, 0, &[], Chain::Ethereum, 100)
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
