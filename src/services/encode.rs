use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;

use num_bigint::BigUint;
use tracing::warn;
use tycho_execution::encoding::{models::Solution, models::Swap, tycho_encoder::TychoEncoder};
use tycho_simulation::{
    protocol::models::ProtocolComponent,
    tycho_common::{models::protocol::ProtocolComponent as CommonProtocolComponent, Bytes},
};

use crate::models::messages::{
    Allowance, Asset, CalldataConfig, CalldataMode, CalldataResult, CalldataStep, EncodeMeta,
    EncodeRequest, EncodeResult, EncodeRoute, EncodeRouteResult, ExecutionCall, ExecutionCallType,
    QuoteFailure, QuoteFailureKind, QuoteStatus,
};
use crate::models::state::AppState;

const ERC20_TRANSFER_SELECTOR: [u8; 4] = [0xa9, 0x05, 0x9c, 0xbb];

pub async fn encode_routes(state: AppState, request: EncodeRequest) -> EncodeResult {
    let auction_id = request.auction_id.clone();
    let mut failures = Vec::new();

    if request.routes.is_empty() {
        failures.push(invalid_request_failure("routes must not be empty"));
        return invalid_request_result(request.request_id, auction_id, failures);
    }

    let mut seen_routes = HashSet::new();
    for route in &request.routes {
        if !seen_routes.insert(route.route_id.as_str()) {
            failures.push(invalid_request_failure(format!(
                "Duplicate route_id: {}",
                route.route_id
            )));
            return invalid_request_result(request.request_id, auction_id, failures);
        }
    }

    let request_token_in = match parse_address(&request.token_in) {
        Ok(token) => token,
        Err(message) => {
            failures.push(invalid_request_failure(message));
            return invalid_request_result(request.request_id, auction_id, failures);
        }
    };
    let request_token_out = match parse_address(&request.token_out) {
        Ok(token) => token,
        Err(message) => {
            failures.push(invalid_request_failure(message));
            return invalid_request_result(request.request_id, auction_id, failures);
        }
    };

    let request_token_in_norm = normalize_address(&request.token_in);
    let request_token_out_norm = normalize_address(&request.token_out);

    let config = match resolve_calldata_config(&state, request.calldata.as_ref()) {
        Ok(config) => config,
        Err(message) => {
            return invalid_request_result(
                request.request_id,
                auction_id,
                vec![invalid_request_failure(message)],
            );
        }
    };

    let mut data = Vec::with_capacity(request.routes.len());
    let mut any_failed = false;

    let context = EncodeContext {
        state: &state,
        request_id: &request.request_id,
        request_token_in: &request_token_in,
        request_token_out: &request_token_out,
        request_token_in_norm: &request_token_in_norm,
        request_token_out_norm: &request_token_out_norm,
        config: &config,
    };

    for route in &request.routes {
        let outcome = encode_route(&context, route).await;
        if let Some(failure) = outcome.failure {
            failures.push(failure);
            any_failed = true;
        }
        data.push(outcome.result);
    }

    let status = if any_failed {
        QuoteStatus::PartialFailure
    } else {
        QuoteStatus::Ready
    };

    EncodeResult {
        request_id: request.request_id,
        data,
        meta: EncodeMeta {
            status,
            auction_id,
            failures,
        },
    }
}

struct RouteOutcome {
    result: EncodeRouteResult,
    failure: Option<QuoteFailure>,
}

struct EncodeContext<'a> {
    state: &'a AppState,
    request_id: &'a str,
    request_token_in: &'a Bytes,
    request_token_out: &'a Bytes,
    request_token_in_norm: &'a str,
    request_token_out_norm: &'a str,
    config: &'a ResolvedCalldataConfig,
}

