// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useWorkspaceViewStore } from '../../stores/workspaceViewStore';
import { PrimaryWorkspace } from './PrimaryWorkspace';

vi.mock('../chat/ChatPanel', () => ({ ChatPanel: () => <div>chat-listener-surface</div> }));
vi.mock('../analysis/AnalysisPanel', () => ({ default: () => <div>analysis-surface</div> }));
vi.mock('../papers/PaperPanel', () => ({ PaperPanel: () => <div>research-surface</div> }));
vi.mock('../workflow/WorkflowPanel', () => ({ WorkflowPanel: () => <div>workflow-surface</div> }));
vi.mock('../extract/ExtractPanel', () => ({ ExtractPanel: () => <div>extract-surface</div> }));

describe('PrimaryWorkspace', () => {
  beforeEach(() => useWorkspaceViewStore.setState({ activeView: 'chat' }));
  afterEach(cleanup);

  it('keeps the primary ChatPanel mounted while a workbench is visible', () => {
    render(<PrimaryWorkspace />);
    const chatSurface = screen.getByText('chat-listener-surface');

    act(() => useWorkspaceViewStore.getState().open('analysis'));

    expect(screen.getByText('analysis-surface')).toBeTruthy();
    expect(screen.getByText('chat-listener-surface').isSameNode(chatSurface)).toBe(true);
    expect(chatSurface.closest('[aria-hidden="true"]')).toBeTruthy();
  });

  it('returns to the primary Agent without recreating contextual navigation', () => {
    render(<PrimaryWorkspace />);
    act(() => useWorkspaceViewStore.getState().open('research'));
    expect(screen.getByText('research-surface')).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: '返回主 Agent' }));
    expect(useWorkspaceViewStore.getState().activeView).toBe('chat');
  });
});
