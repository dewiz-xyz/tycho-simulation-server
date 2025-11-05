#!/bin/bash

# Stress test runner script for Tycho Simulation Server

echo "Tycho Simulation Server Stress Test Runner"
echo "=========================================="

# Check if Python 3 is available
if ! command -v python3 &> /dev/null; then
    echo "Error: Python 3 is required but not installed."
    exit 1
fi

# Check if pip is available
if ! command -v pip3 &> /dev/null; then
    echo "Error: pip3 is required but not installed."
    exit 1
fi

# Create virtual environment if it doesn't exist
if [ ! -d "venv" ]; then
    echo "Creating Python virtual environment..."
    python3 -m venv venv
    if [ $? -ne 0 ]; then
        echo "Error: Failed to create virtual environment."
        exit 1
    fi
fi

# Activate virtual environment and install dependencies
echo "Activating virtual environment and installing dependencies..."
source venv/bin/activate
if [ -f "requirements.txt" ]; then
    pip install -r requirements.txt
    if [ $? -ne 0 ]; then
        echo "Error: Failed to install dependencies."
        exit 1
    fi
fi

# Check if the server is running
echo "Checking if server is running on localhost:3000..."
if ! curl -s http://localhost:3000/status > /dev/null; then
    echo "Warning: Server doesn't seem to be running on localhost:3000"
    echo "Make sure to start the server first with: cargo run --release"
    echo ""
    read -p "Do you want to continue anyway? (y/N): " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        exit 1
    fi
fi

# Run the stress test
echo "Starting stress test..."
echo "This will send 500 concurrent requests to the /simulate endpoint"
echo ""

python stress_test.py

echo ""
echo "Stress test completed!"