async fn encode_route(context: &EncodeContext<'_>, route: &EncodeRoute) -> RouteOutcome {
    let mut result = EncodeRouteResult {
        route_id: route.route_id.clone(),
        amounts_out: route.amounts_out.clone(),
        gas_used: route.gas_used.clone(),
        block_number: route.block_number,
        calldata: None,
    };

    if let Err(message) = validate_route_shape(
        route,
        context.request_token_in_norm,
        context.request_token_out_norm,
    ) {
        return RouteOutcome {
            result,
            failure: Some(route_failure(
                QuoteFailureKind::InvalidRequest,
                format!("Route {}: {}", route.route_id, message),
                None,
                None,
            )),
        };
    }

    let amounts_in = match parse_amounts(&route.amounts) {
        Ok(values) => values,
        Err(message) => {
            return RouteOutcome {
                result,
                failure: Some(route_failure(
                    QuoteFailureKind::InvalidRequest,
                    format!("Route {}: {}", route.route_id, message),
                    None,
                    None,
                )),
            };
        }
    };

    let amounts_out = match parse_amounts(&route.amounts_out) {
        Ok(values) => values,
        Err(message) => {
            return RouteOutcome {
                result,
                failure: Some(route_failure(
                    QuoteFailureKind::InvalidRequest,
                    format!("Route {}: {}", route.route_id, message),
                    None,
                    None,
                )),
            };
        }
    };

    let mut swaps = Vec::with_capacity(route.hops.len());
    let mut protocol_context = None;
    for hop in &route.hops {
        let token_in = match parse_address(&hop.token_in) {
            Ok(token) => token,
            Err(message) => {
                return RouteOutcome {
                    result,
                    failure: Some(route_failure(
                        QuoteFailureKind::InvalidRequest,
                        format!("Route {}: {}", route.route_id, message),
                        None,
                        None,
                    )),
                };
            }
        };
        let token_out = match parse_address(&hop.token_out) {
            Ok(token) => token,
            Err(message) => {
                return RouteOutcome {
                    result,
                    failure: Some(route_failure(
                        QuoteFailureKind::InvalidRequest,
                        format!("Route {}: {}", route.route_id, message),
                        None,
                        None,
                    )),
                };
            }
        };

        let pool_entry = context.state.pool_by_id(&hop.pool_id).await;
        let Some((pool_state, component)) = pool_entry else {
            warn!(
                request_id = context.request_id,
                route_id = route.route_id.as_str(),
                pool_id = hop.pool_id.as_str(),
                "Pool id not found for encode route"
            );
            return RouteOutcome {
                result,
                failure: Some(route_failure(
                    QuoteFailureKind::InvalidRequest,
                    format!(
                        "Route {}: pool_id {} not found",
                        route.route_id, hop.pool_id
                    ),
                    Some(hop.pool_id.as_str()),
                    None,
                )),
            };
        };

        if protocol_context.is_none() {
            protocol_context = Some(component_context(&hop.pool_id, &component));
        }

        let common_component: CommonProtocolComponent = component.as_ref().clone().into();
        swaps.push(Swap::new(
            common_component,
            token_in,
            token_out,
            0.0,
            None,
            Some(Arc::clone(&pool_state)),
            None,
        ));
    }

    let token_in_address = format_address(context.request_token_in);
    let token_out_address = format_address(context.request_token_out);
    let sender_address = format_address(&context.config.sender);
    let receiver_address = format_address(&context.config.receiver);

    let mut steps = Vec::with_capacity(amounts_in.len());
    for (index, amount_in) in amounts_in.iter().enumerate() {
        let amount_out = &amounts_out[index];
        let checked_amount = amount_out.clone();

        let solution = Solution {
            sender: context.config.sender.clone(),
            receiver: context.config.receiver.clone(),
            given_token: context.request_token_in.clone(),
            given_amount: amount_in.clone(),
            checked_token: context.request_token_out.clone(),
            exact_out: false,
            checked_amount,
            swaps: swaps.clone(),
            native_action: None,
        };

        let transactions = {
            #[allow(deprecated)]
            {
                context.config.encoder.encode_full_calldata(vec![solution])
            }
        };

        let transaction = match transactions {
            Ok(mut transactions) => {
                if transactions.len() != 1 {
                    return RouteOutcome {
                        result,
                        failure: Some(route_failure(
                            QuoteFailureKind::Internal,
                            format!(
                                "Route {}: expected 1 transaction, got {}",
                                route.route_id,
                                transactions.len()
                            ),
                            protocol_context
                                .as_ref()
                                .map(|context| context.pool_id.as_str()),
                            protocol_context
                                .as_ref()
                                .map(|context| context.protocol.as_str()),
                        )),
                    };
                }
                transactions.remove(0)
            }
            Err(err) => {
                if let Some(protocol_context) = &protocol_context {
                    warn!(
                        request_id = context.request_id,
                        route_id = route.route_id.as_str(),
                        pool_id = protocol_context.pool_id.as_str(),
                        protocol = protocol_context.protocol.as_str(),
                        error = %err,
                        "Failed to encode route"
                    );
                } else {
                    warn!(
                        request_id = context.request_id,
                        route_id = route.route_id.as_str(),
                        error = %err,
                        "Failed to encode route"
                    );
                }
                return RouteOutcome {
                    result,
                    failure: Some(route_failure(
                        QuoteFailureKind::Internal,
                        format!("Route {}: encode failed: {}", route.route_id, err),
                        protocol_context
                            .as_ref()
                            .map(|context| context.pool_id.as_str()),
                        protocol_context
                            .as_ref()
                            .map(|context| context.protocol.as_str()),
                    )),
                };
            }
        };

        let router_target = format_address(&transaction.to);
        let router_calldata = format_calldata(&transaction.data);
        let router_value = transaction.value.to_str_radix(10);

        let amount_in_value = route.amounts[index].clone();
        let amount_out_value = route.amounts_out[index].clone();

        let step = match context.config.mode {
            CalldataMode::TychoRouterTransferFrom => {
                let call = ExecutionCall {
                    call_type: ExecutionCallType::TychoRouter,
                    target: router_target.clone(),
                    value: router_value,
                    calldata: router_calldata,
                    allowances: vec![Allowance {
                        token: token_in_address.clone(),
                        spender: router_target.clone(),
                        amount: amount_in_value.clone(),
                    }],
                    inputs: vec![Asset {
                        token: token_in_address.clone(),
                        amount: amount_in_value.clone(),
                    }],
                    outputs: vec![Asset {
                        token: token_out_address.clone(),
                        amount: amount_out_value.clone(),
                    }],
                };
                CalldataStep { calls: vec![call] }
            }
            CalldataMode::TychoRouterNone => {
                let transfer_data = match encode_erc20_transfer(&transaction.to, amount_in) {
                    Ok(value) => value,
                    Err(message) => {
                        return RouteOutcome {
                            result,
                            failure: Some(route_failure(
                                QuoteFailureKind::Internal,
                                format!("Route {}: {}", route.route_id, message),
                                protocol_context
                                    .as_ref()
                                    .map(|context| context.pool_id.as_str()),
                                protocol_context
                                    .as_ref()
                                    .map(|context| context.protocol.as_str()),
                            )),
                        };
                    }
                };

                let transfer_call = ExecutionCall {
                    call_type: ExecutionCallType::Erc20Transfer,
                    target: token_in_address.clone(),
                    value: "0".to_string(),
                    calldata: format_calldata(&transfer_data),
                    allowances: Vec::new(),
                    inputs: vec![Asset {
                        token: token_in_address.clone(),
                        amount: amount_in_value.clone(),
                    }],
                    outputs: Vec::new(),
                };

                let router_call = ExecutionCall {
                    call_type: ExecutionCallType::TychoRouter,
                    target: router_target,
                    value: router_value,
                    calldata: router_calldata,
                    allowances: Vec::new(),
                    inputs: Vec::new(),
                    outputs: vec![Asset {
                        token: token_out_address.clone(),
                        amount: amount_out_value.clone(),
                    }],
                };

                CalldataStep {
                    calls: vec![transfer_call, router_call],
                }
            }
        };

        steps.push(step);
    }

    result.calldata = Some(CalldataResult {
        mode: context.config.mode,
        sender: sender_address,
        receiver: receiver_address,
        steps,
    });

    RouteOutcome {
        result,
        failure: None,
    }
}

