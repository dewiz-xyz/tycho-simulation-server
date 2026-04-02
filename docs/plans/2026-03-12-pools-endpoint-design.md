# Pools Endpoint Design

## Goal

Add a read-only HTTP endpoint that returns all currently indexed pools grouped by protocol, with an optional protocol filter for targeted inspection. The initial consumer is Curve coverage analysis, so the response must include both the logical pool id and the onchain pool address.

## Context

The service already keeps protocol components in memory inside `StateStore`, partitioned by `ProtocolKind`. Existing endpoints only expose readiness and simulation behavior, so there is no direct way to inspect the live pool index without inferring coverage through `/simulate`.

## API

Add `GET /pools`.

Supported forms:
- `GET /pools` returns all indexed pools grouped by protocol.
- `GET /pools?protocol=vm:curve` returns only that protocol.

Response fields per pool:
- `id`: logical component id in the local state store
- `address`: onchain component address (`ProtocolComponent.id`)
- `protocol_system`
- `protocol_type_name`
- `chain`
- `contract_ids`
- `tokens`: token address, symbol, decimals

Response fields per protocol group:
- `protocol`
- `count`
- `pools`

## Data Flow

1. The handler parses the optional `protocol` query parameter.
2. `AppState` asks both native and VM `StateStore`s for protocol-grouped pool snapshots.
3. VM pools are only included when the VM side is currently ready, matching existing app-state behavior.
4. The handler serializes a stable subset of `ProtocolComponent` metadata and returns it as JSON.

## Error Handling

- Unknown `protocol` query values return `400 Bad Request`.
- A valid protocol with zero indexed pools returns `200 OK` with `count=0`.
- The endpoint is read-only and does not depend on simulation execution paths.

## Sorting

- Protocol groups follow `ProtocolKind::ALL` order.
- Pools within each group are sorted by logical id for deterministic diffs.

## Testing

- Add a state-store unit test covering protocol enumeration.
- Add an integration test covering `GET /pools` and `GET /pools?protocol=vm:curve`.

## Non-Goals

- Exposing simulator internals or opaque VM state
- Returning liquidity or quote health from this endpoint
- Remote Curve reconciliation inside the service itself
