import { useEffect, useState } from 'react';
import {
  pluginApi,
  type PluginInfo,
  type PluginOutputStyle,
  type PluginThemeDefinition,
} from '../../api/endpoints';
import {
  Package,
  Plus,
  Trash2,
  Power,
  PowerOff,
  RefreshCw,
  ChevronDown,
  ChevronRight,
  Search,
  X,
  Check,
  AlertCircle,
  Loader2,
  FolderPlus,
  ShieldCheck,
  Palette,
  TextQuote,
  Save,
} from 'lucide-react';
import { applyPluginTheme, useUiStore } from '../../stores/uiStore';

export function PluginPanel() {
  const [plugins, setPlugins] = useState<PluginInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [showInstall, setShowInstall] = useState(false);
  const [installSource, setInstallSource] = useState('');
  const [installScope, setInstallScope] = useState('user');
  const [installing, setInstalling] = useState(false);
  const [message, setMessage] = useState<{ type: 'success' | 'error'; text: string } | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [showDeveloperTools, setShowDeveloperTools] = useState(false);
  const [developerPath, setDeveloperPath] = useState('');
  const [scaffoldName, setScaffoldName] = useState('my-plugin');
  const [developerBusy, setDeveloperBusy] = useState(false);
  const [pluginThemes, setPluginThemes] = useState<PluginThemeDefinition[]>([]);
  const activeTheme = useUiStore((state) => state.activePluginTheme);
  const [outputStyles, setOutputStyles] = useState<PluginOutputStyle[]>([]);
  const [activeOutputStyle, setActiveOutputStyle] = useState<string | null>(null);
  const [runtimeBusy, setRuntimeBusy] = useState(false);

  const mutationMessage = (
    result: { wiring_ok?: boolean; errors?: string[]; message?: string },
    fallback: string
  ): { type: 'success' | 'error'; text: string } => {
    if (result.wiring_ok === false) {
      const details = result.errors?.join('; ') || 'Some components could not be activated';
      return { type: 'error', text: `${result.message || fallback}: ${details}` };
    }
    return { type: 'success', text: result.message || fallback };
  };

  useEffect(() => {
    loadPlugins();
  }, []);

  const loadPlugins = async () => {
    try {
      setLoading(true);
      const [data, themes, styles] = await Promise.all([
        pluginApi.list(),
        pluginApi.themes(),
        pluginApi.outputStyles(),
      ]);
      setPlugins(data);
      setPluginThemes(themes.themes);
      applyPluginTheme(themes.themes.find((theme) => theme.name === themes.active) || null);
      setOutputStyles(styles.styles);
      setActiveOutputStyle(styles.active);
      setError(null);
    } catch (e: any) {
      setError(e.message || 'Failed to load plugins');
    } finally {
      setLoading(false);
    }
  };

  const handleThemeChange = async (name: string) => {
    try {
      setRuntimeBusy(true);
      const result = await pluginApi.activateTheme(name || null);
      applyPluginTheme(result.theme);
      setMessage({
        type: 'success',
        text: result.active ? `Theme '${result.active}' activated` : 'Theme reset to default',
      });
    } catch (e: any) {
      setMessage({ type: 'error', text: e.message || 'Failed to activate theme' });
    } finally {
      setRuntimeBusy(false);
    }
  };

  const handleConfigure = async (name: string, values: Record<string, unknown>) => {
    const result = await pluginApi.configure(name, values);
    if (!result.success) {
      throw new Error(result.errors?.join('; ') || result.error || 'Configuration failed');
    }
    setMessage({ type: 'success', text: result.message || `Plugin '${name}' configured` });
    await loadPlugins();
  };

  const handleOutputStyleChange = async (name: string) => {
    try {
      setRuntimeBusy(true);
      const result = await pluginApi.activateOutputStyle(name || null);
      setActiveOutputStyle(result.active);
      setMessage({
        type: 'success',
        text: result.active ? `Output style '${result.active}' activated` : 'Output style cleared',
      });
    } catch (e: any) {
      setMessage({ type: 'error', text: e.message || 'Failed to activate output style' });
    } finally {
      setRuntimeBusy(false);
    }
  };

  const handleInstall = async () => {
    if (!installSource.trim()) return;
    try {
      setInstalling(true);
      setMessage(null);
      const result = await pluginApi.install({
        source: installSource.trim(),
        scope: installScope,
      });
      if (result.success) {
        setMessage(mutationMessage(result, `Plugin '${result.plugin_id}' installed`));
        setInstallSource('');
        setShowInstall(false);
        await loadPlugins();
      } else {
        setMessage({ type: 'error', text: result.error || 'Install failed' });
      }
    } catch (e: any) {
      setMessage({ type: 'error', text: e.message || 'Install failed' });
    } finally {
      setInstalling(false);
    }
  };

  const handleUninstall = async (name: string) => {
    if (!confirm(`Uninstall plugin '${name}'?`)) return;
    try {
      const result = await pluginApi.uninstall({ name });
      if (result.success) {
        setMessage(mutationMessage(result, `Plugin '${name}' uninstalled`));
        await loadPlugins();
      } else {
        setMessage({ type: 'error', text: result.error || 'Uninstall failed' });
      }
    } catch (e: any) {
      setMessage({ type: 'error', text: e.message || 'Uninstall failed' });
    }
  };

  const handleToggle = async (plugin: PluginInfo) => {
    try {
      const result = plugin.enabled
        ? await pluginApi.disable(plugin.name)
        : await pluginApi.enable(plugin.name);
      if (result.success) {
        setMessage(mutationMessage(result, `Plugin ${plugin.enabled ? 'disabled' : 'enabled'}`));
        await loadPlugins();
      } else {
        setMessage({ type: 'error', text: result.error || 'Toggle failed' });
      }
    } catch (e: any) {
      setMessage({ type: 'error', text: e.message || 'Toggle failed' });
    }
  };

  const handleReload = async () => {
    try {
      setLoading(true);
      const result = await pluginApi.reload();
      if (result.success) {
        setMessage({ type: 'success', text: result.message || 'Plugins reloaded' });
        await loadPlugins();
      } else {
        setMessage({ type: 'error', text: result.error || 'Reload failed' });
      }
    } catch (e: any) {
      setMessage({ type: 'error', text: e.message || 'Reload failed' });
    } finally {
      setLoading(false);
    }
  };

  const handleScaffold = async () => {
    if (!developerPath.trim() || !scaffoldName.trim()) return;
    try {
      setDeveloperBusy(true);
      const result = await pluginApi.scaffold(developerPath.trim(), scaffoldName.trim());
      setMessage(
        result.success
          ? { type: 'success', text: result.message || `Plugin scaffolded at ${result.path}` }
          : { type: 'error', text: result.error || 'Scaffold failed' }
      );
    } catch (e: any) {
      setMessage({ type: 'error', text: e.message || 'Scaffold failed' });
    } finally {
      setDeveloperBusy(false);
    }
  };

  const handleValidate = async () => {
    if (!developerPath.trim()) return;
    try {
      setDeveloperBusy(true);
      const report = await pluginApi.validate(developerPath.trim());
      setMessage(
        report.valid
          ? {
              type: 'success',
              text: `Plugin '${report.name || 'unknown'}' is valid: ${report.components.join(', ')}`,
            }
          : { type: 'error', text: report.errors.join('; ') || 'Validation failed' }
      );
    } catch (e: any) {
      setMessage({ type: 'error', text: e.message || 'Validation failed' });
    } finally {
      setDeveloperBusy(false);
    }
  };

  const filteredPlugins = searchQuery
    ? plugins.filter(
        (p) =>
          p.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
          p.description.toLowerCase().includes(searchQuery.toLowerCase()) ||
          p.keywords.some((k) => k.toLowerCase().includes(searchQuery.toLowerCase()))
      )
    : plugins;

  const enabledCount = plugins.filter((p) => p.enabled).length;

  return (
    <div className="space-y-4">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Package className="w-5 h-5" style={{ color: 'var(--accent)' }} />
          <h2 className="text-lg font-semibold" style={{ color: 'var(--text-primary)' }}>
            Plugins
          </h2>
          <span className="text-xs" style={{ color: 'var(--text-tertiary)' }}>
            {plugins.length} installed, {enabledCount} enabled
          </span>
        </div>
        <div className="flex gap-2">
          <button
            onClick={() => setShowDeveloperTools(!showDeveloperTools)}
            className="p-2 rounded-md transition-colors hover:bg-[var(--bg-hover)]"
            style={{ color: 'var(--text-secondary)' }}
            title="Plugin developer tools"
          >
            <FolderPlus className="w-4 h-4" />
          </button>
          <button
            onClick={handleReload}
            disabled={loading}
            className="p-2 rounded-lg transition-colors disabled:opacity-50 hover:bg-[var(--bg-hover)]"
            style={{ color: 'var(--text-secondary)' }}
            title="Reload plugins"
          >
            <RefreshCw className={`w-4 h-4 ${loading ? 'animate-spin' : ''}`} />
          </button>
          <button
            onClick={() => setShowInstall(!showInstall)}
            className="flex items-center gap-1 px-3 py-2 rounded-lg text-[var(--text-on-accent)] text-sm transition-colors hover:opacity-90"
            style={{ background: 'var(--accent)' }}
          >
            <Plus className="w-4 h-4" />
            Install
          </button>
        </div>
      </div>

      {/* Message */}
      {message && (
        <div
          className="flex items-center gap-2 px-3 py-2 rounded-lg text-sm border"
          style={{
            background:
              message.type === 'success' ? 'rgba(34, 197, 94, 0.1)' : 'rgba(239, 68, 68, 0.1)',
            color:
              message.type === 'success'
                ? 'var(--color-success, #22c55e)'
                : 'var(--color-error, #ef4444)',
            borderColor:
              message.type === 'success' ? 'rgba(34, 197, 94, 0.3)' : 'rgba(239, 68, 68, 0.3)',
          }}
        >
          {message.type === 'success' ? (
            <Check className="w-4 h-4" />
          ) : (
            <AlertCircle className="w-4 h-4" />
          )}
          {message.text}
          <button onClick={() => setMessage(null)} className="ml-auto">
            <X className="w-3 h-3" />
          </button>
        </div>
      )}

      {showDeveloperTools && (
        <div
          className="p-4 border space-y-3"
          style={{ background: 'var(--bg-secondary)', borderColor: 'var(--border-primary)' }}
        >
          <div className="grid grid-cols-1 sm:grid-cols-[1fr_180px] gap-2">
            <input
              type="text"
              placeholder="Plugin directory"
              value={developerPath}
              onChange={(event) => setDeveloperPath(event.target.value)}
              className="w-full px-3 py-2 border rounded-md text-sm focus:outline-none focus:border-[var(--accent)]"
              style={{
                background: 'var(--bg-input)',
                borderColor: 'var(--border-primary)',
                color: 'var(--text-primary)',
              }}
            />
            <input
              type="text"
              placeholder="Plugin name"
              value={scaffoldName}
              onChange={(event) => setScaffoldName(event.target.value)}
              className="w-full px-3 py-2 border rounded-md text-sm focus:outline-none focus:border-[var(--accent)]"
              style={{
                background: 'var(--bg-input)',
                borderColor: 'var(--border-primary)',
                color: 'var(--text-primary)',
              }}
            />
          </div>
          <div className="flex gap-2">
            <button
              onClick={handleScaffold}
              disabled={developerBusy || !developerPath.trim() || !scaffoldName.trim()}
              className="flex items-center gap-1 px-3 py-2 rounded-md text-sm text-[var(--text-on-accent)] disabled:opacity-50"
              style={{ background: 'var(--accent)' }}
            >
              {developerBusy ? (
                <Loader2 className="w-4 h-4 animate-spin" />
              ) : (
                <FolderPlus className="w-4 h-4" />
              )}
              Scaffold
            </button>
            <button
              onClick={handleValidate}
              disabled={developerBusy || !developerPath.trim()}
              className="flex items-center gap-1 px-3 py-2 border rounded-md text-sm disabled:opacity-50"
              style={{ borderColor: 'var(--border-primary)', color: 'var(--text-secondary)' }}
            >
              <ShieldCheck className="w-4 h-4" />
              Validate
            </button>
          </div>
        </div>
      )}

      {(pluginThemes.length > 0 || outputStyles.length > 0) && (
        <div
          className="grid grid-cols-1 gap-4 border-y py-3 sm:grid-cols-2"
          style={{ borderColor: 'var(--border-primary)' }}
        >
          <div className="space-y-2">
            <div
              className="flex items-center gap-2 text-sm font-medium"
              style={{ color: 'var(--text-secondary)' }}
            >
              <Palette className="h-4 w-4" />
              Plugin themes
            </div>
            {pluginThemes.length === 0 ? (
              <div className="text-xs" style={{ color: 'var(--text-tertiary)' }}>
                None loaded
              </div>
            ) : (
              <select
                value={activeTheme || ''}
                onChange={(event) => handleThemeChange(event.target.value)}
                disabled={runtimeBusy}
                className="w-full rounded-md border px-3 py-2 text-sm disabled:opacity-50"
                style={{
                  background: 'var(--bg-input)',
                  borderColor: 'var(--border-primary)',
                  color: 'var(--text-primary)',
                }}
              >
                <option value="">Default</option>
                {pluginThemes.map((theme) => (
                  <option key={`${theme.plugin}:${theme.name}`} value={theme.name}>
                    {theme.display_name || theme.name} ({theme.plugin})
                  </option>
                ))}
              </select>
            )}
          </div>

          <label className="space-y-2">
            <span
              className="flex items-center gap-2 text-sm font-medium"
              style={{ color: 'var(--text-secondary)' }}
            >
              <TextQuote className="h-4 w-4" />
              Output style
            </span>
            <select
              value={activeOutputStyle || ''}
              onChange={(event) => handleOutputStyleChange(event.target.value)}
              disabled={runtimeBusy}
              className="w-full rounded-md border px-3 py-2 text-sm disabled:opacity-50"
              style={{
                background: 'var(--bg-input)',
                borderColor: 'var(--border-primary)',
                color: 'var(--text-primary)',
              }}
            >
              <option value="">Default</option>
              {outputStyles.map((style) => (
                <option key={`${style.plugin}:${style.name}`} value={style.name}>
                  {style.name} ({style.plugin})
                </option>
              ))}
            </select>
            {activeOutputStyle && (
              <div className="text-xs" style={{ color: 'var(--text-tertiary)' }}>
                {outputStyles.find((style) => style.name === activeOutputStyle)?.description}
              </div>
            )}
          </label>
        </div>
      )}

      {/* Install Form */}
      {showInstall && (
        <div
          className="rounded-lg p-4 border space-y-3"
          style={{ background: 'var(--bg-secondary)', borderColor: 'var(--border-primary)' }}
        >
          <h3 className="text-sm font-medium" style={{ color: 'var(--text-secondary)' }}>
            Install Plugin
          </h3>
          <input
            type="text"
            placeholder="Local path or git URL..."
            value={installSource}
            onChange={(e) => setInstallSource(e.target.value)}
            className="w-full px-3 py-2 border rounded-md text-sm focus:outline-none focus:border-[var(--accent)]"
            style={{
              background: 'var(--bg-input)',
              borderColor: 'var(--border-primary)',
              color: 'var(--text-primary)',
            }}
          />
          <div className="flex gap-2 items-center">
            <select
              value={installScope}
              onChange={(e) => setInstallScope(e.target.value)}
              className="px-3 py-2 border rounded-md text-sm focus:outline-none"
              style={{
                background: 'var(--bg-input)',
                borderColor: 'var(--border-primary)',
                color: 'var(--text-primary)',
              }}
            >
              <option value="user">User (global)</option>
              <option value="project">Project (team)</option>
              <option value="local">Local (private)</option>
            </select>
            <button
              onClick={handleInstall}
              disabled={installing || !installSource.trim()}
              className="flex items-center gap-1 px-4 py-2 text-[var(--text-on-accent)] rounded-md text-sm transition-colors disabled:opacity-50 hover:opacity-90"
              style={{ background: 'var(--accent)' }}
            >
              {installing ? (
                <Loader2 className="w-4 h-4 animate-spin" />
              ) : (
                <Plus className="w-4 h-4" />
              )}
              Install
            </button>
            <button
              onClick={() => setShowInstall(false)}
              className="px-3 py-2 text-sm transition-colors hover:opacity-80"
              style={{ color: 'var(--text-tertiary)' }}
            >
              Cancel
            </button>
          </div>
        </div>
      )}

      {/* Search */}
      {plugins.length > 3 && (
        <div className="relative">
          <Search
            className="absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4"
            style={{ color: 'var(--text-tertiary)' }}
          />
          <input
            type="text"
            placeholder="Search plugins..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="w-full pl-10 pr-3 py-2 border rounded-lg text-sm focus:outline-none focus:border-[var(--accent)]"
            style={{
              background: 'var(--bg-secondary)',
              borderColor: 'var(--border-primary)',
              color: 'var(--text-primary)',
            }}
          />
        </div>
      )}

      {/* Plugin List */}
      {loading ? (
        <div className="flex items-center justify-center py-8">
          <Loader2 className="w-6 h-6 animate-spin" style={{ color: 'var(--accent)' }} />
        </div>
      ) : error ? (
        <div
          className="flex items-center gap-2 px-3 py-4 text-sm"
          style={{ color: 'var(--color-error, #ef4444)' }}
        >
          <AlertCircle className="w-4 h-4" />
          {error}
        </div>
      ) : filteredPlugins.length === 0 ? (
        <div className="text-center py-8 text-sm" style={{ color: 'var(--text-tertiary)' }}>
          {plugins.length === 0
            ? 'No plugins installed. Click Install to add one.'
            : 'No plugins match your search.'}
        </div>
      ) : (
        <div className="space-y-2">
          {filteredPlugins.map((plugin) => (
            <PluginCard
              key={plugin.name}
              plugin={plugin}
              expanded={expanded === plugin.name}
              onToggleExpand={() => setExpanded(expanded === plugin.name ? null : plugin.name)}
              onToggle={() => handleToggle(plugin)}
              onUninstall={() => handleUninstall(plugin.name)}
              onConfigure={(values) => handleConfigure(plugin.name, values)}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function PluginCard({
  plugin,
  expanded,
  onToggleExpand,
  onToggle,
  onUninstall,
  onConfigure,
}: {
  plugin: PluginInfo;
  expanded: boolean;
  onToggleExpand: () => void;
  onToggle: () => void;
  onUninstall: () => void;
  onConfigure: (values: Record<string, unknown>) => Promise<void>;
}) {
  return (
    <div
      className="rounded-lg border"
      style={{
        background: 'var(--bg-secondary)',
        borderColor: plugin.enabled ? 'var(--border-primary)' : 'var(--border-secondary)',
        opacity: plugin.enabled ? 1 : 0.6,
      }}
    >
      <div className="flex items-center gap-3 px-4 py-3">
        <button
          onClick={onToggleExpand}
          className="transition-colors hover:text-[var(--text-primary)]"
          style={{ color: 'var(--text-tertiary)' }}
        >
          {expanded ? <ChevronDown className="w-4 h-4" /> : <ChevronRight className="w-4 h-4" />}
        </button>

        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span
              className="text-sm font-medium"
              style={{ color: plugin.enabled ? 'var(--text-primary)' : 'var(--text-tertiary)' }}
            >
              {plugin.display_name || plugin.name}
            </span>
            <span className="text-xs" style={{ color: 'var(--text-tertiary)' }}>
              v{plugin.version}
            </span>
            {plugin.license && (
              <span
                className="text-xs px-1.5 py-0.5 rounded-md"
                style={{ background: 'var(--bg-tertiary)', color: 'var(--text-tertiary)' }}
              >
                {plugin.license}
              </span>
            )}
          </div>
          {plugin.description && (
            <p className="text-xs truncate mt-0.5" style={{ color: 'var(--text-secondary)' }}>
              {plugin.description}
            </p>
          )}
        </div>

        <div className="flex items-center gap-1">
          <button
            onClick={onToggle}
            className="p-1.5 rounded-md transition-colors hover:bg-[var(--bg-hover)]"
            style={{
              color: plugin.enabled ? 'var(--color-success, #22c55e)' : 'var(--text-tertiary)',
            }}
            title={plugin.enabled ? 'Disable' : 'Enable'}
          >
            {plugin.enabled ? <Power className="w-4 h-4" /> : <PowerOff className="w-4 h-4" />}
          </button>
          <button
            onClick={onUninstall}
            className="p-1.5 rounded-md transition-colors hover:bg-[var(--bg-hover)]"
            style={{ color: 'var(--text-tertiary)' }}
            title="Uninstall"
          >
            <Trash2 className="w-4 h-4" />
          </button>
        </div>
      </div>

      {expanded && (
        <div
          className="px-4 pb-3 pt-1 border-t space-y-2"
          style={{ borderColor: 'var(--border-primary)' }}
        >
          {plugin.capabilities.length > 0 && (
            <div className="flex flex-wrap gap-1">
              {plugin.capabilities.map((cap) => (
                <span
                  key={cap}
                  className="text-xs px-2 py-0.5 rounded-md"
                  style={{ background: 'rgba(139, 92, 246, 0.1)', color: 'var(--accent)' }}
                >
                  {cap}
                </span>
              ))}
            </div>
          )}

          <div
            className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs"
            style={{ color: 'var(--text-secondary)' }}
          >
            <div>
              Scope: <span style={{ color: 'var(--text-primary)' }}>{plugin.scope}</span>
            </div>
            {plugin.author && (
              <div>
                Author: <span style={{ color: 'var(--text-primary)' }}>{plugin.author}</span>
              </div>
            )}
          </div>

          {plugin.keywords.length > 0 && (
            <div className="flex flex-wrap gap-1">
              {plugin.keywords.map((kw) => (
                <span
                  key={kw}
                  className="text-xs px-1.5 py-0.5 rounded-md"
                  style={{ background: 'var(--bg-tertiary)', color: 'var(--text-secondary)' }}
                >
                  {kw}
                </span>
              ))}
            </div>
          )}

          {plugin.dependencies.length > 0 && (
            <div className="text-xs" style={{ color: 'var(--text-secondary)' }}>
              Dependencies:{' '}
              {plugin.dependencies
                .map((d) => `${d.name}${d.version ? ` (${d.version})` : ''}`)
                .join(', ')}
            </div>
          )}

          {Object.keys(plugin.config).length > 0 && (
            <PluginConfigForm plugin={plugin} onConfigure={onConfigure} />
          )}

          <div className="text-xs font-mono mt-1" style={{ color: 'var(--text-tertiary)' }}>
            {plugin.path}
          </div>
        </div>
      )}
    </div>
  );
}

function PluginConfigForm({
  plugin,
  onConfigure,
}: {
  plugin: PluginInfo;
  onConfigure: (values: Record<string, unknown>) => Promise<void>;
}) {
  const [values, setValues] = useState<Record<string, unknown>>(plugin.config_values);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => setValues(plugin.config_values), [plugin.config_values]);

  const setValue = (key: string, value: unknown) =>
    setValues((current) => ({ ...current, [key]: value }));

  const save = async () => {
    try {
      setSaving(true);
      setError(null);
      await onConfigure(values);
    } catch (reason: any) {
      setError(reason.message || 'Configuration failed');
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="space-y-2 border-t pt-2" style={{ borderColor: 'var(--border-primary)' }}>
      {Object.entries(plugin.config).map(([key, field]) => {
        const value = values[key] ?? field.default ?? '';
        return (
          <label key={key} className="block space-y-1 text-xs">
            <span style={{ color: 'var(--text-secondary)' }}>
              {field.title || key}
              {field.required ? ' *' : ''}
            </span>
            {field.type === 'boolean' ? (
              <input
                type="checkbox"
                checked={Boolean(value)}
                onChange={(event) => setValue(key, event.target.checked)}
              />
            ) : (
              <input
                type={field.sensitive ? 'password' : field.type === 'number' ? 'number' : 'text'}
                value={Array.isArray(value) ? value.join(', ') : String(value)}
                min={field.min ?? undefined}
                max={field.max ?? undefined}
                onChange={(event) => {
                  const raw = event.target.value;
                  setValue(
                    key,
                    field.multiple
                      ? raw
                          .split(',')
                          .map((item) => item.trim())
                          .filter(Boolean)
                      : field.type === 'number'
                        ? raw === ''
                          ? null
                          : Number(raw)
                        : raw
                  );
                }}
                className="w-full rounded-md border px-2 py-1.5"
                style={{
                  background: 'var(--bg-input)',
                  borderColor: 'var(--border-primary)',
                  color: 'var(--text-primary)',
                }}
              />
            )}
            {field.description && (
              <span className="block" style={{ color: 'var(--text-tertiary)' }}>
                {field.description}
              </span>
            )}
          </label>
        );
      })}
      {error && <div style={{ color: 'var(--color-error)' }}>{error}</div>}
      <button
        onClick={save}
        disabled={saving}
        className="flex items-center gap-1 rounded-md border px-2 py-1.5 text-xs disabled:opacity-50"
        style={{ borderColor: 'var(--border-primary)', color: 'var(--text-secondary)' }}
      >
        {saving ? (
          <Loader2 className="h-3.5 w-3.5 animate-spin" />
        ) : (
          <Save className="h-3.5 w-3.5" />
        )}
        Save
      </button>
    </div>
  );
}
