#!/usr/bin/env python3
"""
Stress test script for the /simulate endpoint.
Sends 500 concurrent requests with real Ethereum tokens and calculates timing metrics.
"""

import asyncio
import aiohttp
import time
import json
import uuid
from typing import List, Tuple
import statistics

# Real Ethereum token addresses
TOKENS = {
    "USDC": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
    "USDT": "0xdac17f958d2ee523a2206206994597c13d831ec7", 
    "DAI": "0x6b175474e89094c44da98b954eedeac495271d0f",
    "WETH": "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
    "UNI": "0x1f9840a85d5af5bf1d1762f925bdaddc4201f984",
    "LINK": "0x514910771af9ca656af840dff83e8264ecf986ca",
    "WBTC": "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599",
    "AAVE": "0x7fc66500c84a76ad7e9c93437bfc5ac33e2ddae9",
    "COMP": "0xc00e94cb662c3520282e6f5717214004a7f26888",
    "MKR": "0x9f8f72aa9304c8b593d555f12ef6589cc3a579a2"
}

# Sample amounts (10 elements as requested) - in wei units
AMOUNTS = [
    "1000000000000000000",    # 1 ETH
    "5000000000000000000",    # 5 ETH  
    "10000000000000000000",   # 10 ETH
    "50000000000000000000",   # 50 ETH
    "100000000000000000000",  # 100 ETH
    "500000000000000000000",  # 500 ETH
    "1000000000000000000000", # 1000 ETH
    "5000000000000000000000", # 5000 ETH
    "10000000000000000000000", # 10000 ETH
    "50000000000000000000000"  # 50000 ETH
]

# Server configuration
SERVER_URL = "http://localhost:3000"
ENDPOINT = f"{SERVER_URL}/simulate"

# Test configuration
NUM_REQUESTS = 2000  # Reduced from 1000 to avoid "too many open files" error
MAX_CONCURRENT = 400  # Target concurrent requests

class StressTestResults:
    def __init__(self):
        self.response_times: List[float] = []
        self.successful_requests = 0
        self.failed_requests = 0
        self.errors: List[str] = []
        self.start_time = 0
        self.end_time = 0

    def add_result(self, response_time: float, success: bool, error: str = None):
        self.response_times.append(response_time)
        if success:
            self.successful_requests += 1
        else:
            self.failed_requests += 1
            if error:
                self.errors.append(error)

    def get_total_time(self) -> float:
        return self.end_time - self.start_time

    def get_average_response_time(self) -> float:
        if not self.response_times:
            return 0.0
        return statistics.mean(self.response_times)

    def get_median_response_time(self) -> float:
        if not self.response_times:
            return 0.0
        return statistics.median(self.response_times)

    def get_p95_response_time(self) -> float:
        if not self.response_times:
            return 0.0
        return statistics.quantiles(self.response_times, n=20)[18]  # 95th percentile

    def get_p99_response_time(self) -> float:
        if not self.response_times:
            return 0.0
        return statistics.quantiles(self.response_times, n=100)[98]  # 99th percentile

    def print_summary(self):
        print("\n" + "="*60)
        print("STRESS TEST RESULTS")
        print("="*60)
        print(f"Total requests: {self.successful_requests + self.failed_requests}")
        print(f"Successful requests: {self.successful_requests}")
        print(f"Failed requests: {self.failed_requests}")
        print(f"Success rate: {(self.successful_requests / (self.successful_requests + self.failed_requests) * 100):.2f}%")
        print(f"Total time for all requests: {self.get_total_time():.4f} seconds")
        print(f"Average time per request: {self.get_average_response_time():.4f} seconds")
        print(f"Median response time: {self.get_median_response_time():.4f} seconds")
        print(f"95th percentile response time: {self.get_p95_response_time():.4f} seconds")
        print(f"99th percentile response time: {self.get_p99_response_time():.4f} seconds")
        print(f"Min response time: {min(self.response_times):.4f} seconds")
        print(f"Max response time: {max(self.response_times):.4f} seconds")
        
        if self.errors:
            print(f"\nErrors encountered: {len(self.errors)}")
            for i, error in enumerate(self.errors[:5]):  # Show first 5 errors
                print(f"  {i+1}. {error}")
            if len(self.errors) > 5:
                print(f"  ... and {len(self.errors) - 5} more errors")

def get_random_token_pair() -> Tuple[str, str]:
    """Get a random pair of different tokens for testing."""
    import random
    tokens = list(TOKENS.values())
    token_in, token_out = random.sample(tokens, 2)
    return token_in, token_out

