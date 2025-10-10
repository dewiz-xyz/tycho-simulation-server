# Tycho Simulation Server

A WebSocket server implementation for TypeScript Tycho simulation, providing real-time price simulation services.

## Description

Tycho Simulation Server is a Rust-based WebSocket server that interfaces with the Tycho simulation engine. It provides real-time price simulation services and is designed to be efficient and scalable.

## Features

- Real-time WebSocket communication
- Price simulation service
- Built with Axum web framework
- Async runtime with Tokio
- Structured logging with tracing framework

## Prerequisites

- Rust (latest stable version)
- Cargo (Rust's package manager)
- PostgreSQL (for database functionality)

## Installation

1. Clone the repository:
   ```bash
   git clone https://github.com/dewiz-xyz/tycho-simulation-server.git
   cd tycho-simulation-server
   ```

2. Copy the environment file and configure it:
   ```bash
   cp .env.example .env
   ```
   Update the `.env` file with your specific configuration.

3. Build the project:
   ```bash
   cargo build --release
   ```

## Configuration

The following environment variables can be configured in your `.env` file:

- `TYCHO_URL`: Tycho API URL (default: "tycho-beta.propellerheads.xyz")
- `TYCHO_API_KEY`: Your Tycho API key (required)
- `TVL_THRESHOLD`: TVL add threshold for filtering, denominated in the chain's native token. Default if unset: 300.
- `TVL_KEEP_RATIO`: Ratio used to compute the remove/keep threshold as a fraction of `TVL_THRESHOLD` (i.e., `remove = TVL_THRESHOLD * TVL_KEEP_RATIO`). Default if unset: 0.2. The derived keep threshold is clamped to be no greater than `TVL_THRESHOLD`.
- `PORT`: Server port (default: 3000)
- `HOST`: Server host address (default: 127.0.0.1)
- `RUST_LOG`: Logging level (default: info, options: error, warn, info, debug, trace)
- `QUOTE_TIMEOUT_MS`: Maximum duration in milliseconds that a quote request is allowed to run before cancelling outstanding pool simulations (default: 50)
- `POOL_TIMEOUT_MS`: Per-pool simulation watchdog in milliseconds; each pool simulation is aborted if it exceeds this duration (default: 5)
- `CANCELLATION_TTL_SECS`: Duration in seconds to keep canceled auction entries per WebSocket connection to suppress late updates (default: 10)
- `ENABLE_AUCTION_CANCELLATION`: Feature gate to enable/disable auction-aware cancellation (default: true)

## WebSocket Protocol

Inbound (client → server):
- Tagged messages (required):
  - `{"type":"QuoteAmountOut","data":{ request_id: string, auction_id?: string, token_in: string, token_out: string, amounts: string[] }}`
  - `{"type":"CancelAuctionRequests","data":{ auction_id: string }}`

Outbound (server → client):
- Block updates: `{"type":"BlockUpdate","data":{ block_number, states_count, new_pairs_count, removed_pairs_count }}`
- Quote updates: `{"type":"QuoteUpdate","data":{ request_id: string, data: AmountOutResponse[], meta: QuoteMeta }}` where `meta` optionally includes `auction_id`.
- Cancel acknowledgment: `{"type":"CancelAck","data":{ auction_id: string, canceled_count: number }}`

Cancellation semantics:
- Each WebSocket connection tracks pending quotes by `auction_id`.
- On `CancelAuctionRequests`, pending quotes for that `auction_id` are aborted and a `CancelAck` is sent with `canceled_count`.
- Late `QuoteUpdate`s for a canceled `auction_id` are dropped silently for `CANCELLATION_TTL_SECS` (default 10 seconds).
- Requests without `auction_id` are never canceled and always emit updates.

## Usage

To run the server:

```bash
cargo run --release
```

The server will start on the configured host and port.

## Dependencies

Key dependencies include:
- `tycho-simulation` Core simulation engine
- `tokio`: Async runtime
- `axum`: Web framework
- `serde`: Serialization framework
- `anyhow`: Error handling
- `tracing`: Structured logging
- `tracing-subscriber`: Configurable logging output

## Project Structure

- `src/api`: API router and endpoint definitions
- `src/config`: Configuration management
- `src/handlers`: Request handlers (WebSocket, stream)
- `src/models`: Data models and state definitions
- `src/services`: Business logic and core functionality
- `main.rs`: Application entry point and initialization

## License

This project is licensed under the MIT License.

## Repository

[https://github.com/dewiz-xyz/tycho-simulation-server](https://github.com/dewiz-xyz/tycho-simulation-server)

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request. 
