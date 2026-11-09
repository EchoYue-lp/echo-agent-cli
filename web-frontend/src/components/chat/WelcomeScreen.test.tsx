// @vitest-environment jsdom
import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({ latest: vi.fn(), load: vi.fn(), beginNavigation: vi.fn() }));
vi.mock('../../api/endpoints', () => ({ sessionApi: { getLatest: mocks.latest } }));
vi.mock('../../stores/conversationStore', () => ({
  useConversationStore: Object.assign(
    (select: (state: { workspaceId: string; loadConversation: typeof mocks.load }) => unknown) =>
      select({ workspaceId: 'workspace-a', loadConversation: mocks.load }),
    { getState: () => ({ workspaceId: 'workspace-a' }) }
  ),
}));
vi.mock('../../stores/uiStore', () => ({
  useUiStore: { getState: () => ({ beginNavigation: mocks.beginNavigation }) },
}));

import { WelcomeScreen } from './WelcomeScreen';

beforeEach(() => {
  vi.clearAllMocks();
  mocks.latest.mockResolvedValue({
    found: true,
    id: 'other-parallel-task',
    title: '另一个并行任务',
    updated_at: '2026-10-27T00:00:00Z',
    message_count: 6,
  });
});
afterEach(cleanup);

async function showHome() {
  await act(async () => {
    render(<WelcomeScreen />);
  });
}

describe('首页明确任务选择', () => {
  it('使用用户选定主标题并保留简短品牌副文案', async () => {
    await showHome();
    expect(screen.getByRole('heading', { level: 1, name: '把想法变成成果' })).toBeTruthy();
    expect(screen.getByText('听见你的声音，让想法飞跃。')).toBeTruthy();
    expect(screen.getByLabelText('品牌读音').textContent).toBe('Eclip /ˈɛk.lɪp/');
  });

  it('进入首页不查询最后会话，也不自动恢复其它并行任务', async () => {
    await showHome();
    expect(mocks.latest).not.toHaveBeenCalled();
    expect(mocks.load).not.toHaveBeenCalled();
    expect(mocks.beginNavigation).not.toHaveBeenCalled();
  });

  it('即使存在最近会话也不显示继续上次对话卡片', async () => {
    await showHome();
    expect(screen.queryByRole('button', { name: /继续上次对话|正在恢复会话/ })).toBeNull();
    expect(screen.queryByText('另一个并行任务')).toBeNull();
  });

  it('首页继续引用既有装饰性品牌素材', async () => {
    await showHome();
    const images = document.querySelectorAll('img');
    expect(images).toHaveLength(1);
    expect(images.item(0)?.getAttribute('src')).toBe('/app-icon.svg');
    expect(images.item(0)?.getAttribute('aria-hidden')).toBe('true');
  });
});