fn validate_route_shape(
    route: &EncodeRoute,
    request_token_in_norm: &str,
    request_token_out_norm: &str,
) -> Result<(), String> {
    if route.hops.is_empty() {
        return Err("hops must not be empty".to_string());
    }

    if route.amounts.is_empty() {
        return Err("amounts must not be empty".to_string());
    }

    if route.amounts_out.len() != route.amounts.len() {
        return Err("amounts_out length must match amounts".to_string());
    }

    if route.gas_used.len() != route.amounts.len() {
        return Err("gas_used length must match amounts".to_string());
    }

    let first_in = normalize_address(&route.hops[0].token_in);
    if first_in != request_token_in_norm {
        return Err("first hop token_in does not match request token_in".to_string());
    }

    let last_out = normalize_address(&route.hops[route.hops.len() - 1].token_out);
    if last_out != request_token_out_norm {
        return Err("last hop token_out does not match request token_out".to_string());
    }

    for window in route.hops.windows(2) {
        let prev_out = normalize_address(&window[0].token_out);
        let next_in = normalize_address(&window[1].token_in);
        if prev_out != next_in {
            return Err("hop token continuity mismatch".to_string());
        }
    }

    Ok(())
}

fn parse_amounts(values: &[String]) -> Result<Vec<BigUint>, String> {
    let mut parsed = Vec::with_capacity(values.len());
    for value in values {
        let parsed_value = parse_amount(value)?;
        parsed.push(parsed_value);
    }
    Ok(parsed)
}

