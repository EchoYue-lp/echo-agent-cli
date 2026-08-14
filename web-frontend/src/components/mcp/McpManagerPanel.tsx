import { useState } from 'react';

interface McpServerConfig {
  name: string;
  command: string;
  args: string[];
  env: Record<string, string>;
  transport: 'stdio' | 'http' | 'sse';
  url?: string;
  headers?: Record<string, string>;
  enabled: boolean;
  description?: string;
}

interface McpManagerPanelProps {
  servers: McpServerConfig[];
  onAddServer: (server: Omit<McpServerConfig, 'enabled'>) => void;
  onRemoveServer: (name: string) => void;
  onUpdateServer: (name: string, config: Partial<McpServerConfig>) => void;
  onTestConnection: (name: string) => Promise<{ success: boolean; message: string }>;
  onToggleServer: (name: string) => void;
}

const transportConfig = {
  stdio: { icon: '💻', label: 'Stdio' },
  http: { icon: '🌐', label: 'HTTP' },
  sse: { icon: '📡', label: 'SSE' },
};

export function McpManagerPanel({
  servers,
  onAddServer,
  onRemoveServer,
  onTestConnection,
  onToggleServer,
}: McpManagerPanelProps) {
  const [editingServer, setEditingServer] = useState<string | null>(null);
  const [newServer, setNewServer] = useState<Partial<McpServerConfig>>({
    transport: 'stdio',
    args: [],
    env: {},
  });
  const [testResults, setTestResults] = useState<
    Record<string, { success: boolean; message: string }>
  >({});

  const handleTest = async (name: string) => {
    const result = await onTestConnection(name);
    setTestResults((prev) => ({ ...prev, [name]: result }));
  };

  const handleAdd = () => {
    if (newServer.name && newServer.command) {
      onAddServer(newServer as Omit<McpServerConfig, 'enabled'>);
      setNewServer({ transport: 'stdio', args: [], env: {} });
      setEditingServer(null);
    }
  };

  return (
    <div className="h-full flex flex-col">
      {/* Header */}
      <div className="flex items-center justify-between px-5 py-3 bg-[var(--bg-secondary)] border-b border-[var(--border-primary)]">
        <div className="flex items-center gap-2">
          <svg
            className="w-5 h-5 text-[var(--accent)]"
            fill="none"
            viewBox="0 0 24 24"
            stroke="currentColor"
          >
            <path
              strokeLinecap="round"
              strokeLinejoin="round"
              strokeWidth={2}
              d="M19 11H5m14 0a2 2 0 012 2v6a2 2 0 01-2 2H5a2 2 0 01-2-2v-6a2 2 0 012-2m14 0V9a2 2 0 00-2-2M5 11V9a2 2 0 012-2m0 0V5a2 2 0 012-2h6a2 2 0 012 2v2M7 7h10"
            />
          </svg>
          <span className="text-sm font-semibold text-[var(--text-primary)]">MCP Servers</span>
          <span className="text-xs text-[var(--text-tertiary)] bg-[var(--bg-hover)] px-2 py-0.5 rounded-full">
            {servers.length}
          </span>
        </div>
        <button
          onClick={() => setEditingServer('new')}
          className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium
            bg-[var(--accent)] text-[var(--text-on-accent)] hover:opacity-90 transition-opacity"
        >
          <svg className="w-3.5 h-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 4v16m8-8H4" />
          </svg>
          Add Server
        </button>
      </div>

      {/* Add Server Form */}
      {editingServer === 'new' && (
        <div className="p-4 bg-[var(--bg-secondary)] border-b border-[var(--border-primary)] animate-slide-up">
          <h3 className="text-sm font-semibold text-[var(--text-primary)] mb-3">
            Add New MCP Server
          </h3>
          <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
            <div>
              <label
                htmlFor="mcp-server-name"
                className="block text-xs text-[var(--text-tertiary)] mb-1"
              >
                Server Name
              </label>
              <input
                id="mcp-server-name"
                placeholder="e.g., playwright"
                value={newServer.name || ''}
                onChange={(e) => setNewServer({ ...newServer, name: e.target.value })}
                className="w-full px-3 py-2 rounded-lg bg-[var(--bg-primary)] border border-[var(--border-primary)] text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--border-focus)] focus:ring-2 focus:ring-[var(--accent-bg)]"
              />
            </div>
            <div>
              <label
                htmlFor="mcp-server-transport"
                className="block text-xs text-[var(--text-tertiary)] mb-1"
              >
                Transport
              </label>
              <select
                id="mcp-server-transport"
                value={newServer.transport}
                onChange={(e) =>
                  setNewServer({
                    ...newServer,
                    transport: e.target.value as 'stdio' | 'http' | 'sse',
                  })
                }
                className="w-full px-3 py-2 rounded-lg bg-[var(--bg-primary)] border border-[var(--border-primary)] text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--border-focus)]"
              >
                <option value="stdio">Stdio</option>
                <option value="http">HTTP</option>
                <option value="sse">SSE</option>
              </select>
            </div>
            <div className="md:col-span-2">
              <label
                htmlFor="mcp-server-command"
                className="block text-xs text-[var(--text-tertiary)] mb-1"
              >
                Command
              </label>
              <input
                id="mcp-server-command"
                placeholder="e.g., npx -y @playwright/mcp@latest"
                value={newServer.command || ''}
                onChange={(e) => setNewServer({ ...newServer, command: e.target.value })}
                className="w-full px-3 py-2 rounded-lg bg-[var(--bg-primary)] border border-[var(--border-primary)] text-sm text-[var(--text-primary)] font-mono focus:outline-none focus:border-[var(--border-focus)] focus:ring-2 focus:ring-[var(--accent-bg)]"
              />
            </div>
            <div className="md:col-span-2">
              <label
                htmlFor="mcp-server-description"
                className="block text-xs text-[var(--text-tertiary)] mb-1"
              >
                Description (optional)
              </label>
              <textarea
                id="mcp-server-description"
                placeholder="Brief description of this MCP server"
                value={newServer.description || ''}
                onChange={(e) => setNewServer({ ...newServer, description: e.target.value })}
                rows={2}
                className="w-full px-3 py-2 rounded-lg bg-[var(--bg-primary)] border border-[var(--border-primary)] text-sm text-[var(--text-primary)] focus:outline-none focus:border-[var(--border-focus)] focus:ring-2 focus:ring-[var(--accent-bg)] resize-y"
              />
            </div>
          </div>
          <div className="flex items-center gap-2 mt-3">
            <button
              onClick={handleAdd}
              className="px-4 py-2 rounded-lg text-sm font-medium bg-[var(--accent)] text-[var(--text-on-accent)] hover:opacity-90 transition-opacity"
            >
              Add Server
            </button>
            <button
              onClick={() => setEditingServer(null)}
              className="px-4 py-2 rounded-lg text-sm font-medium text-[var(--text-secondary)] hover:text-[var(--text-primary)] hover:bg-[var(--bg-hover)] transition-colors"
            >
              Cancel
            </button>
          </div>
        </div>
      )}

      {/* Server List */}
      <div className="flex-1 overflow-y-auto p-4">
        {servers.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-16 text-center">
            <div className="text-4xl mb-3">🔌</div>
            <p className="text-sm text-[var(--text-tertiary)] mb-2">No MCP servers configured</p>
            <p className="text-xs text-[var(--text-tertiary)]">
              Add a server to extend agent capabilities
            </p>
          </div>
        ) : (
          <div className="space-y-3">
            {servers.map((server) => {
              const transport = transportConfig[server.transport];
              return (
                <div
                  key={server.name}
                  className={`rounded-xl border transition-all ${
                    server.enabled
                      ? 'border-[var(--border-primary)] bg-[var(--bg-primary)]'
                      : 'border-dashed border-[var(--border-primary)] bg-[var(--bg-secondary)] opacity-75'
                  }`}
                >
                  <div className="p-4">
                    <div className="flex items-start justify-between">
                      <div className="flex items-start gap-3">
                        {/* Status indicator */}
                        <div
                          className={`mt-1 w-2.5 h-2.5 rounded-full flex-shrink-0 ${server.enabled ? 'bg-[var(--color-success)]' : 'bg-[var(--text-tertiary)]'}`}
                        />
                        <div>
                          <div className="flex items-center gap-2 mb-1">
                            <h4 className="text-sm font-semibold text-[var(--text-primary)]">
                              {server.name}
                            </h4>
                            <span
                              className={`text-xs px-2 py-0.5 rounded-md font-medium ${
                                server.enabled
                                  ? 'bg-[var(--color-success-bg)] text-[var(--color-success-text)]'
                                  : 'bg-[var(--bg-hover)] text-[var(--text-secondary)] dark:text-[var(--text-tertiary)]'
                              }`}
                            >
                              {server.enabled ? 'Active' : 'Inactive'}
                            </span>
                          </div>
                          <div className="flex items-center gap-2 text-xs text-[var(--text-tertiary)] mb-1">
                            <span className="inline-flex items-center gap-1">
                              <span>{transport.icon}</span>
                              {transport.label}
                            </span>
                            <span>·</span>
                            <code className="font-mono text-xs">{server.command}</code>
                          </div>
                          {server.description && (
                            <p className="text-xs text-[var(--text-secondary)]">
                              {server.description}
                            </p>
                          )}
                        </div>
                      </div>

                      {/* Actions */}
                      <div className="flex items-center gap-1 ml-4">
                        <button
                          onClick={() => onToggleServer(server.name)}
                          className={`p-2 rounded-lg transition-colors ${
                            server.enabled
                              ? 'text-[var(--color-success-text)] hover:bg-[var(--color-success-bg)]'
                              : 'text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)]'
                          }`}
                          title={server.enabled ? 'Disable' : 'Enable'}
                        >
                          {server.enabled ? (
                            <svg
                              className="w-5 h-5"
                              fill="none"
                              viewBox="0 0 24 24"
                              stroke="currentColor"
                            >
                              <path
                                strokeLinecap="round"
                                strokeLinejoin="round"
                                strokeWidth={2}
                                d="M10 9v6m4-6v6m7-3a9 9 0 11-18 0 9 9 0 0118 0z"
                              />
                            </svg>
                          ) : (
                            <svg
                              className="w-5 h-5"
                              fill="none"
                              viewBox="0 0 24 24"
                              stroke="currentColor"
                            >
                              <path
                                strokeLinecap="round"
                                strokeLinejoin="round"
                                strokeWidth={2}
                                d="M14.752 11.168l-3.197-2.132A1 1 0 0010 9.87v4.263a1 1 0 001.555.832l3.197-2.132a1 1 0 000-1.664z"
                              />
                              <path
                                strokeLinecap="round"
                                strokeLinejoin="round"
                                strokeWidth={2}
                                d="M21 12a9 9 0 11-18 0 9 9 0 0118 0z"
                              />
                            </svg>
                          )}
                        </button>
                        <button
                          onClick={() => handleTest(server.name)}
                          className="p-2 rounded-lg text-[var(--text-tertiary)] hover:text-[var(--accent)] hover:bg-[var(--accent-bg)] transition-colors"
                          title="Test Connection"
                        >
                          <svg
                            className="w-4 h-4"
                            fill="none"
                            viewBox="0 0 24 24"
                            stroke="currentColor"
                          >
                            <path
                              strokeLinecap="round"
                              strokeLinejoin="round"
                              strokeWidth={2}
                              d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z"
                            />
                          </svg>
                        </button>
                        <button
                          onClick={() => onRemoveServer(server.name)}
                          className="p-2 rounded-lg text-[var(--text-tertiary)] hover:text-[var(--color-error)] hover:bg-[var(--color-error-bg)] transition-colors"
                          title="Remove"
                        >
                          <svg
                            className="w-4 h-4"
                            fill="none"
                            viewBox="0 0 24 24"
                            stroke="currentColor"
                          >
                            <path
                              strokeLinecap="round"
                              strokeLinejoin="round"
                              strokeWidth={2}
                              d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16"
                            />
                          </svg>
                        </button>
                      </div>
                    </div>

                    {/* Test result */}
                    {testResults[server.name] && (
                      <div
                        className={`mt-3 p-3 rounded-lg text-xs ${
                          testResults[server.name].success
                            ? 'bg-[var(--color-success-bg)] border border-[var(--color-success-bg)] text-[var(--color-success-text)]'
                            : 'bg-[var(--color-error-bg)] border border-[var(--color-error-bg)] text-[var(--color-error-text)]'
                        }`}
                      >
                        {testResults[server.name].message}
                      </div>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

export default McpManagerPanel;
