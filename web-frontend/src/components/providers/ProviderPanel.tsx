import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  AudioLines,
  Brain,
  Check,
  ChevronDown,
  ChevronUp,
  Image as ImageIcon,
  Pencil,
  Plus,
  Save,
  Server,
  Trash2,
  Video,
  X,
  Zap,
} from 'lucide-react';
import { providerApi } from '../../api/endpoints';
import type {
  ConfiguredModel,
  LlmApiProtocol,
  ModelInputModality,
  ModelProviderView,
} from '../../generated';
import { errorMessage } from '../../lib/tauri-bridge';
import { Modal } from '../common/Modal';

const MODELS_CHANGED_EVENT = 'eko:models-changed';
const PROTOCOLS: ReadonlyArray<[LlmApiProtocol, string]> = [
  ['chat_completions', 'Chat Completions'],
  ['responses', 'Responses'],
  ['anthropic', 'Anthropic'],
];

const PROTOCOL_LABELS: Record<LlmApiProtocol, string> = {
  chat_completions: 'Chat Completions',
  responses: 'Responses',
  anthropic: 'Anthropic',
};

interface ProviderDraft {
  name: string;
  baseUrl: string;
  apiKey: string;
}

interface ModelDraft {
  id?: string;
  model: string;
  protocol: LlmApiProtocol;
  imageInput: boolean;
  audioInput: boolean;
  videoInput: boolean;
  temperature: string;
  maxTokens: string;
  contextWindow: string;
}

interface ProviderEditorState {
  provider: ModelProviderView | null;
}

interface ModelEditorState {
  provider: ModelProviderView;
  model: ConfiguredModel | null;
}

type DeleteTarget =
  | { kind: 'provider'; provider: ModelProviderView; modelCount: number }
  | { kind: 'model'; model: ConfiguredModel };

type Notice = { success: boolean; message: string } | null;

const fieldClass =
  'mt-1 min-h-9 w-full rounded-md border border-[var(--border-primary)] bg-[var(--bg-input)] px-3 text-sm text-[var(--text-primary)] outline-none placeholder:text-[var(--text-tertiary)] focus:border-[var(--border-focus)]';
const iconButtonClass =
  'flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-[var(--text-tertiary)] transition-colors hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] disabled:opacity-40';
const dangerIconButtonClass = `${iconButtonClass} hover:bg-red-500/10 hover:text-red-600`;

const emptyProvider = (): ProviderDraft => ({ name: '', baseUrl: '', apiKey: '' });

const providerDraft = (provider: ModelProviderView): ProviderDraft => ({
  name: provider.name,
  baseUrl: provider.base_url,
  apiKey: '',
});

const emptyModel = (protocol: LlmApiProtocol = 'chat_completions'): ModelDraft => ({
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
  model: model.model,
  protocol: model.api_protocol,
  imageInput: model.input_modalities.includes('image'),
  audioInput: model.input_modalities.includes('audio'),
  videoInput: model.input_modalities.includes('video'),
  temperature: model.temperature == null ? '' : String(model.temperature),
  maxTokens: model.max_tokens == null ? '' : String(model.max_tokens),
  contextWindow: model.context_window == null ? '' : String(model.context_window),
});

function providerHost(baseUrl: string): string {
  try {
    return new URL(baseUrl).host;
  } catch {
    return baseUrl;
  }
}

function modelName(model: ConfiguredModel): string {
  return model.display_name || model.model;
}

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

function DialogHeader({ title, onClose }: { title: string; onClose: () => void }) {
  return (
    <div className="flex min-h-12 items-center justify-between border-b border-[var(--border-primary)] px-4">
      <h3 className="text-sm font-semibold text-[var(--text-primary)]">{title}</h3>
      <button type="button" aria-label="关闭" onClick={onClose} className={iconButtonClass}>
        <X size={16} />
      </button>
    </div>
  );
}

function InlineNotice({ notice }: { notice: Notice }) {
  if (!notice) return null;
  return (
    <div
      role="status"
      className={`rounded-md border px-3 py-2 text-xs ${
        notice.success
          ? 'border-emerald-500/35 bg-emerald-500/10 text-emerald-600'
          : 'border-red-500/35 bg-red-500/10 text-red-600'
      }`}
    >
      {notice.message}
    </div>
  );
}

