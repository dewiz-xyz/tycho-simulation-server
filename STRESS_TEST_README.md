# Stress Test for Tycho Simulation Server

This directory contains a stress test script for the `/simulate` endpoint of the Tycho Simulation Server.

## Overview

The stress test script (`stress_test.py`) performs the following:

- **Concurrent Requests**: Sends 500 concurrent HTTP requests to the `/simulate` endpoint
- **Real Token Data**: Uses real Ethereum token addresses (USDC, USDT, DAI, WETH, UNI, LINK, WBTC, AAVE, COMP, MKR)
- **Amount Arrays**: Each request includes 10 different amounts (ranging from 1 ETH to 50,000 ETH in wei)
- **Timing Metrics**: Calculates average response time, total time, and various percentiles
- **Error Handling**: Tracks successful vs failed requests and error details

## Prerequisites

1. **Python 3.7+** with pip
2. **Tycho Simulation Server** running on `localhost:3000`
3. **Dependencies**: `aiohttp` (automatically installed by the script)

## Quick Start

### Option 1: Using the shell script (Recommended)
```bash
./run_stress_test.sh
```

### Option 2: Manual execution
```bash
# Create and activate virtual environment
python3 -m venv venv
source venv/bin/activate

# Install dependencies
pip install -r requirements.txt

# Run the stress test
python stress_test.py
```

## Configuration

You can modify the following parameters in `stress_test.py`:

- `NUM_REQUESTS`: Number of concurrent requests (default: 500)
- `MAX_CONCURRENT`: Maximum concurrent connections (default: 100)
- `SERVER_URL`: Server URL (default: http://localhost:3000)
- `AMOUNTS`: Array of amounts to test with (default: 10 different amounts)
- `TOKENS`: Dictionary of real Ethereum token addresses

## Sample Request Format

Each request sent to the `/simulate` endpoint has this structure:

```json
{
  "request_id": "stress_test_123_abc12345",
  "token_in": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
  "token_out": "0xdac17f958d2ee523a2206206994597c13d831ec7",
  "amounts": [
    "1000000000000000000",
    "5000000000000000000",
    "10000000000000000000",
    "50000000000000000000",
    "100000000000000000000",
    "500000000000000000000",
    "1000000000000000000000",
    "5000000000000000000000",
    "10000000000000000000000",
    "50000000000000000000000"
  ]
}
```

## Output Metrics

The stress test provides comprehensive metrics:

- **Total requests**: Number of requests sent
- **Successful requests**: Number of successful responses
- **Failed requests**: Number of failed responses
- **Success rate**: Percentage of successful requests
- **Total time**: Time taken for all requests to complete
- **Average response time**: Mean response time per request
- **Median response time**: 50th percentile response time
- **95th percentile**: 95% of requests completed within this time
- **99th percentile**: 99% of requests completed within this time
- **Min/Max response times**: Fastest and slowest individual requests
- **Throughput**: Requests per second
- **Error details**: List of errors encountered

## Example Output

```
============================================================
STRESS TEST RESULTS
============================================================
Total requests: 500
Successful requests: 487
Failed requests: 13
Success rate: 97.40%
Total time for all requests: 12.3456 seconds
Average time per request: 0.1234 seconds
Median response time: 0.0987 seconds
95th percentile response time: 0.2345 seconds
99th percentile response time: 0.4567 seconds
Min response time: 0.0123 seconds
Max response time: 0.7890 seconds

Throughput: 39.45 requests/second
```

## Token Addresses Used

The script uses real Ethereum mainnet token addresses:

- **USDC**: `0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48`
- **USDT**: `0xdac17f958d2ee523a2206206994597c13d831ec7`
- **DAI**: `0x6b175474e89094c44da98b954eedeac495271d0f`
- **WETH**: `0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2`
- **UNI**: `0x1f9840a85d5af5bf1d1762f925bdaddc4201f984`
- **LINK**: `0x514910771af9ca656af840dff83e8264ecf986ca`
- **WBTC**: `0x2260fac5e5542a773aa44fbcfedf7c193bc2c599`
- **AAVE**: `0x7fc66500c84a76ad7e9c93437bfc5ac33e2ddae9`
- **COMP**: `0xc00e94cb662c3520282e6f5717214004a7f26888`
- **MKR**: `0x9f8f72aa9304c8b593d555f12ef6589cc3a579a2`

## Troubleshooting

### Server not running
If you get connection errors, make sure the Tycho Simulation Server is running:
```bash
cargo run --release
```

### Permission denied
If the shell script doesn't run, make it executable:
```bash
chmod +x run_stress_test.sh
```

### High failure rate
If you see many failed requests, consider:
- Reducing `NUM_REQUESTS` or `MAX_CONCURRENT`
- Increasing server resources
- Checking server logs for errors

### Memory issues
For very high concurrency, you might need to adjust system limits or reduce the number of concurrent requests.

## Customization

To test with different parameters:

1. **Different server URL**: Change `SERVER_URL` in the script
2. **More/fewer requests**: Modify `NUM_REQUESTS`
3. **Different tokens**: Update the `TOKENS` dictionary
4. **Different amounts**: Modify the `AMOUNTS` array
5. **Timeout settings**: Adjust the `timeout` parameter in `aiohttp.ClientTimeout`

## Notes

- The script uses random token pairs for each request to simulate realistic usage
- Response times are measured using `time.perf_counter()` for high precision
- The script includes proper error handling and timeout management
- All amounts are specified in wei (the smallest unit of ETH)
- The script respects connection limits to avoid overwhelming the server
