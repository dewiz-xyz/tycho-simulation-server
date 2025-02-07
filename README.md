# Tycho Simulation Server

A WebSocket server implementation for TypeScript Tycho simulation, providing real-time price simulation services.

## Description

Tycho Simulation Server is a Rust-based WebSocket server that interfaces with the Tycho simulation engine. It provides real-time price simulation services and is designed to be efficient and scalable.

## Features

- Real-time WebSocket communication
- Price simulation service
- Built with Axum web framework
- Async runtime with Tokio

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

- `PORT`: Server port (default: 3000)
- `HOST`: Server host address (default: 127.0.0.1)

## Usage

To run the server:

```bash
cargo run --release
```

The server will start on the configured host and port.

## Dependencies

Key dependencies include:
- `tycho-simulation`: Core simulation engine
- `tokio`: Async runtime
- `axum`: Web framework
- `serde`: Serialization framework
- `anyhow`: Error handling

## License

This project is licensed under the MIT License.

## Repository

[https://github.com/dewiz-xyz/tycho-simulation-server](https://github.com/dewiz-xyz/tycho-simulation-server)

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request. 