fn parse_amount(value: &str) -> Result<BigUint, String> {
    BigUint::from_str(value).map_err(|_| format!("Invalid amount: {}", value))
}

fn normalize_address(value: &str) -> String {
    let trimmed = value.trim();
    let no_prefix = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    no_prefix.to_lowercase()
}

fn parse_address(value: &str) -> Result<Bytes, String> {
    let trimmed = value.trim();
    let bytes =
        Bytes::from_str(trimmed).map_err(|err| format!("Invalid address {}: {}", value, err))?;
    if bytes.len() != 20 {
        return Err(format!("Invalid address length for {}", value));
    }
    Ok(bytes)
}

fn format_0x_hex(raw: &str) -> String {
    if let Some(stripped) = raw.strip_prefix("0x") {
        return format!("0x{stripped}");
    }
    if let Some(stripped) = raw.strip_prefix("0X") {
        return format!("0x{stripped}");
    }
    format!("0x{raw}")
}

fn format_address(bytes: &Bytes) -> String {
    format_0x_hex(&format!("{bytes:x}"))
}

fn format_calldata(data: &[u8]) -> String {
    let bytes = Bytes::from(data.to_vec());
    format_0x_hex(&format!("{bytes:x}"))
}

fn encode_erc20_transfer(target: &Bytes, amount: &BigUint) -> Result<Vec<u8>, String> {
    let target_bytes = target.as_ref();
    if target_bytes.len() > 32 {
        return Err("ERC20 transfer target is too long".to_string());
    }
    let amount_bytes = amount.to_bytes_be();
    if amount_bytes.len() > 32 {
        return Err("ERC20 transfer amount is too large".to_string());
    }

    let mut data = Vec::with_capacity(4 + 32 + 32);
    data.extend_from_slice(&ERC20_TRANSFER_SELECTOR);

    let mut target_word = [0u8; 32];
    target_word[32 - target_bytes.len()..].copy_from_slice(target_bytes);
    data.extend_from_slice(&target_word);

    let mut amount_word = [0u8; 32];
    amount_word[32 - amount_bytes.len()..].copy_from_slice(&amount_bytes);
    data.extend_from_slice(&amount_word);

    Ok(data)
}

