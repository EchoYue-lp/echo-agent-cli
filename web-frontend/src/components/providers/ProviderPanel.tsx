import { useCallback, useEffect, useMemo, useState } from 'react';
import { Check, Plus, Save, Server, Trash2, Zap } from 'lucide-react';
import { providerApi } from '../../api/endpoints';
import { errorMessage } from '../../lib/tauri-bridge';
import type {
  ConfiguredModel,
  LlmApiProtocol,
  ModelInputModality,
  ModelProviderView,
} from '../../generated';
import { thinkingLevelOptions } from '../chat/thinkingLevels';

const MODELS_CHANGED_EVENT = 'eko:models-changed';
const PROTOCOLS: ReadonlyArray<[LlmApiProtocol, string]> = [
  ['chat_completions', 'Chat Completions'],
  ['responses', 'Responses'],
  ['anthropic', 'Anthropic'],
];

interface ProviderDraft {
  id: string;
  name: string;
  baseUrl: string;
  apiKey: string;
  apiKeyEnv: string;
  requiresApiKey: boolean;
  protocol: LlmApiProtocol;
}

interface ModelDraft {
  id?: string;
  displayName: string;
  model: string;
  protocol: LlmApiProtocol;
  imageInput: boolean;
  audioInput: boolean;
  videoInput: boolean;
  temperature: string;
  maxTokens: string;
  contextWindow: string;
}

type Notice = { success: boolean; message: string } | null;

const emptyProvider = (): ProviderDraft => ({
  id: '',
  name: '',
  baseUrl: '',
  apiKey: '',
  apiKeyEnv: '',
  requiresApiKey: false,
  protocol: 'chat_completions',
});

const providerDraft = (provider: ModelProviderView): ProviderDraft => ({
  id: provider.id,
  name: provider.name,
  baseUrl: provider.base_url,
  apiKey: '',
  apiKeyEnv: provider.api_key_env ?? '',
  requiresApiKey: provider.requires_api_key,
  protocol: provider.default_api_protocol,
});

const emptyModel = (protocol: LlmApiProtocol = 'chat_completions'): ModelDraft => ({
  displayName: '',
  model: '',
  protocol,
  imageInput: false,
  audioInput: false,
  videoInput: false,
  temperature: '',
  maxTokens: '',
  contextWindow: '',
});

const modelDraft = (model: ConfiguredModel): ModelDraft => ({
  id: model.id,
  displayName: model.display_name,
  model: model.model,
  protocol: model.api_protocol,
  imageInput: model.input_modalities.includes('image'),
  audioInput: model.input_modalities.includes('audio'),
  videoInput: model.input_modalities.includes('video'),
  temperature: model.temperature == null ? '' : String(model.temperature),
  maxTokens: model.max_tokens == null ? '' : String(model.max_tokens),
  contextWindow: model.context_window == null ? '' : String(model.context_window),
});

function ProtocolControl({
  value,
  onChange,
}: {
  value: LlmApiProtocol;
  onChange: (protocol: LlmApiProtocol) => void;
}) {
  return (
    <div className="grid grid-cols-3 rounded-md border border-[var(--border-primary)] bg-[var(--bg-input)] p-0.5">
      {PROTOCOLS.map(([protocol, label]) => (
        <button
          key={protocol}
          type="button"
          aria-pressed={value === protocol}
          onClick={() => onChange(protocol)}
          className={`min-h-8 min-w-0 rounded px-2 text-xs transition-colors ${
            value === protocol
              ? 'bg-[var(--accent)] text-white'
              : 'text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]'
          }`}
        >
          {label}
        </button>
      ))}
    </div>
  );
}

