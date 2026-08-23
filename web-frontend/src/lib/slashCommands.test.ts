import { describe, expect, it, vi } from 'vitest';
import {
  SLASH_COMMANDS,
  dispatchGuiSlashCommand,
  filterCommands,
  type GuiSlashHandlers,
} from './slashCommands';

function handlers() {
  return {
    clear: vi.fn(),
    tasks: vi.fn(),
    analysis: vi.fn(),
    research: vi.fn(),
    browser: vi.fn(),
    files: vi.fn(),
    workflows: vi.fn(),
    extract: vi.fn(),
  } satisfies GuiSlashHandlers;
}

describe('GUI slash command registry', () => {
  it('dispatches every visible command to a real GUI handler', async () => {
    for (const command of SLASH_COMMANDS) {
      const commandHandlers = handlers();
      await expect(dispatchGuiSlashCommand(command.name, commandHandlers)).resolves.toBe(true);
      expect(commandHandlers[command.target]).toHaveBeenCalledOnce();
    }
  });

  it('keeps unsupported chat-like commands out of the palette', async () => {
    expect(filterCommands('/model')).toEqual([]);
    expect(filterCommands('/git')).toEqual([]);
    await expect(dispatchGuiSlashCommand('/unknown', handlers())).resolves.toBe(false);
  });

  it('dispatches aliases to the same handler', async () => {
    const commandHandlers = handlers();
    await expect(dispatchGuiSlashCommand('/papers', commandHandlers)).resolves.toBe(true);
    expect(commandHandlers.research).toHaveBeenCalledOnce();
  });
});
