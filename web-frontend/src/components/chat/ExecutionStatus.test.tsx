// @vitest-environment jsdom
import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useChatStore } from '../../stores/chatStore';
import { ExecutionStatus } from './ExecutionStatus';

describe('长操作的真实状态显示', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-10-27T12:00:00Z'));
    useChatStore.getState().clearMessages();
    useChatStore.getState().startAssistantMessage();
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it('等待时只推进耗时，最后事件时间和正文保持原事实', () => {
    const store = useChatStore.getState();
    store.observeActivity('tool:1', '执行工具 run_command');
    const lastEvent = useChatStore.getState().activity?.lastEventAt;
    render(<ExecutionStatus />);
    act(() => vi.advanceTimersByTime(61_000));
    expect(screen.getByText('已用时 61 秒')).toBeTruthy();
    expect(screen.getByText('执行工具 run_command')).toBeTruthy();
    expect(useChatStore.getState().activity?.lastEventAt).toBe(lastEvent);
    expect(useChatStore.getState().messages.at(-1)?.content).toBe('');
    expect(useChatStore.getState().isStreaming).toBe(true);
    act(() => useChatStore.getState().noteActivityEvent());
    expect(useChatStore.getState().activity?.lastEventAt).toBe(Date.now());
    expect(useChatStore.getState().activity?.startedAt).toBe(lastEvent);
  });

  it('新操作单独计时，完成和切换历史清除等待状态', () => {
    useChatStore.getState().observeActivity('model', '模型正在响应');
    act(() => vi.advanceTimersByTime(30_000));
    useChatStore.getState().observeActivity('tool:1', '执行工具 read_file');
    render(<ExecutionStatus />);
    expect(screen.getByText('已用时 0 秒')).toBeTruthy();
    act(() => useChatStore.getState().setRunStatus('completed'));
    expect(screen.queryByLabelText('当前执行状态')).toBeNull();
    expect(useChatStore.getState().activity).toBeNull();
    useChatStore.getState().replaceMessages([]);
    expect(useChatStore.getState().activity).toBeNull();
  });
});
