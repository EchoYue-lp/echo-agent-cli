import { useCallback, useEffect, useState } from 'react';
import { providerApi } from '../../api/endpoints';
import type { ConfiguredModel, ProviderTemplate } from '../../types/api';

const MODELS_CHANGED_EVENT = 'echocowork:models-changed';

function notifyModelsChanged() {
  window.dispatchEvent(new Event(MODELS_CHANGED_EVENT));
}

export function ProviderPanel() {
  const [providers, setProviders] = useState<ProviderTemplate[]>([]);
  const [configuredModels, setConfiguredModels] = useState<ConfiguredModel[]>([]);
  const [currentModel, setCurrentModel] = useState<string>('');
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [apiKey, setApiKey] = useState('');
  const [baseUrl, setBaseUrl] = useState('');
  const [selectedModel, setSelectedModel] = useState('');
  const [customModel, setCustomModel] = useState('');
  const [temperature, setTemperature] = useState('');
  const [maxTokens, setMaxTokens] = useState('');
  const [contextWindow, setContextWindow] = useState('');
  const [contextWindowPreset, setContextWindowPreset] = useState('auto');
  const [loading, setLoading] = useState(true);
  const [testing, setTesting] = useState(false);
  const [switching, setSwitching] = useState(false);
  const [testResult, setTestResult] = useState<{ success: boolean; message: string } | null>(null);
  const [switchResult, setSwitchResult] = useState<{ success: boolean; message: string } | null>(
    null
  );
  const [modelActionId, setModelActionId] = useState<string | null>(null);

  const loadConfiguredModels = useCallback(async () => {
    const res = await providerApi.listConfigured();
    setConfiguredModels(res.models);
    const active = res.models.find((model) => model.is_default);
    setCurrentModel(active?.display_name || active?.model || '');
  }, []);

  useEffect(() => {
    Promise.all([providerApi.listTemplates(), providerApi.listConfigured()])
      .then(([templateRes, modelRes]) => {
        setProviders(templateRes.providers);
        setConfiguredModels(modelRes.models);
        const active = modelRes.models.find((model) => model.is_default);
        setCurrentModel(active?.display_name || active?.model || '');
        // Backfill context_window from the active configured model so that
        // re-saving (e.g. after changing temperature) does not silently
        // overwrite the stored value with null.
        if (active?.context_window != null && active.context_window > 0) {
          setContextWindow(String(active.context_window));
        }
        const firstProvider = templateRes.providers[0];
        if (firstProvider) {
          setSelectedId(firstProvider.id);
          setSelectedModel((firstProvider.default_models ?? [])[0] ?? '');
          setBaseUrl(firstProvider.base_url);
        }
        setLoading(false);
      })
      .catch((e) => {
        console.error(e);
        setLoading(false);
      });
  }, []);

  const selected = providers.find((p) => p.id === selectedId) ?? null;
  const isCustom = selectedId === 'custom';
  const providerHasModels = (providerId: string) =>
    configuredModels.some((model) => model.provider === providerId);
  const authSourceLabel = (source?: string) => {
    switch (source) {
      case 'input':
        return '本次输入的 API Key';
      case 'config':
        return '已保存的 API Key';
      case 'env':
        return '环境变量 API Key';
      case 'none':
        return '未找到 API Key';
      default:
        return source || '未知来源';
    }
  };

  const handleSelectProvider = (p: ProviderTemplate) => {
    setSelectedId(p.id);
    setSelectedModel((p.default_models ?? [])[0] ?? '');
    setCustomModel('');
    setBaseUrl(p.base_url);
    setApiKey('');
    setTemperature('');
    setMaxTokens('');
    setContextWindow('');
    setContextWindowPreset('auto');
    setTestResult(null);
    setSwitchResult(null);
  };

  const handleTest = async () => {
    if (!selected) return;
    setTesting(true);
    setTestResult(null);
    try {
      const model = customModel.trim() || selectedModel;
      const res = await providerApi.test({
        provider: selected.id,
        model,
        api_key: apiKey || undefined,
        base_url: baseUrl && baseUrl !== selected.base_url ? baseUrl : undefined,
      });
      setTestResult({
        success: res.success,
        message: res.success
          ? `连接成功！模型: ${res.model}，使用: ${authSourceLabel(res.auth_source)}，响应: ${res.response?.slice(0, 100)}`
          : `连接失败: ${res.error}（使用: ${authSourceLabel(res.auth_source)}）`,
      });
    } catch (e: any) {
      setTestResult({ success: false, message: `请求失败: ${e.message}` });
    } finally {
      setTesting(false);
    }
  };

  const handleSwitch = async () => {
    if (!selected) return;
    setSwitching(true);
    setSwitchResult(null);
    try {
      const model = customModel.trim() || selectedModel;
      const trimmedApiKey = apiKey.trim();
      const trimmedBaseUrl = baseUrl.trim();
      const hasCustomApiKey = trimmedApiKey.length > 0;
      const hasCustomBaseUrl = trimmedBaseUrl.length > 0 && trimmedBaseUrl !== selected.base_url;
      const res = await providerApi.upsertConfigured({
        model,
        provider: selected.id,
        api_key: hasCustomApiKey ? trimmedApiKey : undefined,
        base_url: hasCustomBaseUrl || isCustom ? trimmedBaseUrl : undefined,
        temperature: temperature ? Number(temperature) : undefined,
        max_tokens: maxTokens ? Number(maxTokens) : undefined,
        context_window: contextWindow ? Number(contextWindow) : undefined,
        set_default: true,
      });
      setSwitchResult({
        success: res.success,
        message: `已保存并设为默认模型（使用: ${authSourceLabel(res.auth_source)}）`,
      });
      if (res.success) {
        await loadConfiguredModels();
        notifyModelsChanged();
      }
    } catch (e: any) {
      setSwitchResult({ success: false, message: `切换失败: ${e.message}` });
    } finally {
      setSwitching(false);
    }
  };

  const handleSetDefault = async (model: ConfiguredModel) => {
    setModelActionId(model.id);
    try {
      const res = await providerApi.setDefault(model.id);
      setCurrentModel(res.display_name || res.model);
      await loadConfiguredModels();
      notifyModelsChanged();
    } finally {
      setModelActionId(null);
    }
  };

  const handleDeleteModel = async (model: ConfiguredModel) => {
    setModelActionId(model.id);
    try {
      await providerApi.deleteConfigured(model.id);
      await loadConfiguredModels();
      notifyModelsChanged();
    } finally {
      setModelActionId(null);
    }
  };

  if (loading) return <div className="p-3 text-sm text-[var(--text-tertiary)]">加载中...</div>;

  return (
    <div className="space-y-4 p-3">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h3 className="text-sm font-semibold text-[var(--text-primary)]">模型供应商</h3>
          <p className="mt-0.5 text-xs text-[var(--text-tertiary)]">
            当前模型:{' '}
            <span className="font-medium text-[var(--accent)]">{currentModel || '未设置'}</span>
          </p>
        </div>
      </div>

      <div className="rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)]">
        <div className="flex items-center justify-between border-b border-[var(--border-secondary)] px-3 py-2">
          <h4 className="text-xs font-semibold text-[var(--text-primary)]">已添加模型</h4>
          <span className="text-[10px] text-[var(--text-tertiary)]">
            {configuredModels.length} 个
          </span>
        </div>
        {configuredModels.length === 0 ? (
          <div className="px-3 py-4 text-xs text-[var(--text-tertiary)]">
            还没有添加模型。选择下方厂商并保存后，会出现在输入框的模型切换里。
          </div>
        ) : (
          <div className="divide-y divide-[var(--border-secondary)]">
            {configuredModels.map((model) => (
              <div key={model.id} className="flex items-center gap-3 px-3 py-2">
                <div className="min-w-0 flex-1">
                  <div className="truncate text-sm font-medium text-[var(--text-primary)]">
                    {model.display_name || model.model}
                  </div>
                  <div className="truncate text-[10px] text-[var(--text-tertiary)]">
                    {model.provider}
                    {model.is_default && <span className="ml-2 text-[var(--accent)]">默认</span>}
                  </div>
                </div>
                {!model.is_default && (
                  <button
                    onClick={() => handleSetDefault(model)}
                    disabled={modelActionId === model.id}
                    className="rounded-md border border-[var(--border-primary)] px-2 py-1 text-[11px] text-[var(--text-secondary)] transition-colors hover:bg-[var(--bg-hover)] disabled:opacity-40"
                  >
                    设为默认
                  </button>
                )}
                <button
                  onClick={() => handleDeleteModel(model)}
                  disabled={modelActionId === model.id}
                  className="rounded-md px-2 py-1 text-[11px] text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] disabled:opacity-40"
                >
                  删除
                </button>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Provider grid */}
      <div className="grid grid-cols-2 gap-2">
        {providers.map((p) => (
          <button
            key={p.id}
            onClick={() => handleSelectProvider(p)}
            className={`flex items-center gap-3 rounded-lg border p-3 text-left transition-all
              ${
                selectedId === p.id
                  ? 'border-[var(--accent)] bg-[var(--accent)]/5 shadow-sm'
                  : 'border-[var(--border-primary)] bg-[var(--bg-primary)] hover:border-[var(--border-focus)] hover:bg-[var(--bg-hover)]'
              }`}
          >
            <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-[var(--bg-secondary)] text-xs font-semibold text-[var(--text-secondary)]">
              {p.name.slice(0, 1).toUpperCase()}
            </span>
            <div className="min-w-0 flex-1">
              <div className="truncate text-sm font-medium text-[var(--text-primary)]">
                {p.name}
              </div>
              {p.id !== 'custom' && (
                <div className="flex items-center gap-1.5 mt-0.5">
                  <span
                    className={`inline-block h-1.5 w-1.5 rounded-full ${
                      providerHasModels(p.id) ? 'bg-emerald-500' : 'bg-[var(--text-tertiary)]'
                    }`}
                  />
                  <span className="text-[10px] text-[var(--text-tertiary)]">
                    {providerHasModels(p.id) ? '已配置' : '未配置'}
                  </span>
                </div>
              )}
              {p.id === 'custom' && (
                <div className="text-[10px] text-[var(--text-tertiary)] mt-0.5">自定义端点</div>
              )}
            </div>
          </button>
        ))}
      </div>

      {/* Selected provider config */}
      {selected && (
        <div className="rounded-lg border border-[var(--border-primary)] p-4 space-y-3">
          <div className="flex items-center gap-2">
            <span className="flex h-7 w-7 items-center justify-center rounded-md bg-[var(--bg-secondary)] text-xs font-semibold text-[var(--text-secondary)]">
              {selected.name.slice(0, 1).toUpperCase()}
            </span>
            <h4 className="text-sm font-semibold text-[var(--text-primary)]">{selected.name}</h4>
          </div>

          {/* API Key */}
          {(selected.requires_api_key || isCustom) && (
            <div>
              <label className="mb-1 block text-xs text-[var(--text-secondary)]">
                API Key
                {!isCustom && selected.api_key_env && (
                  <span className="ml-1 text-[var(--text-tertiary)]">
                    (环境变量: {selected.api_key_env})
                  </span>
                )}
              </label>
              <input
                type="password"
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder={
                  isCustom
                    ? '输入 API Key'
                    : providerHasModels(selected.id)
                      ? '已配置 (留空使用已保存配置或环境变量)'
                      : '输入 API Key'
                }
                className="w-full rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] px-3 py-2 text-sm text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)] focus:border-[var(--border-focus)]"
              />
              <p className="mt-1 text-[10px] text-[var(--text-tertiary)]">
                留空时会使用已保存配置，其次使用环境变量。重新输入并保存会覆盖已保存的 API Key。
              </p>
            </div>
          )}

          {/* Base URL */}
          <div>
            <label className="mb-1 block text-xs text-[var(--text-secondary)]">
              API 地址
              {!isCustom && (
                <span className="ml-1 text-[var(--text-tertiary)]">(可选，覆盖默认)</span>
              )}
            </label>
            <input
              type="text"
              value={baseUrl}
              onChange={(e) => setBaseUrl(e.target.value)}
              placeholder={isCustom ? 'https://api.example.com/v1' : selected.base_url}
              className="w-full rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] px-3 py-2 text-sm text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)] focus:border-[var(--border-focus)]"
            />
          </div>

          {/* Model selection */}
          <div>
            <label className="mb-1 block text-xs text-[var(--text-secondary)]">模型</label>
            {isCustom || (selected.default_models ?? []).length === 0 ? (
              <input
                type="text"
                value={isCustom ? customModel || selectedModel : customModel}
                onChange={(e) => {
                  if (isCustom) {
                    setSelectedModel(e.target.value);
                    setCustomModel('');
                  } else {
                    setCustomModel(e.target.value);
                  }
                }}
                placeholder="输入模型名称"
                className="w-full rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] px-3 py-2 text-sm text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)] focus:border-[var(--border-focus)]"
              />
            ) : (
              <>
                <div className="flex gap-2">
                  <select
                    value={customModel.trim() ? '__custom__' : selectedModel}
                    onChange={(e) => {
                      if (e.target.value === '__custom__') {
                        setCustomModel(selectedModel);
                      } else {
                        setSelectedModel(e.target.value);
                        setCustomModel('');
                      }
                    }}
                    className="flex-1 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] px-2 py-2 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--border-focus)]"
                  >
                    {(selected.default_models ?? []).map((m) => (
                      <option key={m} value={m}>
                        {m}
                      </option>
                    ))}
                    <option value="__custom__">自定义模型...</option>
                  </select>
                </div>
                {customModel.trim() && (
                  <input
                    type="text"
                    value={customModel}
                    onChange={(e) => setCustomModel(e.target.value)}
                    placeholder="输入自定义模型名称"
                    className="mt-2 w-full rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] px-3 py-2 text-sm text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)] focus:border-[var(--border-focus)]"
                  />
                )}
              </>
            )}
          </div>

          {/* Temperature & Max Tokens */}
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="mb-1 block text-xs text-[var(--text-secondary)]">
                温度
                <span className="ml-1 text-[var(--text-tertiary)]">(可选)</span>
              </label>
              <input
                type="number"
                step="0.1"
                min="0"
                max="2"
                value={temperature}
                onChange={(e) => setTemperature(e.target.value)}
                placeholder="默认"
                className="w-full rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] px-3 py-2 text-sm text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)] focus:border-[var(--border-focus)]"
              />
            </div>
            <div>
              <label className="mb-1 block text-xs text-[var(--text-secondary)]">
                最大输出 Token
                <span className="ml-1 text-[var(--text-tertiary)]">(可选)</span>
              </label>
              <input
                type="number"
                min="1"
                value={maxTokens}
                onChange={(e) => setMaxTokens(e.target.value)}
                placeholder="默认（由模型决定）"
                className="w-full rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] px-3 py-2 text-sm text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)] focus:border-[var(--border-focus)]"
              />
            </div>
          </div>

          {/* Context Window */}
          <div>
            <label className="mb-1 block text-xs text-[var(--text-secondary)]">
              模型上下文窗口
              <span className="ml-1 text-[var(--text-tertiary)]">(可选，留空自动推断)</span>
            </label>
            <div className="flex gap-2">
              <select
                value={contextWindowPreset}
                onChange={(e) => {
                  const val = e.target.value;
                  setContextWindowPreset(val);
                  if (val === 'auto') {
                    setContextWindow('');
                  } else if (val !== 'custom') {
                    setContextWindow(val);
                  }
                }}
                className="rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] px-2 py-2 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--border-focus)]"
              >
                <option value="auto">自动</option>
                <option value="4096">4K</option>
                <option value="8192">8K</option>
                <option value="16384">16K</option>
                <option value="32768">32K</option>
                <option value="65536">64K</option>
                <option value="131072">128K</option>
                <option value="200000">200K</option>
                <option value="500000">500K</option>
                <option value="1000000">1M</option>
                <option value="2000000">2M</option>
                <option value="custom">自定义</option>
              </select>
              {contextWindowPreset === 'custom' && (
                <input
                  type="number"
                  min="1"
                  step="1000"
                  value={contextWindow}
                  onChange={(e) => setContextWindow(e.target.value)}
                  placeholder="输入 token 数"
                  className="flex-1 rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] px-3 py-2 text-sm text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)] focus:border-[var(--border-focus)]"
                />
              )}
            </div>
            <p className="mt-1 text-[10px] text-[var(--text-tertiary)]">
              用于压缩触发、TokenBudget 分配和自适应压缩调优。自动模式下根据模型名称推断。
            </p>
          </div>

          {/* Action buttons */}
          <div className="flex items-center gap-2 pt-1">
            <button
              onClick={handleTest}
              disabled={
                testing ||
                (isCustom &&
                  (!apiKey.trim() ||
                    !baseUrl.trim() ||
                    !(customModel.trim() || selectedModel.trim())))
              }
              className="rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] px-4 py-1.5 text-xs font-medium text-[var(--text-primary)] transition-colors hover:bg-[var(--bg-hover)] disabled:cursor-not-allowed disabled:opacity-40"
            >
              {testing ? '测试中...' : '测试连接'}
            </button>
            <button
              onClick={handleSwitch}
              disabled={
                switching ||
                (isCustom &&
                  (!apiKey.trim() ||
                    !baseUrl.trim() ||
                    !(customModel.trim() || selectedModel.trim())))
              }
              className="rounded-lg bg-[var(--accent)] px-4 py-1.5 text-xs font-medium text-[var(--text-on-accent)] transition-colors hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-40"
            >
              {switching ? '保存中...' : '保存并使用'}
            </button>
          </div>

          {/* Results */}
          {testResult && (
            <div
              className={`rounded-lg px-3 py-2 text-xs ${
                testResult.success
                  ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400'
                  : 'bg-red-500/10 text-red-600 dark:text-red-400'
              }`}
            >
              {testResult.message}
            </div>
          )}
          {switchResult && (
            <div
              className={`rounded-lg px-3 py-2 text-xs ${
                switchResult.success
                  ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400'
                  : 'bg-red-500/10 text-red-600 dark:text-red-400'
              }`}
            >
              {switchResult.message}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