fn invalid_request_failure<T: Into<String>>(message: T) -> QuoteFailure {
    QuoteFailure {
        kind: QuoteFailureKind::InvalidRequest,
        message: message.into(),
        pool: None,
        pool_name: None,
        pool_address: None,
        protocol: None,
    }
}

fn route_failure(
    kind: QuoteFailureKind,
    message: String,
    pool: Option<&str>,
    protocol: Option<&str>,
) -> QuoteFailure {
    QuoteFailure {
        kind,
        message,
        pool: pool.map(|value| value.to_string()),
        pool_name: None,
        pool_address: None,
        protocol: protocol.map(|value| value.to_string()),
    }
}

fn invalid_request_result(
    request_id: String,
    auction_id: Option<String>,
    failures: Vec<QuoteFailure>,
) -> EncodeResult {
    EncodeResult {
        request_id,
        data: Vec::new(),
        meta: EncodeMeta {
            status: QuoteStatus::InvalidRequest,
            auction_id,
            failures,
        },
    }
}

struct ResolvedCalldataConfig {
    mode: CalldataMode,
    sender: Bytes,
    receiver: Bytes,
    encoder: Arc<dyn TychoEncoder>,
}

fn resolve_calldata_config(
    state: &AppState,
    calldata: Option<&CalldataConfig>,
) -> Result<ResolvedCalldataConfig, String> {
    let mode = calldata.and_then(|config| config.mode).unwrap_or_default();
    let encode_state = state.encode_state();
    let sender = match calldata.and_then(|config| config.sender.as_deref()) {
        Some(value) => parse_address(value)?,
        None => encode_state
            .cow_settlement_contract
            .clone()
            .ok_or_else(|| {
                "COW_SETTLEMENT_CONTRACT is required when calldata.sender is missing".to_string()
            })?,
    };

    let receiver = match calldata.and_then(|config| config.receiver.as_deref()) {
        Some(value) => parse_address(value)?,
        None => encode_state
            .cow_settlement_contract
            .clone()
            .ok_or_else(|| {
                "COW_SETTLEMENT_CONTRACT is required when calldata.receiver is missing".to_string()
            })?,
    };

    let encoder = match mode {
        CalldataMode::TychoRouterTransferFrom => Arc::clone(&encode_state.transfer_from_encoder),
        CalldataMode::TychoRouterNone => Arc::clone(&encode_state.none_encoder),
    };

    Ok(ResolvedCalldataConfig {
        mode,
        sender,
        receiver,
        encoder,
    })
}

struct RouteProtocolContext {
    pool_id: String,
    protocol: String,
}

fn component_context(pool_id: &str, component: &ProtocolComponent) -> RouteProtocolContext {
    RouteProtocolContext {
        pool_id: pool_id.to_string(),
        protocol: component.protocol_system.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_erc20_transfer_builds_selector() {
        let target =
            Bytes::from_str("0x0000000000000000000000000000000000000001").expect("valid address");
        let amount = BigUint::from(42u32);
        let data = encode_erc20_transfer(&target, &amount).expect("valid transfer");
        assert_eq!(&data[..4], &ERC20_TRANSFER_SELECTOR);
        assert_eq!(data.len(), 68);
    }

    #[test]
    fn format_address_prefixes_0x() {
        let address =
            Bytes::from_str("0x00000000000000000000000000000000000000ab").expect("valid address");
        let formatted = format_address(&address);
        assert!(formatted.starts_with("0x"));
        assert_eq!(formatted.len(), 42);
        assert_eq!((formatted.len() - 2) % 2, 0);
    }

    #[test]
    fn format_calldata_prefixes_0x() {
        let data = vec![0x00, 0x01, 0x0a];
        let formatted = format_calldata(&data);
        assert_eq!(formatted, "0x00010a");
        assert_eq!((formatted.len() - 2) % 2, 0);
    }

    #[test]
    fn normalize_address_accepts_uppercase_prefix() {
        assert_eq!(normalize_address("0XAbCdEf"), normalize_address("0xabcdef"));
    }
}
