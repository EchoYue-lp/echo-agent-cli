import { useEffect, useState } from 'react';
import { providerApi } from '../../api/endpoints';
import type { ProviderInfo } from '../../types/api';

// 自定义供应商模板
const CUSTOM_PROVIDER: ProviderInfo = {
  id: 'custom',
  name: '自定义',
  icon: '⚙️',
  models: [],
  base_url: '',
  api_key_env: '',
  requires_api_key: true,
  configured: false,
};

export function ProviderPanel() {
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [currentModel, setCurrentModel] = useState<string>('');
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [apiKey, setApiKey] = useState('');
  const [baseUrl, setBaseUrl] = useState('');
  const [selectedModel, setSelectedModel] = useState('');
  const [customModel, setCustomModel] = useState('');
  const [temperature, setTemperature] = useState('');
  const [maxTokens, setMaxTokens] = useState('');
  const [loading, setLoading] = useState(true);
  const [testing, setTesting] = useState(false);
  const [switching, setSwitching] = useState(false);
  const [testResult, setTestResult] = useState<{ success: boolean; message: string } | null>(null);
  const [switchResult, setSwitchResult] = useState<{ success: boolean; message: string } | null>(null);

  useEffect(() => {
    providerApi.list().then((res) => {
      // 在列表末尾追加自定义供应商
      const allProviders = [...res.providers, CUSTOM_PROVIDER];
      setProviders(allProviders);
      setCurrentModel(res.current_model);

      // Auto-select provider matching current model
      const match = allProviders.find((p) =>
        p.models.some((m) => m === res.current_model)
      );
      if (match) {
        setSelectedId(match.id);
        setSelectedModel(res.current_model);
        setBaseUrl(match.base_url);
      }
      setLoading(false);
    }).catch((e) => {
      console.error(e);
      setLoading(false);
    });
  }, []);

  const selected = providers.find((p) => p.id === selectedId) ?? null;
  const isCustom = selectedId === 'custom';

  const handleSelectProvider = (p: ProviderInfo) => {
    setSelectedId(p.id);
    setSelectedModel(p.models[0] ?? '');
    setCustomModel('');
    setBaseUrl(p.base_url);
    setApiKey('');
    setTemperature('');
    setMaxTokens('');
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
        base_url: (baseUrl && baseUrl !== selected.base_url) ? baseUrl : undefined,
      });
      setTestResult({
        success: res.success,
        message: res.success
          ? `连接成功！模型: ${res.model}, 响应: ${res.response?.slice(0, 100)}`
          : `连接失败: ${res.error}`,
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
      const hasCustomCredentials = apiKey.trim().length > 0;
      const res = await providerApi.switch({
        model,
        provider: selected.id,
        api_key: hasCustomCredentials ? apiKey : undefined,
        base_url: hasCustomCredentials ? baseUrl : undefined,
        temperature: temperature ? Number(temperature) : undefined,
        max_tokens: maxTokens ? Number(maxTokens) : undefined,
      });
      setSwitchResult({
        success: res.success,
        message: res.message,
      });
      if (res.success) {
        setCurrentModel(res.model);
      }
    } catch (e: any) {
      setSwitchResult({ success: false, message: `切换失败: ${e.message}` });
    } finally {
      setSwitching(false);
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
            当前模型: <span className="font-medium text-[var(--accent)]">{currentModel || '未设置'}</span>
          </p>
        </div>
      </div>

      {/* Provider grid */}
      <div className="grid grid-cols-2 gap-2">
        {providers.map((p) => (
          <button
            key={p.id}
            onClick={() => handleSelectProvider(p)}
            className={`flex items-center gap-3 rounded-lg border p-3 text-left transition-all
              ${selectedId === p.id
                ? 'border-[var(--accent)] bg-[var(--accent)]/5 shadow-sm'
                : 'border-[var(--border-primary)] bg-[var(--bg-primary)] hover:border-[var(--border-focus)] hover:bg-[var(--bg-hover)]'
              }`}
          >
            <span className="text-xl">{p.icon}</span>
            <div className="min-w-0 flex-1">
              <div className="truncate text-sm font-medium text-[var(--text-primary)]">{p.name}</div>
              {p.id !== 'custom' && (
                <div className="flex items-center gap-1.5 mt-0.5">
                  <span
                    className={`inline-block h-1.5 w-1.5 rounded-full ${
                      p.configured ? 'bg-emerald-500' : 'bg-[var(--text-tertiary)]'
                    }`}
                  />
                  <span className="text-[10px] text-[var(--text-tertiary)]">
                    {p.configured ? '已配置' : '未配置'}
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
            <span className="text-lg">{selected.icon}</span>
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
                    : selected.configured
                      ? '已配置 (留空使用环境变量)'
                      : '输入 API Key'
                }
                className="w-full rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] px-3 py-2 text-sm text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)] focus:border-[var(--border-focus)]"
              />
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
            {isCustom || selected.models.length === 0 ? (
              <input
                type="text"
                value={isCustom ? (customModel || selectedModel) : customModel}
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
                    {selected.models.map((m) => (
                      <option key={m} value={m}>{m}</option>
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
                最大上下文
                <span className="ml-1 text-[var(--text-tertiary)]">(可选)</span>
              </label>
              <input
                type="number"
                min="1"
                value={maxTokens}
                onChange={(e) => setMaxTokens(e.target.value)}
                placeholder="默认"
                className="w-full rounded-lg border border-[var(--border-primary)] bg-[var(--bg-input)] px-3 py-2 text-sm text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)] focus:border-[var(--border-focus)]"
              />
            </div>
          </div>

          {/* Action buttons */}
          <div className="flex items-center gap-2 pt-1">
            <button
              onClick={handleTest}
              disabled={testing || (isCustom && (!apiKey.trim() || !baseUrl.trim() || !(customModel.trim() || selectedModel.trim())))}
              className="rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] px-4 py-1.5 text-xs font-medium text-[var(--text-primary)] transition-colors hover:bg-[var(--bg-hover)] disabled:cursor-not-allowed disabled:opacity-40"
            >
              {testing ? '测试中...' : '测试连接'}
            </button>
            <button
              onClick={handleSwitch}
              disabled={switching || (isCustom && (!apiKey.trim() || !baseUrl.trim() || !(customModel.trim() || selectedModel.trim())))}
              className="rounded-lg bg-[var(--accent)] px-4 py-1.5 text-xs font-medium text-white transition-colors hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-40"
            >
              {switching ? '切换中...' : '切换模型'}
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