export function ProviderPanel() {
  const [providers, setProviders] = useState<ModelProviderView[]>([]);
  const [models, setModels] = useState<ConfiguredModel[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [providerForm, setProviderForm] = useState<ProviderDraft>(emptyProvider);
  const [modelForm, setModelForm] = useState<ModelDraft>(emptyModel);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice>(null);

  const reload = useCallback(
    async (preferredId?: string) => {
      const [providerResult, modelResult] = await Promise.all([
        providerApi.listProviders(),
        providerApi.listConfigured(),
      ]);
      setProviders(providerResult.providers);
      setModels(modelResult.models);
      const nextId =
        preferredId ??
        providerResult.providers.find((provider) => provider.id === selectedId)?.id ??
        providerResult.providers[0]?.id ??
        null;
      setSelectedId(nextId);
      const next = providerResult.providers.find((provider) => provider.id === nextId);
      if (next) setProviderForm(providerDraft(next));
    },
    [selectedId]
  );

  useEffect(() => {
    void reload()
      .catch((error: unknown) => setNotice({ success: false, message: errorMessage(error) }))
      .finally(() => setLoading(false));
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const selectedProvider = providers.find((provider) => provider.id === selectedId) ?? null;
  const selectedModels = useMemo(
    () => models.filter((model) => model.provider === selectedId),
    [models, selectedId]
  );
  const fieldClass =
    'mt-1 min-h-9 w-full rounded-md border border-[var(--border-primary)] bg-[var(--bg-input)] px-3 text-sm text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)] focus:border-[var(--border-focus)]';

  const selectProvider = (provider: ModelProviderView) => {
    setSelectedId(provider.id);
    setProviderForm(providerDraft(provider));
    setModelForm(emptyModel(provider.default_api_protocol));
    setNotice(null);
  };

  const modalities = (): ModelInputModality[] => [
    'text',
    ...(modelForm.imageInput ? (['image'] as ModelInputModality[]) : []),
    ...(modelForm.audioInput ? (['audio'] as ModelInputModality[]) : []),
    ...(modelForm.videoInput ? (['video'] as ModelInputModality[]) : []),
  ];

  const saveProvider = async () => {
    setBusy('provider');
    setNotice(null);
    try {
      const result = await providerApi.upsertProvider({
        id: providerForm.id.trim(),
        name: providerForm.name.trim(),
        base_url: providerForm.baseUrl.trim(),
        api_key: providerForm.apiKey.trim() || undefined,
        api_key_env: providerForm.apiKeyEnv.trim() || undefined,
        requires_api_key: providerForm.requiresApiKey,
        default_api_protocol: providerForm.protocol,
      });
      await reload(result.provider_id);
      setNotice({ success: true, message: `Provider ${result.provider_id} 已保存` });
    } catch (error: unknown) {
      setNotice({ success: false, message: `保存失败: ${errorMessage(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const removeProvider = async () => {
    if (!selectedProvider) return;
    setBusy(selectedProvider.id);
    try {
      await providerApi.deleteProvider(selectedProvider.id);
      setSelectedId(null);
      setProviderForm(emptyProvider());
      await reload();
      setNotice({ success: true, message: `${selectedProvider.name} 已删除` });
    } catch (error: unknown) {
      setNotice({ success: false, message: `删除失败: ${errorMessage(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const saveModel = async (setDefault: boolean) => {
    if (!selectedProvider) return;
    setBusy('model');
    setNotice(null);
    try {
      await providerApi.upsertConfigured({
        id: modelForm.id,
        display_name: modelForm.displayName.trim() || undefined,
        provider: selectedProvider.id,
        model: modelForm.model.trim(),
        api_protocol: modelForm.protocol,
        input_modalities: modalities(),
        temperature: modelForm.temperature ? Number(modelForm.temperature) : undefined,
        max_tokens: modelForm.maxTokens ? Number(modelForm.maxTokens) : undefined,
        context_window: modelForm.contextWindow ? Number(modelForm.contextWindow) : undefined,
        set_default: setDefault,
      });
      await reload(selectedProvider.id);
      setModelForm(emptyModel(selectedProvider.default_api_protocol));
      window.dispatchEvent(new Event(MODELS_CHANGED_EVENT));
      setNotice({ success: true, message: setDefault ? '模型已保存并启用' : '模型已保存' });
    } catch (error: unknown) {
      setNotice({ success: false, message: `保存失败: ${errorMessage(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const testConnection = async () => {
    setBusy('test');
    setNotice(null);
    try {
      const result = await providerApi.test({
        provider: providerForm.id.trim(),
        model: modelForm.model.trim(),
        api_protocol: modelForm.protocol,
        input_modalities: modalities(),
        api_key: providerForm.apiKey.trim() || undefined,
        base_url: providerForm.baseUrl.trim(),
      });
      setNotice({
        success: result.success,
        message: result.success
          ? `连接成功: ${result.model}`
          : `连接失败: ${result.error ?? '未知错误'}`,
      });
    } catch (error: unknown) {
      setNotice({ success: false, message: `连接失败: ${errorMessage(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const setDefaultModel = async (model: ConfiguredModel) => {
    setBusy(model.id);
    try {
      await providerApi.setDefault(model.id);
      await reload(model.provider);
      window.dispatchEvent(new Event(MODELS_CHANGED_EVENT));
      setNotice({ success: true, message: `${model.display_name || model.model} 已启用` });
    } catch (error: unknown) {
      setNotice({ success: false, message: `切换失败: ${errorMessage(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const removeModel = async (model: ConfiguredModel) => {
    setBusy(model.id);
    try {
      await providerApi.deleteConfigured(model.id);
      await reload(model.provider);
      window.dispatchEvent(new Event(MODELS_CHANGED_EVENT));
      setNotice({ success: true, message: `${model.display_name || model.model} 已删除` });
    } catch (error: unknown) {
      setNotice({ success: false, message: `删除失败: ${errorMessage(error)}` });
    } finally {
      setBusy(null);
    }
  };

  if (loading) return <div className="p-4 text-sm text-[var(--text-tertiary)]">加载中...</div>;

  return (
    <div className="p-3">
      <div className="mb-3 flex items-center justify-between">
        <div>
          <h3 className="text-sm font-semibold text-[var(--text-primary)]">模型 Provider</h3>
          <p className="text-xs text-[var(--text-tertiary)]">
            {providers.length} providers · {models.length} models
          </p>
        </div>
        <button
          type="button"
          onClick={() => {
            setSelectedId(null);
            setProviderForm(emptyProvider());
            setModelForm(emptyModel());
          }}
          className="flex min-h-8 items-center gap-1.5 rounded-md bg-[var(--accent)] px-3 text-xs font-medium text-white"
        >
          <Plus size={14} /> 添加 Provider
        </button>
      </div>

      {notice && (
        <div
          role="status"
          className={`mb-3 rounded-md border px-3 py-2 text-xs ${
            notice.success
              ? 'border-emerald-500/35 bg-emerald-500/10 text-emerald-600'
              : 'border-red-500/35 bg-red-500/10 text-red-600'
          }`}
        >
          {notice.message}
        </div>
      )}

      <div className="grid min-h-[520px] grid-cols-1 border-y border-[var(--border-primary)] md:grid-cols-[210px_minmax(0,1fr)]">
        <aside className="border-b border-[var(--border-primary)] py-2 md:border-b-0 md:border-r">
          {providers.length === 0 ? (
            <div className="px-3 py-6 text-center text-xs text-[var(--text-tertiary)]">
              暂无 Provider
            </div>
          ) : (
            <div className="space-y-1 px-2">
              {providers.map((provider) => (
                <button
                  key={provider.id}
                  type="button"
                  onClick={() => selectProvider(provider)}
                  className={`flex min-h-11 w-full items-center gap-2 rounded-md px-2 text-left ${
                    selectedId === provider.id
                      ? 'bg-[var(--bg-hover)] text-[var(--text-primary)]'
                      : 'text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]'
                  }`}
                >
                  <Server size={15} className="shrink-0" />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-xs font-medium">{provider.name}</span>
                    <span className="block text-[10px] text-[var(--text-tertiary)]">
                      {provider.model_count} models
                    </span>
                  </span>
                </button>
              ))}
            </div>
          )}
        </aside>

        <main className="min-w-0 divide-y divide-[var(--border-primary)]">
          <section className="space-y-3 p-4">
            <div className="flex items-center justify-between">
              <h4 className="text-xs font-semibold text-[var(--text-primary)]">
                {selectedProvider ? 'Provider 配置' : '新 Provider'}
              </h4>
              {selectedProvider && (
                <button
                  type="button"
                  title="删除 Provider"
                  aria-label="删除 Provider"
                  onClick={() => void removeProvider()}
                  disabled={busy === selectedProvider.id}
                  className="flex h-8 w-8 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-red-500/10 hover:text-red-600 disabled:opacity-40"
                >
                  <Trash2 size={15} />
                </button>
              )}
            </div>

            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <label className="text-xs text-[var(--text-secondary)]">
                Provider ID
                <input
                  value={providerForm.id}
                  disabled={selectedProvider != null}
                  onChange={(event) =>
                    setProviderForm((value) => ({ ...value, id: event.target.value }))
                  }
                  placeholder="company-gateway"
                  className={`${fieldClass} disabled:opacity-60`}
                />
              </label>
              <label className="text-xs text-[var(--text-secondary)]">
                名称
                <input
                  value={providerForm.name}
                  onChange={(event) =>
                    setProviderForm((value) => ({ ...value, name: event.target.value }))
                  }
                  placeholder="Company Gateway"
                  className={fieldClass}
                />
              </label>
            </div>

            <label className="block text-xs text-[var(--text-secondary)]">
              API 根地址
              <input
                value={providerForm.baseUrl}
                onChange={(event) =>
                  setProviderForm((value) => ({ ...value, baseUrl: event.target.value }))
                }
                placeholder="https://gateway.example.com/v1"
                className={fieldClass}
              />
            </label>

            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <label className="text-xs text-[var(--text-secondary)]">
                API Key
                <input
                  type="password"
                  value={providerForm.apiKey}
                  onChange={(event) =>
                    setProviderForm((value) => ({ ...value, apiKey: event.target.value }))
                  }
                  placeholder={selectedProvider?.has_auth_token ? '已配置' : '可选'}
                  className={fieldClass}
                />
              </label>
              <label className="text-xs text-[var(--text-secondary)]">
                环境变量
                <input
                  value={providerForm.apiKeyEnv}
                  onChange={(event) =>
                    setProviderForm((value) => ({ ...value, apiKeyEnv: event.target.value }))
                  }
                  placeholder="COMPANY_LLM_API_KEY"
                  className={fieldClass}
                />
              </label>
            </div>

            <div>
              <div className="mb-1 text-xs text-[var(--text-secondary)]">新模型默认协议</div>
              <ProtocolControl
                value={providerForm.protocol}
                onChange={(protocol) => setProviderForm((value) => ({ ...value, protocol }))}
              />
            </div>

            <div className="flex flex-wrap items-center justify-between gap-3">
              <label className="flex items-center gap-2 text-xs text-[var(--text-secondary)]">
                <input
                  type="checkbox"
                  checked={providerForm.requiresApiKey}
                  onChange={(event) =>
                    setProviderForm((value) => ({
                      ...value,
                      requiresApiKey: event.target.checked,
                    }))
                  }
                />
                必须提供 API Key
              </label>
              <button
                type="button"
                onClick={() => void saveProvider()}
                disabled={
                  busy === 'provider' || !providerForm.id.trim() || !providerForm.baseUrl.trim()
                }
                className="flex min-h-8 items-center gap-1.5 rounded-md bg-[var(--accent)] px-3 text-xs font-medium text-white disabled:opacity-40"
              >
                <Save size={14} /> {busy === 'provider' ? '保存中...' : '保存 Provider'}
              </button>
            </div>
          </section>

          {selectedProvider && (
            <section className="space-y-3 p-4">
              <div className="flex items-center justify-between">
                <h4 className="text-xs font-semibold text-[var(--text-primary)]">模型</h4>
                <button
                  type="button"
                  title="添加模型"
                  aria-label="添加模型"
                  onClick={() => setModelForm(emptyModel(selectedProvider.default_api_protocol))}
                  className="flex h-8 w-8 items-center justify-center rounded-md text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
                >
                  <Plus size={15} />
                </button>
              </div>

              {selectedModels.length > 0 && (
                <div className="divide-y divide-[var(--border-secondary)] border-y border-[var(--border-secondary)]">
                  {selectedModels.map((model) => (
                    <div key={model.id} className="flex min-h-12 items-center gap-2 py-2">
                      <button
                        type="button"
                        onClick={() => setModelForm(modelDraft(model))}
                        className="min-w-0 flex-1 text-left"
                      >
                        <span className="block truncate text-xs font-medium text-[var(--text-primary)]">
                          {model.display_name || model.model}
                        </span>
                        <span className="block truncate text-[10px] text-[var(--text-tertiary)]">
                          {model.model} · {model.api_protocol} · {model.input_modalities.join(', ')}
                        </span>
                        <span className="block truncate text-[10px] text-[var(--text-tertiary)]">
                          思考:{' '}
                          {thinkingLevelOptions(model.thinking_levels)
                            .map((level) => level.label)
                            .join(' / ')}
                        </span>
                      </button>
                      {model.is_default ? (
                        <span className="flex items-center gap-1 text-[10px] text-emerald-600">
                          <Check size={12} /> 使用中
                        </span>
                      ) : (
                        <button
                          type="button"
                          onClick={() => void setDefaultModel(model)}
                          disabled={busy === model.id}
                          className="min-h-7 rounded-md border border-[var(--border-primary)] px-2 text-[10px] text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] disabled:opacity-40"
                        >
                          启用
                        </button>
                      )}
                      <button
                        type="button"
                        title="删除模型"
                        aria-label={`删除 ${model.display_name || model.model}`}
                        onClick={() => void removeModel(model)}
                        disabled={busy === model.id}
                        className="flex h-7 w-7 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-red-500/10 hover:text-red-600 disabled:opacity-40"
                      >
                        <Trash2 size={13} />
                      </button>
                    </div>
                  ))}
                </div>
              )}

              <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                <label className="text-xs text-[var(--text-secondary)]">
                  模型 ID
                  <input
                    value={modelForm.model}
                    onChange={(event) =>
                      setModelForm((value) => ({ ...value, model: event.target.value }))
                    }
                    placeholder="model-name"
                    className={fieldClass}
                  />
                </label>
                <label className="text-xs text-[var(--text-secondary)]">
                  显示名称
                  <input
                    value={modelForm.displayName}
                    onChange={(event) =>
                      setModelForm((value) => ({ ...value, displayName: event.target.value }))
                    }
                    placeholder="Model Name"
                    className={fieldClass}
                  />
                </label>
              </div>

              <div>
                <div className="mb-1 text-xs text-[var(--text-secondary)]">API 协议</div>
                <ProtocolControl
                  value={modelForm.protocol}
                  onChange={(protocol) => setModelForm((value) => ({ ...value, protocol }))}
                />
              </div>

              <div>
                <div className="mb-1 text-xs text-[var(--text-secondary)]">输入能力</div>
                <div className="flex min-h-9 flex-wrap items-center gap-4 rounded-md border border-[var(--border-primary)] px-3 text-xs text-[var(--text-secondary)]">
                  <label className="flex items-center gap-2">
                    <input type="checkbox" checked disabled /> 纯文本（默认）
                  </label>
                  <label className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      checked={modelForm.imageInput}
                      onChange={(event) =>
                        setModelForm((value) => ({ ...value, imageInput: event.target.checked }))
                      }
                    />{' '}
                    图像
                  </label>
                  <label className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      checked={modelForm.audioInput}
                      onChange={(event) =>
                        setModelForm((value) => ({ ...value, audioInput: event.target.checked }))
                      }
                    />{' '}
                    音频
                  </label>
                  <label className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      checked={modelForm.videoInput}
                      onChange={(event) =>
                        setModelForm((value) => ({ ...value, videoInput: event.target.checked }))
                      }
                    />{' '}
                    视频
                  </label>
                </div>
              </div>

              <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
                {[
                  ['Temperature', 'temperature'],
                  ['Max tokens', 'maxTokens'],
                  ['Context window', 'contextWindow'],
                ].map(([label, key]) => (
                  <label key={key} className="text-xs text-[var(--text-secondary)]">
                    {label}
                    <input
                      type="number"
                      step={key === 'temperature' ? '0.1' : '1'}
                      value={modelForm[key as 'temperature' | 'maxTokens' | 'contextWindow']}
                      onChange={(event) =>
                        setModelForm((value) => ({ ...value, [key]: event.target.value }))
                      }
                      className={fieldClass}
                    />
                  </label>
                ))}
              </div>

              <div className="flex flex-wrap justify-end gap-2">
                <button
                  type="button"
                  onClick={() => void testConnection()}
                  disabled={busy === 'test' || !modelForm.model.trim()}
                  className="flex min-h-8 items-center gap-1.5 rounded-md border border-[var(--border-primary)] px-3 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] disabled:opacity-40"
                >
                  <Zap size={14} /> {busy === 'test' ? '测试中...' : '测试连接'}
                </button>
                <button
                  type="button"
                  onClick={() => void saveModel(false)}
                  disabled={busy === 'model' || !modelForm.model.trim()}
                  className="min-h-8 rounded-md border border-[var(--border-primary)] px-3 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] disabled:opacity-40"
                >
                  保存
                </button>
                <button
                  type="button"
                  onClick={() => void saveModel(true)}
                  disabled={busy === 'model' || !modelForm.model.trim()}
                  className="flex min-h-8 items-center gap-1.5 rounded-md bg-[var(--accent)] px-3 text-xs font-medium text-white disabled:opacity-40"
                >
                  <Check size={14} /> 保存并启用
                </button>
              </div>
            </section>
          )}
        </main>
      </div>
    </div>
  );
}
