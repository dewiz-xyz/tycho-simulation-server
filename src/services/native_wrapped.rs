use tycho_simulation::{
    protocol::models::ProtocolComponent,
    tycho_common::{
        models::{token::Token, Chain},
        Bytes,
    },
};

const NATIVE_TOKEN_ADDRESS_BYTES: [u8; 20] = [0u8; 20];

pub(crate) fn native_token_address() -> Bytes {
    Bytes::from(NATIVE_TOKEN_ADDRESS_BYTES)
}

pub(crate) fn is_direct_native_wrapped_pair(
    token_in: &Bytes,
    token_out: &Bytes,
    wrapped_native: Option<&Bytes>,
) -> bool {
    let Some(wrapped_native) = wrapped_native else {
        return false;
    };

    (token_in == &native_token_address() && token_out == wrapped_native)
        || (token_in == wrapped_native && token_out == &native_token_address())
}

pub(crate) fn simulation_tokens_for_pool(
    request_token_in: &Token,
    request_token_out: &Token,
    component: &ProtocolComponent,
) -> (Token, Token) {
    (
        remap_request_token_for_pool(request_token_in, component),
        remap_request_token_for_pool(request_token_out, component),
    )
}

pub(crate) fn remap_request_token_for_pool(
    request_token: &Token,
    component: &ProtocolComponent,
) -> Token {
    let native = request_token.chain.native_token();
    let wrapped_native = request_token.chain.wrapped_native_token();

    if native.address == wrapped_native.address {
        return request_token.clone();
    }

    let pool_has_native = component
        .tokens
        .iter()
        .any(|token| token.address == native.address);
    let pool_has_wrapped = component
        .tokens
        .iter()
        .any(|token| token.address == wrapped_native.address);

    if request_token.address == wrapped_native.address && pool_has_native && !pool_has_wrapped {
        return native;
    }

    if request_token.address == native.address && pool_has_wrapped && !pool_has_native {
        return wrapped_native;
    }

    request_token.clone()
}

pub(crate) fn normalize_native_to_wrapped(
    address: &Bytes,
    chain: Chain,
    keep_native_unwrapped: bool,
) -> Bytes {
    if !keep_native_unwrapped && *address == chain.native_token().address {
        chain.wrapped_native_token().address
    } else {
        address.clone()
    }
}
