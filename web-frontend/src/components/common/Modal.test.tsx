// @vitest-environment jsdom
import { fireEvent, render } from '@testing-library/react';
import { createRef, useState } from 'react';
import { describe, expect, it, vi } from 'vitest';
import CommandPalette from './CommandPalette';
import { Modal } from './Modal';

function LayeredModalHarness() {
  const [dialogOpen, setDialogOpen] = useState(true);
  const [paletteOpen, setPaletteOpen] = useState(false);

  return (
    <>
      {dialogOpen && (
        <Modal onClose={() => setDialogOpen(false)} ariaLabel="Underlying dialog">
          <button onClick={() => setPaletteOpen(true)}>Open command palette</button>
        </Modal>
      )}
      <CommandPalette isOpen={paletteOpen} onClose={() => setPaletteOpen(false)} commands={[]} />
    </>
  );
}

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

  it('keeps dialog controls above the backdrop interception layer', () => {
    const onClose = vi.fn();
    const onAction = vi.fn();
    const { getByRole } = render(
      <Modal onClose={onClose} ariaLabel="Layered dialog">
        <button onClick={onAction}>Action</button>
      </Modal>
    );

    const dialog = getByRole('dialog', { name: 'Layered dialog' });
    const backdrop = getByRole('button', { name: '关闭对话框' });
    expect(dialog.classList.contains('z-10')).toBe(true);
    expect(backdrop.classList.contains('z-0')).toBe(true);

    fireEvent.click(getByRole('button', { name: 'Action' }));
    expect(onAction).toHaveBeenCalledOnce();
    expect(onClose).not.toHaveBeenCalled();
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

  it('closes only the topmost palette and restores focus through the modal stack', () => {
    const opener = document.createElement('button');
    document.body.append(opener);
    opener.focus();

    const { getByRole, queryByRole } = render(<LayeredModalHarness />);
    const underlyingDialog = getByRole('dialog', { name: 'Underlying dialog' });
    const paletteButton = getByRole('button', { name: 'Open command palette' });
    fireEvent.click(paletteButton);

    expect(getByRole('dialog', { name: '命令面板' })).toBeTruthy();
    expect(underlyingDialog.parentElement?.getAttribute('aria-hidden')).toBe('true');

    fireEvent.keyDown(document, { key: 'Escape' });

    expect(queryByRole('dialog', { name: '命令面板' })).toBeNull();
    expect(getByRole('dialog', { name: 'Underlying dialog' })).toBeTruthy();
    expect(underlyingDialog.parentElement?.getAttribute('aria-hidden')).toBe('false');
    expect(document.activeElement).toBe(paletteButton);

    fireEvent.keyDown(document, { key: 'Escape' });

    expect(queryByRole('dialog', { name: 'Underlying dialog' })).toBeNull();
    expect(document.activeElement).toBe(opener);
    opener.remove();
  });
});
