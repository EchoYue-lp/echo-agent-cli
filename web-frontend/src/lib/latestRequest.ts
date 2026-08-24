export interface LatestRequestIdentity {
  scopeKey: string;
  sequence: number;
}

/** Rejects late responses from an older request or workspace incarnation. */
export class LatestRequestFence {
  private sequence = 0;

  begin(scopeKey: string): LatestRequestIdentity {
    this.sequence += 1;
    return { scopeKey, sequence: this.sequence };
  }

  isCurrent(request: LatestRequestIdentity, currentScopeKey: string): boolean {
    return request.scopeKey === currentScopeKey && request.sequence === this.sequence;
  }
}

/** Owns cleanup for the latest operation independently from result-selection fences. */
export class LatestOperationOwner {
  private sequence = 0;

  begin(): number {
    this.sequence += 1;
    return this.sequence;
  }

  isCurrent(operation: number): boolean {
    return operation === this.sequence;
  }
}
