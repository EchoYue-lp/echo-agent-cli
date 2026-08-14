// @vitest-environment jsdom
import { fireEvent, render } from '@testing-library/react';
import { createRef } from 'react';
import { describe, expect, it, vi } from 'vitest';
import { Modal } from './Modal';

describe('Modal', () => {
  it('announces itself, traps focus, closes with Escape, and restores focus', () => {
    const onClose = vi.fn();
    const initialFocusRef = createRef<HTMLButtonElement>();
    const opener = document.createElement('button');
    document.body.append(opener);
    opener.focus();

    const { getByRole, unmount } = render(
      <Modal onClose={onClose} ariaLabel="Test dialog" initialFocusRef={initialFocusRef}>
        <button ref={initialFocusRef}>First</button>
        <button>Last</button>
      </Modal>
    );

    const dialog = getByRole('dialog', { name: 'Test dialog' });
    const first = getByRole('button', { name: 'First' });
    const last = getByRole('button', { name: 'Last' });
    expect(dialog.getAttribute('aria-modal')).toBe('true');
    expect(document.activeElement).toBe(first);

    last.focus();
    fireEvent.keyDown(document, { key: 'Tab' });
    expect(document.activeElement).toBe(first);

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledOnce();

    unmount();
    expect(document.activeElement).toBe(opener);
    opener.remove();
  });

  it('closes through the backdrop button', () => {
    const onClose = vi.fn();
    const { getByRole } = render(
      <Modal onClose={onClose} ariaLabel="Backdrop dialog">
        <button>Action</button>
      </Modal>
    );

    fireEvent.click(getByRole('button', { name: '关闭对话框' }));
    expect(onClose).toHaveBeenCalledOnce();
  });

  it('keeps focus inside across rerenders and uses the latest close handler', () => {
    const firstClose = vi.fn();
    const latestClose = vi.fn();
    const opener = document.createElement('button');
    const outside = document.createElement('button');
    document.body.append(opener, outside);
    opener.focus();

    const { getByRole, rerender, unmount } = render(
      <Modal onClose={firstClose} ariaLabel="Stable dialog">
        <button>Inside</button>
      </Modal>
    );
    const inside = getByRole('button', { name: 'Inside' });
    expect(document.activeElement).toBe(inside);

    rerender(
      <Modal onClose={latestClose} ariaLabel="Stable dialog">
        <button>Inside</button>
      </Modal>
    );
    expect(document.activeElement).toBe(inside);

    outside.focus();
    fireEvent.keyDown(document, { key: 'Tab' });
    expect(document.activeElement).toBe(inside);

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(firstClose).not.toHaveBeenCalled();
    expect(latestClose).toHaveBeenCalledOnce();

    unmount();
    expect(document.activeElement).toBe(opener);
    opener.remove();
    outside.remove();
  });

  it('suspends keyboard handling while covered by a nested modal', () => {
    const onClose = vi.fn();
    const opener = document.createElement('button');
    document.body.append(opener);
    opener.focus();

    const { getByRole, rerender, unmount } = render(
      <Modal onClose={onClose} ariaLabel="Underlying dialog" active={false}>
        <button>Underlying action</button>
      </Modal>
    );
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(opener);

    rerender(
      <Modal onClose={onClose} ariaLabel="Underlying dialog">
        <button>Underlying action</button>
      </Modal>
    );
    expect(document.activeElement).toBe(getByRole('button', { name: 'Underlying action' }));

    unmount();
    opener.remove();
  });
});
