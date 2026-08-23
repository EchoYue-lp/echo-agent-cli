import { fireEvent, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { useRightWorkspaceStore } from '../../stores/rightWorkspaceStore';
import { AutomationPanel } from './AutomationPanel';

vi.mock('../workflow/WorkflowPanel', () => ({
  WorkflowPanel: () => <div>workflow-production-panel</div>,
}));

vi.mock('../extract/ExtractPanel', () => ({
  ExtractPanel: () => <div>extract-production-panel</div>,
}));

describe('AutomationPanel', () => {
  beforeEach(() => {
    useRightWorkspaceStore.setState({ automationView: 'workflows' });
  });

  it('mounts both production surfaces through visible tabs', () => {
    render(<AutomationPanel />);
    expect(screen.getByText('workflow-production-panel')).toBeTruthy();

    fireEvent.click(screen.getByRole('tab', { name: '结构化提取' }));
    expect(screen.getByText('extract-production-panel')).toBeTruthy();
  });
});
// @vitest-environment jsdom