async def send_single_request(session: aiohttp.ClientSession, request_id: int) -> Tuple[float, bool, str]:
    """Send a single request to the /simulate endpoint."""
    token_in, token_out = get_random_token_pair()
    
    payload = {
        "request_id": f"stress_test_{request_id}_{uuid.uuid4().hex[:8]}",
        "token_in": token_in,
        "token_out": token_out,
        "amounts": AMOUNTS
    }
    
    start_time = time.perf_counter()
    
    try:
        async with session.post(ENDPOINT, json=payload) as response:
            await response.text()  # Consume the response
            end_time = time.perf_counter()
            response_time = end_time - start_time
            
            if response.status == 200:
                return response_time, True, ""
            else:
                return response_time, False, f"HTTP {response.status}"
                
    except asyncio.TimeoutError:
        end_time = time.perf_counter()
        return end_time - start_time, False, "Timeout"
    except Exception as e:
        end_time = time.perf_counter()
        return end_time - start_time, False, str(e)

async def run_stress_test() -> StressTestResults:
    """Run the stress test with concurrent requests."""
    results = StressTestResults()
    
    # Configure connection limits optimized for 500 concurrent requests
    connector = aiohttp.TCPConnector(
        limit=600,  # Allow more connections than concurrent requests
        limit_per_host=600,  # Per-host limit
        ttl_dns_cache=300,  # DNS cache TTL
        use_dns_cache=True,
        keepalive_timeout=10,  # Shorter keepalive to free connections faster
        enable_cleanup_closed=True,  # Clean up closed connections
        force_close=False,  # Allow connection reuse for better performance
        family=0,  # Allow both IPv4 and IPv6
        ssl=False  # Disable SSL for localhost
    )
    timeout = aiohttp.ClientTimeout(total=30)  # 30 second timeout per request
    
    print(f"Starting stress test...")
    print(f"Target: {ENDPOINT}")
    print(f"Total requests: {NUM_REQUESTS}")
    print(f"Max concurrent: {MAX_CONCURRENT}")
    print(f"Token pairs: {len(TOKENS)} tokens available")
    print(f"Amounts per request: {len(AMOUNTS)}")
    
    results.start_time = time.perf_counter()
    
    async with aiohttp.ClientSession(connector=connector, timeout=timeout) as session:
        # Create semaphore to limit concurrent requests
        semaphore = asyncio.Semaphore(MAX_CONCURRENT)
        
        async def limited_request(request_id: int):
            async with semaphore:
                return await send_single_request(session, request_id)
        
        # Process all requests concurrently with proper semaphore limiting
        print(f"\nSending {NUM_REQUESTS} requests with {MAX_CONCURRENT} concurrent limit...")
        
        # Create all tasks at once - semaphore will handle the concurrency limiting
        tasks = [limited_request(i) for i in range(NUM_REQUESTS)]
        
        # Execute all requests concurrently
        print("Executing all requests concurrently...")
        request_results = await asyncio.gather(*tasks, return_exceptions=True)
        
        # Process results
        for i, result in enumerate(request_results):
            if isinstance(result, Exception):
                results.add_result(0.0, False, f"Task {i}: {str(result)}")
            else:
                response_time, success, error = result
                results.add_result(response_time, success, error)
    
    results.end_time = time.perf_counter()
    return results

async def main():
    """Main function to run the stress test."""
    print("Tycho Simulation Server Stress Test")
    print("="*40)
    
    # Check system limits
    import resource
    soft_limit, hard_limit = resource.getrlimit(resource.RLIMIT_NOFILE)
    print(f"System file descriptor limits: soft={soft_limit}, hard={hard_limit}")
    
    if soft_limit < MAX_CONCURRENT * 2:
        print(f"Warning: File descriptor limit ({soft_limit}) may be too low for {MAX_CONCURRENT} concurrent requests")
        print("Consider increasing with: ulimit -n 65536")
        print("")
    
    try:
        results = await run_stress_test()
        results.print_summary()
        
        # Additional metrics
        if results.response_times:
            requests_per_second = results.successful_requests / results.get_total_time()
            print(f"\nThroughput: {requests_per_second:.2f} requests/second")
            
            # Show some example token pairs used
            print(f"\nToken pairs used in testing:")
            for i, (symbol, address) in enumerate(list(TOKENS.items())[:5]):
                print(f"  {symbol}: {address}")
            if len(TOKENS) > 5:
                print(f"  ... and {len(TOKENS) - 5} more tokens")
                
    except KeyboardInterrupt:
        print("\nStress test interrupted by user")
    except Exception as e:
        print(f"\nError running stress test: {e}")

if __name__ == "__main__":
    asyncio.run(main())
