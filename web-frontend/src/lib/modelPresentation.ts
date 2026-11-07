import type { ConfiguredModel, ModelProviderView } from '../generated';
import { providerApi } from '../api/endpoints';

/** Provider ID用于匹配，展示用户配置的名称，避免同名模型和内部UUID混淆。 */
export function modelLabel(
  model: Pick<ConfiguredModel, 'provider' | 'display_name' | 'model'>,
  providers: readonly Pick<ModelProviderView, 'id' | 'name'>[] = []
) {
  const provider = providers.find((item) => item.id === model.provider);
  return `${provider?.name || model.provider} / ${model.display_name || model.model}`;
}

/** 名称查询暂不可用时仍保留模型ID和Provider ID，不阻断原模型选择。 */
export async function configuredModelPresentation() {
  const [response, { providers }] = await Promise.all([
    providerApi.listConfigured(),
    providerApi.listProviders().catch(() => ({ providers: [] })),
  ]);
  return { ...response, providers };
}
