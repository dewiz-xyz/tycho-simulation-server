mod allocation;
mod calldata;
mod error;
mod model;
mod normalize;
mod request;
mod resimulate;
mod response;
mod tycho_swaps;
mod wire;

#[cfg(test)]
mod test_support;

pub use error::{EncodeError, EncodeErrorKind};

use crate::models::messages::{RouteEncodeRequest, RouteEncodeResponse};
use crate::models::state::AppState;

pub async fn encode_route(
    state: AppState,
    request: RouteEncodeRequest,
) -> Result<RouteEncodeResponse, EncodeError> {
    let chain = request::validate_chain(request.chain_id)?;
    request::validate_swap_kinds(&request)?;

    let token_in = wire::parse_address(&request.token_in)?;
    let token_out = wire::parse_address(&request.token_out)?;
    let amount_in = wire::parse_amount(&request.amount_in)?;
    let min_amount_out = wire::parse_amount(&request.min_amount_out)?;
    let router_address = wire::parse_address(&request.tycho_router_address)?;
    // Guard against panics in downstream EVM encoding (uint256 inputs).
    wire::biguint_to_u256_checked(&amount_in, "amountIn")?;
    wire::biguint_to_u256_checked(&min_amount_out, "minAmountOut")?;
    request::ensure_erc20_tokens(chain, &token_in, &token_out)?;

    let native_address = chain.native_token().address;
    let normalized =
        normalize::normalize_route(&request, &token_in, &token_out, &amount_in, &native_address)?;
    let resimulated =
        resimulate::resimulate_route(&state, &normalized, chain, &token_in, &token_out).await?;
    response::log_resimulation_amounts(request.request_id.as_deref(), &resimulated);
    let encoder = calldata::build_encoder(chain, router_address.clone())?;
    let expected_total = response::compute_expected_total(&resimulated);
    if expected_total < min_amount_out {
        return Err(EncodeError::simulation(
            "Route expectedAmountOut below minAmountOut",
        ));
    }
    let route_context = calldata::RouteContext {
        request: &request,
        token_in: &token_in,
        token_out: &token_out,
        amount_in: &amount_in,
        router_address: &router_address,
    };
    let router_call = calldata::build_route_calldata_tx(
        &route_context,
        &resimulated,
        encoder.as_ref(),
        &min_amount_out,
    )?;
    let reset_approval =
        request::should_reset_allowance(&state.reset_allowance_tokens, request.chain_id, &token_in);
    let interactions = calldata::build_settlement_interactions(
        &token_in,
        &amount_in,
        router_call,
        reset_approval,
    )?;

    let debug = response::build_debug(&state, &request).await;

    Ok(RouteEncodeResponse {
        interactions,
        debug,
    })
}
