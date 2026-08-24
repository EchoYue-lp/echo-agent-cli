import { describe, expect, it } from 'vitest';
import { LatestOperationOwner, LatestRequestFence } from './latestRequest';

function deferred<T>() {
  let resolve: ((value: T) => void) | undefined;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve: (value: T) => resolve?.(value) };
}

describe('LatestRequestFence', () => {
  it('drops an older same-scope response that completes after the latest request', async () => {
    const fence = new LatestRequestFence();
    const older = deferred<string>();
    const latest = deferred<string>();
    const olderRequest = fence.begin('workspace-a:generation-1');
    const latestRequest = fence.begin('workspace-a:generation-1');
    let visible = '';
    const publish = async (
      request: ReturnType<LatestRequestFence['begin']>,
      response: Promise<string>
    ) => {
      const value = await response;
      if (fence.isCurrent(request, 'workspace-a:generation-1')) visible = value;
    };

    const olderPublish = publish(olderRequest, older.promise);
    const latestPublish = publish(latestRequest, latest.promise);
    latest.resolve('latest');
    await latestPublish;
    older.resolve('older');
    await olderPublish;

    expect(visible).toBe('latest');
  });

  it.each(['save', 'run', 'review-save'])(
    'drops a late %s result after the user selects another document',
    async () => {
      const fence = new LatestRequestFence();
      const mutation = deferred<string>();
      const selection = deferred<string>();
      const mutationRequest = fence.begin('workspace-a:generation-1');
      const selectionRequest = fence.begin('workspace-a:generation-1');
      let selected = 'analysis-a';
      const publish = async (
        request: ReturnType<LatestRequestFence['begin']>,
        response: Promise<string>
      ) => {
        const value = await response;
        if (fence.isCurrent(request, 'workspace-a:generation-1')) selected = value;
      };

      const mutationPublish = publish(mutationRequest, mutation.promise);
      const selectionPublish = publish(selectionRequest, selection.promise);
      selection.resolve('analysis-b');
      await selectionPublish;
      mutation.resolve('analysis-a-late');
      await mutationPublish;

      expect(selected).toBe('analysis-b');
    }
  );

  it('keeps operation cleanup independent from selection refreshes', () => {
    const selectionFence = new LatestRequestFence();
    const operationOwner = new LatestOperationOwner();
    const operation = operationOwner.begin();

    selectionFence.begin('workspace-a:generation-1');
    selectionFence.begin('workspace-a:generation-1');

    expect(operationOwner.isCurrent(operation)).toBe(true);
  });

  it('lets only the latest overlapping operation clear its busy state', () => {
    const operationOwner = new LatestOperationOwner();
    const older = operationOwner.begin();
    const latest = operationOwner.begin();

    expect(operationOwner.isCurrent(older)).toBe(false);
    expect(operationOwner.isCurrent(latest)).toBe(true);
  });
});
