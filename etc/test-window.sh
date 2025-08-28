#!/bin/bash
# Test the compositor and spawn a window

set -e

echo "=== Testing SWL Compositor ==="

# Kill any existing compositor
pkill -f "target/.*swl" 2>/dev/null || true
sleep 1

# Build
echo "Building compositor..."
cargo build 2>&1 | tail -2

# Start compositor in background
LOG_FILE="compositor-test.log"
echo "Starting compositor..."
./target/debug/swl > "$LOG_FILE" 2>&1 &
COMPOSITOR_PID=$!
echo "Compositor PID: $COMPOSITOR_PID"

# Wait for initialization
echo "Waiting for compositor to initialize..."
sleep 3

# Check if running
if ! kill -0 $COMPOSITOR_PID 2>/dev/null; then
    echo "ERROR: Compositor failed to start"
    cat "$LOG_FILE"
    exit 1
fi

# Get the socket name from logs
SOCKET_NAME=$(grep "Listening on wayland socket:" "$LOG_FILE" | awk '{print $NF}')
if [ -z "$SOCKET_NAME" ]; then
    echo "ERROR: Could not find socket name"
    cat "$LOG_FILE"
    kill $COMPOSITOR_PID 2>/dev/null || true
    exit 1
fi

echo "✓ Compositor running with socket: $SOCKET_NAME"

# Test connection
export WAYLAND_DISPLAY=$SOCKET_NAME
echo "Testing Wayland connection..."

if command -v weston-info > /dev/null 2>&1; then
    echo "Running weston-info..."
    timeout 2 weston-info 2>&1 | head -20 || true
fi

# Try to spawn a terminal
if command -v foot > /dev/null 2>&1; then
    echo ""
    echo "Spawning foot terminal with WAYLAND_DISPLAY=$SOCKET_NAME..."
    # Make sure foot uses our compositor's socket
    unset WAYLAND_DISPLAY
    export WAYLAND_DISPLAY=$SOCKET_NAME
    echo "WAYLAND_DISPLAY is set to: $WAYLAND_DISPLAY"
    
    # Start foot with our display
    WAYLAND_DISPLAY=$SOCKET_NAME foot --log-level=info > foot.log 2>&1 &
    TERM_PID=$!
    echo "Started foot with PID $TERM_PID"
    
    # Give it more time to connect
    sleep 3
    
    # Check if foot is still running
    if kill -0 $TERM_PID 2>/dev/null; then
        echo "Foot is still running, checking for window creation..."
    else
        echo "Foot exited, checking its log..."
        cat foot.log | head -20
    fi
    
    # Check if window was created in logs
    echo ""
    echo "Checking compositor log for window creation..."
    grep -E "(New window|window|toplevel|surface commit|client)" "$LOG_FILE" | tail -15 || echo "No window messages found"
    
    # Kill terminal if still running
    kill $TERM_PID 2>/dev/null || true
elif command -v weston-terminal > /dev/null 2>&1; then
    echo ""
    echo "Spawning weston-terminal..."
    weston-terminal > /dev/null 2>&1 &
    TERM_PID=$!
    sleep 2
    
    # Check if window was created in logs
    echo ""
    echo "Checking for window creation..."
    grep -E "(window|toplevel|surface commit)" "$LOG_FILE" | tail -10 || echo "No window messages found"
    
    # Kill terminal
    kill $TERM_PID 2>/dev/null || true
fi

echo ""
echo "=== Last 20 compositor logs ==="
tail -20 "$LOG_FILE"

echo ""
echo "=== Test Complete ==="
echo "Stopping compositor..."
kill $COMPOSITOR_PID 2>/dev/null || true
sleep 1

echo "Full log saved to: $LOG_FILE"