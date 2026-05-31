"""
Python gRPC client for Echo Agent

Usage:
    pip install grpcio grpcio-tools
    python examples/python_client.py
"""

import grpc
import agent_pb2
import agent_pb2_grpc


def run():
    # Connect to the gRPC server
    channel = grpc.insecure_channel("localhost:50051")
    stub = agent_pb2_grpc.AgentServiceStub(channel)

    # Execute a task
    request = agent_pb2.ExecuteRequest(
        task="Search for papers about LLM agents",
        session_id=""
    )
    try:
        response = stub.Execute(request)
        print(f"Execute response: {response.result}")
        print(f"Success: {response.success}")
        print(f"Iterations: {response.iterations}")
    except grpc.RpcError as e:
        print(f"RPC failed: {e.code()}: {e.details()}")

    # Stream chat
    chat_request = agent_pb2.ChatStreamRequest(
        message="Hello, agent!",
        session_id=""
    )
    try:
        for chunk in stub.ChatStream(chat_request):
            if chunk.HasField("token"):
                print(chunk.token.data, end="")
            elif chunk.HasField("final_answer"):
                print(f"\nFinal: {chunk.final_answer.data}")
            elif chunk.HasField("error"):
                print(f"\nError: {chunk.error.message}")
    except grpc.RpcError as e:
        print(f"Chat stream failed: {e.code()}: {e.details()}")

    # Get status
    try:
        status = stub.GetStatus(agent_pb2.StatusRequest())
        print(f"Status: {status.status}, Version: {status.version}")
    except grpc.RpcError as e:
        print(f"Status failed: {e.code()}: {e.details()}")


if __name__ == "__main__":
    run()
