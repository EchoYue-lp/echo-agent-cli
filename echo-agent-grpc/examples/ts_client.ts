/**
 * TypeScript gRPC client for Echo Agent
 *
 * Usage:
 *   npm install @grpc/grpc-js
 *   npx ts-node examples/ts_client.ts
 */

import * as grpc from '@grpc/grpc-js';
import * as protoLoader from '@grpc/proto-loader';
import * as path from 'path';

const PROTO_PATH = path.join(__dirname, '../proto/agent.proto');

const packageDefinition = protoLoader.loadSync(PROTO_PATH, {
  keepCase: true,
  longs: String,
  enums: String,
  defaults: true,
  oneofs: true,
});

const agentProto = grpc.loadPackageDefinition(packageDefinition).echoagent as any;

function main() {
  const client = new agentProto.AgentService(
    'localhost:50051',
    grpc.credentials.createInsecure()
  );

  // Execute a task
  client.Execute({ task: 'Search for papers about LLM agents' }, (err: any, response: any) => {
    if (err) {
      console.error('Execute error:', err);
      return;
    }
    console.log('Execute response:', response.result);
  });

  // Stream chat
  const stream = client.ChatStream({ message: 'Hello, agent!' });
  stream.on('data', (chunk: any) => {
    if (chunk.token) {
      process.stdout.write(chunk.token.data);
    } else if (chunk.finalAnswer) {
      console.log('\nFinal:', chunk.finalAnswer.data);
    } else if (chunk.error) {
      console.error('\nError:', chunk.error.message);
    }
  });
  stream.on('error', (err: any) => {
    console.error('Stream error:', err);
  });

  // Get status
  client.GetStatus({}, (err: any, response: any) => {
    if (err) {
      console.error('Status error:', err);
      return;
    }
    console.log('Status:', response.status, 'Version:', response.version);
  });
}

main();