export function ProviderPanel() {
  const [providers, setProviders] = useState<ModelProviderView[]>([]);
  const [models, setModels] = useState<ConfiguredModel[]>([]);
  const [providerForm, setProviderForm] = useState<ProviderDraft>(emptyProvider);
  const [modelForm, setModelForm] = useState<ModelDraft>(emptyModel);
  const [providerEditor, setProviderEditor] = useState<ProviderEditorState | null>(null);
  const [modelEditor, setModelEditor] = useState<ModelEditorState | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<DeleteTarget | null>(null);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice>(null);
  const [dialogNotice, setDialogNotice] = useState<Notice>(null);

  const reload = useCallback(async () => {
    const [providerResult, modelResult] = await Promise.all([
      providerApi.listProviders(),
      providerApi.listConfigured(),
    ]);
    setProviders(providerResult.providers);
    setModels(modelResult.models);
  }, []);

  useEffect(() => {
    void reload()
      .catch((error: unknown) => setNotice({ success: false, message: errorMessage(error) }))
      .finally(() => setLoading(false));
  }, [reload]);

  const modelsByProvider = useMemo(() => {
    const result = new Map<string, ConfiguredModel[]>();
    for (const provider of providers) result.set(provider.id, []);
    for (const model of models) result.get(model.provider)?.push(model);
    return result;
  }, [models, providers]);

  const openProviderEditor = (provider: ModelProviderView | null) => {
    setProviderForm(provider ? providerDraft(provider) : emptyProvider());
    setDialogNotice(null);
    setProviderEditor({ provider });
  };

  const openModelEditor = (provider: ModelProviderView, model: ConfiguredModel | null) => {
    setModelForm(model ? modelDraft(model) : emptyModel(provider.default_api_protocol));
    setAdvancedOpen(false);
    setDialogNotice(null);
    setModelEditor({ provider, model });
  };

  const modalities = (): ModelInputModality[] => [
    'text',
    ...(modelForm.imageInput ? (['image'] as ModelInputModality[]) : []),
    ...(modelForm.audioInput ? (['audio'] as ModelInputModality[]) : []),
    ...(modelForm.videoInput ? (['video'] as ModelInputModality[]) : []),
  ];

  const saveProvider = async () => {
    const current = providerEditor?.provider ?? null;
    setBusy('save-provider');
    setDialogNotice(null);
    try {
      await providerApi.upsertProvider({
        id: current?.id ?? `provider-${crypto.randomUUID()}`,
        name: providerForm.name.trim(),
        base_url: providerForm.baseUrl.trim(),
        api_key: providerForm.apiKey.trim() || undefined,
        api_key_env: current?.api_key_env ?? undefined,
        requires_api_key: false,
        default_api_protocol: current?.default_api_protocol ?? 'chat_completions',
      });
      await reload();
      setProviderEditor(null);
      setNotice({ success: true, message: `${providerForm.name.trim()} 已保存` });
    } catch (error: unknown) {
      setDialogNotice({ success: false, message: `保存失败: ${errorMessage(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const saveModel = async () => {
    if (!modelEditor) return;
    setBusy('save-model');
    setDialogNotice(null);
    try {
      await providerApi.upsertConfigured({
        id: modelForm.id,
        provider: modelEditor.provider.id,
        model: modelForm.model.trim(),
        api_protocol: modelForm.protocol,
        input_modalities: modalities(),
        temperature: modelForm.temperature ? Number(modelForm.temperature) : undefined,
        max_tokens: modelForm.maxTokens ? Number(modelForm.maxTokens) : undefined,
        context_window: modelForm.contextWindow ? Number(modelForm.contextWindow) : undefined,
        set_default: false,
      });
      await reload();
      setModelEditor(null);
      window.dispatchEvent(new Event(MODELS_CHANGED_EVENT));
      setNotice({ success: true, message: `${modelForm.model.trim()} 已保存` });
    } catch (error: unknown) {
      setDialogNotice({ success: false, message: `保存失败: ${errorMessage(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const testConnection = async () => {
    if (!modelEditor) return;
    setBusy('test-model');
    setDialogNotice(null);
    try {
      const result = await providerApi.test({
        provider: modelEditor.provider.id,
        model: modelForm.model.trim(),
        api_protocol: modelForm.protocol,
        input_modalities: modalities(),
        base_url: modelEditor.provider.base_url,
      });
      setDialogNotice({
        success: result.success,
        message: result.success
          ? `连接成功: ${result.model}`
          : `连接失败: ${result.error ?? '未知错误'}`,
      });
    } catch (error: unknown) {
      setDialogNotice({ success: false, message: `连接失败: ${errorMessage(error)}` });
    } finally {
      setBusy(null);
    }
  };

  const confirmDelete = async () => {
    if (!deleteTarget) return;
    setBusy('delete');
    setDialogNotice(null);
    try {
      if (deleteTarget.kind === 'provider') {
        await providerApi.deleteProvider(deleteTarget.provider.id);
        setNotice({
          success: true,
          message: `${deleteTarget.provider.name} 及其 ${deleteTarget.modelCount} 个模型已删除`,
        });
      } else {
        await providerApi.deleteConfigured(deleteTarget.model.id);
        setNotice({ success: true, message: `${modelName(deleteTarget.model)} 已删除` });
      }
      await reload();
      setDeleteTarget(null);
      window.dispatchEvent(new Event(MODELS_CHANGED_EVENT));
    } catch (error: unknown) {
      setDialogNotice({ success: false, message: `删除失败: ${errorMessage(error)}` });
    } finally {
      setBusy(null);
    }
  };

  if (loading) return <div className="p-4 text-sm text-[var(--text-tertiary)]">加载中...</div>;

  return (
    <div className="p-3">
      <div className="mb-3 flex min-h-9 items-center justify-between gap-3">
        <p className="text-xs text-[var(--text-tertiary)]">
          {providers.length} 个 Provider · {models.length} 个模型
        </p>
        <button
          type="button"
          onClick={() => openProviderEditor(null)}
          className="flex min-h-8 items-center gap-1.5 rounded-md bg-[var(--accent)] px-3 text-xs font-medium text-white"
        >
          <Plus size={14} /> 添加 Provider
        </button>
      </div>

      <InlineNotice notice={notice} />

      <div
        className={`overflow-hidden rounded-md border border-[var(--border-primary)] ${notice ? 'mt-3' : ''}`}
      >
        {providers.length === 0 ? (
          <div className="flex min-h-40 flex-col items-center justify-center gap-3 text-[var(--text-tertiary)]">
            <Server size={22} />
            <span className="text-xs">暂无 Provider</span>
          </div>
        ) : (
          <div className="divide-y divide-[var(--border-primary)]">
            {providers.map((provider) => {
              const providerModels = modelsByProvider.get(provider.id) ?? [];
              return (
                <section key={provider.id}>
                  <div className="flex min-h-16 items-center gap-3 bg-[var(--bg-secondary)] px-4 py-2">
                    <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-[var(--border-secondary)] text-[var(--text-secondary)]">
                      <Server size={16} />
                    </span>
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-sm font-medium text-[var(--text-primary)]">
                        {provider.name}
                      </div>
                      <div className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-[10px] text-[var(--text-tertiary)]">
                        <span>{providerHost(provider.base_url)}</span>
                        <span>·</span>
                        <span>{providerModels.length} 个模型</span>
                        <span>·</span>
                        <span>{provider.has_auth_token ? 'API Key 已配置' : '无 API Key'}</span>
                      </div>
                    </div>
                    <div className="flex items-center gap-0.5">
                      <button
                        type="button"
                        title="编辑 Provider"
                        aria-label={`编辑 ${provider.name}`}
                        onClick={() => openProviderEditor(provider)}
                        className={iconButtonClass}
                      >
                        <Pencil size={14} />
                      </button>
                      <button
                        type="button"
                        title="添加模型"
                        aria-label={`为 ${provider.name} 添加模型`}
                        onClick={() => openModelEditor(provider, null)}
                        className={iconButtonClass}
                      >
                        <Plus size={15} />
                      </button>
                      <button
                        type="button"
                        title="删除 Provider"
                        aria-label={`删除 ${provider.name}`}
                        onClick={() => {
                          setDialogNotice(null);
                          setDeleteTarget({
                            kind: 'provider',
                            provider,
                            modelCount: providerModels.length,
                          });
                        }}
                        className={dangerIconButtonClass}
                      >
                        <Trash2 size={14} />
                      </button>
                    </div>
                  </div>

                  {providerModels.length === 0 ? (
                    <div className="flex min-h-12 items-center px-16 text-xs text-[var(--text-tertiary)]">
                      暂无模型
                    </div>
                  ) : (
                    <div className="divide-y divide-[var(--border-secondary)]">
                      {providerModels.map((model) => (
                        <div
                          key={model.id}
                          className="flex min-h-14 items-center gap-3 px-4 py-2 pl-16"
                        >
                          <div className="min-w-0 flex-1">
                            <div className="flex min-w-0 flex-wrap items-center gap-2">
                              <span className="truncate text-xs font-medium text-[var(--text-primary)]">
                                {modelName(model)}
                              </span>
                              {model.is_default && (
                                <span className="flex items-center gap-1 text-[10px] text-emerald-600">
                                  <Check size={11} /> 使用中
                                </span>
                              )}
                            </div>
                            <div className="mt-1 flex flex-wrap items-center gap-2 text-[10px] text-[var(--text-tertiary)]">
                              <span>{PROTOCOL_LABELS[model.api_protocol]}</span>
                              {model.input_modalities.includes('image') && (
                                <span className="flex items-center gap-1" title="支持图片输入">
                                  <ImageIcon size={11} /> 图片
                                </span>
                              )}
                              {model.input_modalities.includes('audio') && (
                                <span className="flex items-center gap-1" title="支持音频输入">
                                  <AudioLines size={11} /> 音频
                                </span>
                              )}
                              {model.input_modalities.includes('video') && (
                                <span className="flex items-center gap-1" title="支持视频输入">
                                  <Video size={11} /> 视频
                                </span>
                              )}
                              <span className="flex items-center gap-1">
                                <Brain size={11} />
                                {model.thinking_levels.length > 0
                                  ? `思考 ${model.thinking_levels.length} 档`
                                  : '思考自动'}
                              </span>
                            </div>
                          </div>
                          <div className="flex items-center gap-0.5">
                            <button
                              type="button"
                              title="编辑模型"
                              aria-label={`编辑 ${modelName(model)}`}
                              onClick={() => openModelEditor(provider, model)}
                              className={iconButtonClass}
                            >
                              <Pencil size={14} />
                            </button>
                            <button
                              type="button"
                              title="删除模型"
                              aria-label={`删除 ${modelName(model)}`}
                              onClick={() => {
                                setDialogNotice(null);
                                setDeleteTarget({ kind: 'model', model });
                              }}
                              className={dangerIconButtonClass}
                            >
                              <Trash2 size={14} />
                            </button>
                          </div>
                        </div>
                      ))}
                    </div>
                  )}
                </section>
              );
            })}
          </div>
        )}
      </div>

      {providerEditor && (
        <Modal
          ariaLabel={providerEditor.provider ? '编辑 Provider' : '新增 Provider'}
          onClose={() => setProviderEditor(null)}
          className="w-[min(520px,calc(100vw-32px))] overflow-hidden rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-xl"
        >
          <DialogHeader
            title={providerEditor.provider ? '编辑 Provider' : '新增 Provider'}
            onClose={() => setProviderEditor(null)}
          />
          <div className="space-y-3 p-4">
            <InlineNotice notice={dialogNotice} />
            <label className="block text-xs text-[var(--text-secondary)]">
              Provider 名称
              <input
                autoFocus
                value={providerForm.name}
                onChange={(event) =>
                  setProviderForm((value) => ({ ...value, name: event.target.value }))
                }
                placeholder="例如 OpenAI、DeepSeek 或本地模型"
                className={fieldClass}
              />
            </label>
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
            <label className="block text-xs text-[var(--text-secondary)]">
              API Key（可选）
              <input
                type="password"
                value={providerForm.apiKey}
                onChange={(event) =>
                  setProviderForm((value) => ({ ...value, apiKey: event.target.value }))
                }
                placeholder={
                  providerEditor.provider?.has_auth_token
                    ? '已配置；留空将继续使用原 Key'
                    : '本地或免认证服务可留空'
                }
                className={fieldClass}
              />
            </label>
          </div>
          <div className="flex justify-end gap-2 border-t border-[var(--border-primary)] px-4 py-3">
            <button
              type="button"
              onClick={() => setProviderEditor(null)}
              className="min-h-8 rounded-md px-3 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
            >
              取消
            </button>
            <button
              type="button"
              onClick={() => void saveProvider()}
              disabled={
                busy === 'save-provider' ||
                !providerForm.name.trim() ||
                !providerForm.baseUrl.trim()
              }
              className="flex min-h-8 items-center gap-1.5 rounded-md bg-[var(--accent)] px-3 text-xs font-medium text-white disabled:opacity-40"
            >
              <Save size={14} /> {busy === 'save-provider' ? '保存中...' : '保存 Provider'}
            </button>
          </div>
        </Modal>
      )}

      {modelEditor && (
        <Modal
          ariaLabel={modelEditor.model ? '编辑模型' : '新增模型'}
          onClose={() => setModelEditor(null)}
          className="flex max-h-[min(760px,calc(100vh-32px))] w-[min(600px,calc(100vw-32px))] flex-col overflow-hidden rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-xl"
        >
          <DialogHeader
            title={modelEditor.model ? '编辑模型' : '新增模型'}
            onClose={() => setModelEditor(null)}
          />
          <div className="min-h-0 space-y-3 overflow-y-auto p-4">
            <p className="text-xs text-[var(--text-tertiary)]">{modelEditor.provider.name}</p>
            <InlineNotice notice={dialogNotice} />
            <label className="block text-xs text-[var(--text-secondary)]">
              API 模型名称
              <input
                autoFocus
                value={modelForm.model}
                onChange={(event) =>
                  setModelForm((value) => ({ ...value, model: event.target.value }))
                }
                placeholder="例如 gpt-5.6-sol"
                className={fieldClass}
              />
            </label>
            <div>
              <div className="mb-1 text-xs text-[var(--text-secondary)]">API 协议</div>
              <ProtocolControl
                value={modelForm.protocol}
                onChange={(protocol) => setModelForm((value) => ({ ...value, protocol }))}
              />
            </div>
            <div>
              <div className="mb-1 text-xs text-[var(--text-secondary)]">输入能力</div>
              <div className="flex min-h-10 flex-wrap items-center gap-4 rounded-md border border-[var(--border-primary)] px-3 text-xs text-[var(--text-secondary)]">
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
                  />
                  图片
                </label>
                <label className="flex items-center gap-2">
                  <input
                    type="checkbox"
                    checked={modelForm.audioInput}
                    onChange={(event) =>
                      setModelForm((value) => ({ ...value, audioInput: event.target.checked }))
                    }
                  />
                  音频
                </label>
                <label className="flex items-center gap-2">
                  <input
                    type="checkbox"
                    checked={modelForm.videoInput}
                    onChange={(event) =>
                      setModelForm((value) => ({ ...value, videoInput: event.target.checked }))
                    }
                  />
                  视频
                </label>
              </div>
            </div>
            <button
              type="button"
              aria-expanded={advancedOpen}
              onClick={() => setAdvancedOpen((value) => !value)}
              className="flex min-h-8 items-center gap-1 text-xs text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
            >
              {advancedOpen ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
              高级参数
            </button>
            {advancedOpen && (
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
            )}
          </div>
          <div className="flex flex-wrap justify-end gap-2 border-t border-[var(--border-primary)] px-4 py-3">
            <button
              type="button"
              onClick={() => void testConnection()}
              disabled={busy === 'test-model' || !modelForm.model.trim()}
              className="flex min-h-8 items-center gap-1.5 rounded-md border border-[var(--border-primary)] px-3 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] disabled:opacity-40"
            >
              <Zap size={14} /> {busy === 'test-model' ? '测试中...' : '测试连接'}
            </button>
            <button
              type="button"
              onClick={() => setModelEditor(null)}
              className="min-h-8 rounded-md px-3 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
            >
              取消
            </button>
            <button
              type="button"
              onClick={() => void saveModel()}
              disabled={busy === 'save-model' || !modelForm.model.trim()}
              className="flex min-h-8 items-center gap-1.5 rounded-md bg-[var(--accent)] px-3 text-xs font-medium text-white disabled:opacity-40"
            >
              <Save size={14} /> {busy === 'save-model' ? '保存中...' : '保存模型'}
            </button>
          </div>
        </Modal>
      )}

      {deleteTarget && (
        <Modal
          ariaLabel={deleteTarget.kind === 'provider' ? '删除 Provider' : '删除模型'}
          onClose={() => setDeleteTarget(null)}
          className="w-[min(420px,calc(100vw-32px))] overflow-hidden rounded-lg border border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-xl"
        >
          <DialogHeader
            title={deleteTarget.kind === 'provider' ? '删除 Provider' : '删除模型'}
            onClose={() => setDeleteTarget(null)}
          />
          <div className="space-y-3 p-4 text-sm text-[var(--text-secondary)]">
            <InlineNotice notice={dialogNotice} />
            {deleteTarget.kind === 'provider' ? (
              <p>
                确定删除“{deleteTarget.provider.name}”及其 {deleteTarget.modelCount} 个模型吗？
              </p>
            ) : (
              <p>确定删除“{modelName(deleteTarget.model)}”吗？</p>
            )}
          </div>
          <div className="flex justify-end gap-2 border-t border-[var(--border-primary)] px-4 py-3">
            <button
              type="button"
              onClick={() => setDeleteTarget(null)}
              className="min-h-8 rounded-md px-3 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)]"
            >
              取消
            </button>
            <button
              type="button"
              onClick={() => void confirmDelete()}
              disabled={busy === 'delete'}
              className="flex min-h-8 items-center gap-1.5 rounded-md bg-red-600 px-3 text-xs font-medium text-white disabled:opacity-40"
            >
              <Trash2 size={14} /> {busy === 'delete' ? '删除中...' : '删除'}
            </button>
          </div>
        </Modal>
      )}
    </div>
  );
}
