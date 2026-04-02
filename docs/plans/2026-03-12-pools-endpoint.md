# Pools Endpoint Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a read-only `/pools` endpoint that lists indexed pools by protocol and use it to diff local Curve coverage against Curve's Ethereum pool list.

**Architecture:** Expose a lightweight snapshot path from `StateStore`, merge native and VM protocol groups in `AppState`, and serialize the result through a new Axum handler. Keep the payload limited to stable `ProtocolComponent` metadata so the endpoint stays cheap and deterministic.

**Tech Stack:** Rust, Axum, Tokio, serde, existing `StateStore`/`AppState` models

---

### Task 1: Add a failing state-store enumeration test

**Files:**
- Modify: `src/models/state.rs`

**Step 1: Write the failing test**

Add a unit test that inserts pools for at least two different protocols and asserts that a new state-store enumeration method returns grouped, sorted snapshots with the expected ids and protocol kinds.

**Step 2: Run test to verify it fails**

Run: `cargo test pools_by_protocol --lib`
Expected: FAIL because the enumeration method does not exist yet.

**Step 3: Write minimal implementation**

Add a snapshot method on `StateStore` that iterates shards, clones `ProtocolComponent` pointers, and returns protocol-grouped pools in deterministic order.

**Step 4: Run test to verify it passes**

Run: `cargo test pools_by_protocol --lib`
Expected: PASS

### Task 2: Add a failing `/pools` integration test

**Files:**
- Create: `tests/integration/pools_route.rs`
- Modify: `tests/integration/main.rs`

**Step 1: Write the failing test**

Build a small app state with one native pool and one Curve VM pool, then assert:
- `GET /pools` returns both groups
- `GET /pools?protocol=vm:curve` returns only the Curve group
- each pool includes ids, address, and token metadata

**Step 2: Run test to verify it fails**

Run: `cargo test pools_route --test integration`
Expected: FAIL because the handler and route do not exist yet.

**Step 3: Write minimal implementation**

Add the handler, router registration, query parsing, and app-state aggregation needed to satisfy the test.

**Step 4: Run test to verify it passes**

Run: `cargo test pools_route --test integration`
Expected: PASS

### Task 3: Refine response DTOs and validation

**Files:**
- Create: `src/handlers/pools.rs`
- Modify: `src/handlers/mod.rs`
- Modify: `src/api/mod.rs`
- Modify: `src/models/protocol.rs`
- Modify: `src/models/state.rs`

**Step 1: Add protocol parsing support**

Accept canonical protocol names from the query string and reject unknown values with `400`.

**Step 2: Keep payload stable**

Serialize:
- logical id
- onchain address
- protocol system/type
- chain
- token metadata
- contract ids

**Step 3: Run targeted tests**

Run: `cargo test pools --lib --test integration`
Expected: PASS

### Task 4: Verify endpoint against the running service

**Files:**
- None

**Step 1: Query the new endpoint**

Run: `curl -s http://127.0.0.1:3000/pools?protocol=vm:curve`

**Step 2: Save the snapshot**

Write the response to a timestamped file under `logs/`.

**Step 3: Compare with Curve API**

Fetch `https://api.curve.finance/v1/getPools/all/ethereum`, diff by onchain address, and produce:
- total real Curve pools
- total local `vm:curve` pools
- shared pools
- only-in-Curve list
- only-in-local list

### Task 5: Final verification

**Files:**
- None

**Step 1: Run focused tests**

Run:
- `cargo test pools_by_protocol --lib`
- `cargo test pools_route --test integration`

**Step 2: Sanity-check status**

Run: `curl -s http://127.0.0.1:3000/status`

**Step 3: Report results**

Summarize the implementation, verification commands, and the Curve diff with artifact paths.
