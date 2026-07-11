import { beforeEach, describe, expect, it } from 'vitest';
import { useChatStore } from './chatStore';

describe('chat tool execution projection', () => {
  beforeEach(() => {
    useChatStore.getState().clearMessages();
    useChatStore.setState({ currentRound: null, runStatus: 'idle' });
  });

  it('keeps same-name parallel tools isolated by call id', () => {
    const store = useChatStore.getState();
    store.startAssistantMessage('assistant-test');
    store.startToolBatch(2);
    store.setToolCall('call-a', 'shell', { command: 'printf a' });
    store.setToolCall('call-b', 'shell', { command: 'printf b' });

    store.appendToolOutput('call-b', 'stdout', 'b');
    store.completeToolCall('call-b', 'b', true);
    store.appendToolOutput('call-a', 'stderr', 'warning');
    store.completeToolCall('call-a', 'failed', false);

    const tools = useChatStore
      .getState()
      .messages.find((message) => message.isStreaming)?.toolCalls;
    expect(tools).toHaveLength(2);
    expect(tools?.find((tool) => tool.id === 'call-a')).toMatchObject({
      status: 'failed',
      stderr: 'warning',
    });
    expect(tools?.find((tool) => tool.id === 'call-b')).toMatchObject({
      status: 'succeeded',
      stdout: 'b',
    });
  });

  it('shows a started tool as running instead of successful', () => {
    const store = useChatStore.getState();
    store.startAssistantMessage('assistant-running');
    store.setToolCall('call-running', 'shell', { command: 'sleep 1' });

    const tool = useChatStore.getState().messages[0]?.toolCalls?.[0];
    expect(tool).toMatchObject({
      id: 'call-running',
      status: 'running',
      success: false,
    });
    expect(tool).not.toHaveProperty('finishedAt');
  });

  it('folds terminal metadata into the stable call projection', () => {
    const store = useChatStore.getState();
    store.startAssistantMessage('assistant-metadata');
    store.setToolCall('call-meta', 'shell', { command: 'exit 7' });
    store.completeToolStream('call-meta', false, { exit_code: '7', duration_ms: '1250' }, true);

    const tool = useChatStore.getState().messages[0]?.toolCalls?.[0];
    expect(tool).toMatchObject({
      id: 'call-meta',
      status: 'failed',
      success: false,
      truncated: true,
      metadata: { exit_code: '7', duration_ms: '1250' },
    });
  });

  it('marks running tools failed when the stream aborts', () => {
    const store = useChatStore.getState();
    store.startAssistantMessage('assistant-error');
    store.setToolCall('call-error', 'shell', { command: 'sleep 10' });
    store.markRunningToolsFailed('connection lost');

    expect(useChatStore.getState().messages[0]?.toolCalls?.[0]).toMatchObject({
      status: 'failed',
      success: false,
      stderr: 'connection lost',
    });
  });
});
