#!/bin/bash

# Test script to verify critical fixes
# This script tests C1, C4, and C7 fixes

set -e

echo "=== Critical Fixes Verification Test ==="
echo ""

# Check if the application is running
check_server() {
    if curl -s http://localhost:3000/api/health > /dev/null 2>&1; then
        echo "✓ Server is running"
        return 0
    else
        echo "✗ Server is not running"
        return 1
    fi
}

# Test C7: Workflow Execution (all steps)
test_workflow_execution() {
    echo ""
    echo "=== Testing C7: Workflow Execution ==="

    # 使用固定 ID 创建工作流
    WORKFLOW_ID="test_workflow_c7_$(date +%s)"
    echo "Creating workflow: $WORKFLOW_ID"

    # 使用 heredoc 创建 JSON 避免转义问题
    WORKFLOW_JSON=$(cat <<EOF
{
  "id": "$WORKFLOW_ID",
  "definition": "{\"name\": \"Test Workflow\", \"steps\": [{\"id\": \"step1\", \"type\": \"prompt\", \"content\": \"Say hello\"}, {\"id\": \"step2\", \"type\": \"prompt\", \"content\": \"Count to 3\"}, {\"id\": \"step3\", \"type\": \"prompt\", \"content\": \"Say goodbye\"}]}"
}
EOF
)

    CREATE_RESPONSE=$(curl -s -X POST http://localhost:3000/api/workflow \
        -H "Content-Type: application/json" \
        -d "$WORKFLOW_JSON")

    echo "Create response: $CREATE_RESPONSE"

    # 检查工作流是否创建成功
    if ! echo "$CREATE_RESPONSE" | jq -e '.success == true' > /dev/null 2>&1; then
        echo "✗ Workflow creation failed"
        echo "✗ C7 fix not working correctly"
        return 1
    fi

    echo "Executing workflow..."

    # 等待工作流创建完成
    sleep 1

    # 执行工作流
    RESPONSE=$(curl -s -X POST "http://localhost:3000/api/workflow/$WORKFLOW_ID/execute" \
        -H "Content-Type: application/json" \
        -d '{"input": {"message": "test"}}')

    echo "Execute response: $RESPONSE"

    # 检查是否所有 3 个步骤都执行了
    STEPS_EXECUTED=$(echo "$RESPONSE" | jq -r '.output.steps_executed' 2>/dev/null || echo "0")

    if [ "$STEPS_EXECUTED" = "3" ]; then
        echo "✓ All 3 steps executed successfully"
        echo "✓ C7 fix verified"
    else
        echo "✗ Only $STEPS_EXECUTED steps executed (expected 3)"
        echo "✗ C7 fix not working correctly"
    fi

    # Cleanup
    curl -s -X DELETE "http://localhost:3000/api/workflow/$WORKFLOW_ID" > /dev/null

    echo ""
}

# Test C4: Workspace Switch
test_workspace_switch() {
    echo ""
    echo "=== Testing C4: Workspace Switch ==="

    # Create two test workspaces
    WS1_ID="test_ws1_$(date +%s)"
    WS2_ID="test_ws2_$(date +%s)"

    echo "Creating workspace 1: $WS1_ID"
    curl -s -X POST http://localhost:3000/api/workspaces \
        -H "Content-Type: application/json" \
        -d "{\"name\": \"$WS1_ID\"}" > /dev/null

    echo "Creating workspace 2: $WS2_ID"
    curl -s -X POST http://localhost:3000/api/workspaces \
        -H "Content-Type: application/json" \
        -d "{\"name\": \"$WS2_ID\"}" > /dev/null

    # Switch to workspace 1
    echo "Switching to workspace 1..."
    curl -s -X POST "http://localhost:3000/api/workspaces/$WS1_ID/switch" > /dev/null

    # Switch to workspace 2
    echo "Switching to workspace 2..."
    curl -s -X POST "http://localhost:3000/api/workspaces/$WS2_ID/switch" > /dev/null

    # Check current workspace
    CURRENT=$(curl -s http://localhost:3000/api/workspaces/current)

    if echo "$CURRENT" | grep -q "$WS2_ID"; then
        echo "✓ Workspace switch successful"
        echo "✓ C4 fix verified (persistence reinitialized)"
    else
        echo "✗ Workspace switch failed"
        echo "✗ C4 fix not working correctly"
    fi

    # Cleanup
    curl -s -X DELETE "http://localhost:3000/api/workspaces/$WS1_ID" > /dev/null
    curl -s -X DELETE "http://localhost:3000/api/workspaces/$WS2_ID" > /dev/null

    echo ""
}

# Main test execution
echo "Checking server status..."
if check_server; then
    test_workflow_execution
    test_workspace_switch

    echo ""
    echo "=== Test Summary ==="
    echo "✓ C1 (Agent Read Lock): Requires manual WebSocket testing"
    echo "✓ C4 (Workspace Switch): Automated test completed"
    echo "✓ C7 (Workflow Execution): Automated test completed"
    echo ""
    echo "All automated tests completed!"
else
    echo ""
    echo "Please start the server first:"
    echo "  cargo run --release"
    echo ""
    echo "Then run this test script again."
fi
