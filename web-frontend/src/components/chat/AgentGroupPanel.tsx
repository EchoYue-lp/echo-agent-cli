import { useCallback, useEffect, useMemo, useState } from 'react';
import { LoaderCircle, Plus, Save, Trash2, UserRoundPlus } from 'lucide-react';
import {
  agentApi,
  type AgentAddress,
  type AgentEndpoint,
  type AgentGroup,
  type AgentGroupMember,
} from '../../api/endpoints';
import { useToastStore } from '../../stores/toastStore';

interface AgentGroupPanelProps {
  endpoints: AgentEndpoint[];
  currentAddress: AgentAddress | null;
}

interface MemberDraft {
  endpointKey: string;
  role: string;
  label: string | null;
}

interface GroupDraft {
  groupId: string | null;
  name: string;
  leaderKey: string;
  members: MemberDraft[];
}

const addressKey = (address: AgentAddress) => `${address.workspace_id}/${address.conversation_id}`;

const groupToDraft = (group: AgentGroup): GroupDraft => ({
  groupId: group.group_id,
  name: group.name,
  leaderKey: addressKey(group.leader),
  members: group.members.map((member) => ({
    endpointKey: addressKey(member.address),
    role: member.subagent_role,
    label: member.label,
  })),
});

export function AgentGroupPanel({ endpoints, currentAddress }: AgentGroupPanelProps) {
  const [groups, setGroups] = useState<AgentGroup[]>([]);
  const [draft, setDraft] = useState<GroupDraft | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const endpointsByKey = useMemo(
    () => new Map(endpoints.map((endpoint) => [addressKey(endpoint.address), endpoint])),
    [endpoints]
  );

  const newDraft = useCallback((): GroupDraft | null => {
    const leaderKey =
      (currentAddress && endpointsByKey.has(addressKey(currentAddress))
        ? addressKey(currentAddress)
        : endpoints[0] && addressKey(endpoints[0].address)) || '';
    const firstMember = endpoints.find((endpoint) => addressKey(endpoint.address) !== leaderKey);
    if (!leaderKey || !firstMember) return null;
    return {
      groupId: null,
      name: '',
      leaderKey,
      members: [{ endpointKey: addressKey(firstMember.address), role: 'explorer', label: null }],
    };
  }, [currentAddress, endpoints, endpointsByKey]);

  const loadGroups = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const response = await agentApi.listGroups();
      setGroups(response.groups);
      setDraft((existing) => {
        const selected =
          existing?.groupId && response.groups.find((group) => group.group_id === existing.groupId);
        return selected
          ? groupToDraft(selected)
          : response.groups[0]
            ? groupToDraft(response.groups[0])
            : newDraft();
      });
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setLoading(false);
    }
  }, [newDraft]);

  useEffect(() => {
    void loadGroups();
  }, [loadGroups]);

  const endpointLabel = (key: string) => {
    const endpoint = endpointsByKey.get(key);
    if (!endpoint) return key;
    return `${endpoint.workspace_name} / ${endpoint.conversation_title || '未命名会话'}`;
  };

  const handleLeaderChange = (leaderKey: string) => {
    setDraft((current) => {
      if (!current) return current;
      const members = current.members.filter((member) => member.endpointKey !== leaderKey);
      const replacement = endpoints.find(
        (endpoint) =>
          addressKey(endpoint.address) !== leaderKey &&
          !members.some((member) => member.endpointKey === addressKey(endpoint.address))
      );
      return {
        ...current,
        leaderKey,
        members:
          members.length > 0 || !replacement
            ? members
            : [
                {
                  endpointKey: addressKey(replacement.address),
                  role: 'explorer',
                  label: null,
                },
              ],
      };
    });
  };

  const addMember = () => {
    setDraft((current) => {
      if (!current) return current;
      const endpoint = endpoints.find((candidate) => {
        const key = addressKey(candidate.address);
        return (
          key !== current.leaderKey && !current.members.some((member) => member.endpointKey === key)
        );
      });
      if (!endpoint) return current;
      return {
        ...current,
        members: [
          ...current.members,
          { endpointKey: addressKey(endpoint.address), role: '', label: null },
        ],
      };
    });
  };

  const saveGroup = async () => {
    if (!draft || saving) return;
    const leader = endpointsByKey.get(draft.leaderKey)?.address;
    const members: AgentGroupMember[] = draft.members.flatMap((member) => {
      const address = endpointsByKey.get(member.endpointKey)?.address;
      return address ? [{ address, subagent_role: member.role.trim(), label: member.label }] : [];
    });
    if (!draft.name.trim() || !leader || members.length !== draft.members.length) {
      setError('请填写组名，并为每个角色选择有效会话。');
      return;
    }
    if (members.some((member) => !member.subagent_role)) {
      setError('每个成员都需要 Subagent 角色。');
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const request = { name: draft.name.trim(), leader, members };
      const response = draft.groupId
        ? await agentApi.updateGroup(draft.groupId, request)
        : await agentApi.createGroup(request);
      setGroups((current) => {
        const withoutSaved = current.filter((group) => group.group_id !== response.group.group_id);
        return [...withoutSaved, response.group].sort((left, right) =>
          left.name.localeCompare(right.name)
        );
      });
      setDraft(groupToDraft(response.group));
      useToastStore.getState().addToast('success', 'Agent 组已保存');
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSaving(false);
    }
  };

  const deleteGroup = async () => {
    if (!draft?.groupId || saving || !window.confirm(`删除 Agent 组“${draft.name}”？`)) return;
    setSaving(true);
    setError(null);
    try {
      await agentApi.deleteGroup(draft.groupId);
      const remaining = groups.filter((group) => group.group_id !== draft.groupId);
      setGroups(remaining);
      setDraft(remaining[0] ? groupToDraft(remaining[0]) : newDraft());
      useToastStore.getState().addToast('success', 'Agent 组已删除');
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="grid min-h-0 flex-1 grid-cols-1 grid-rows-[minmax(140px,0.34fr)_minmax(0,0.66fr)] md:grid-cols-[minmax(230px,0.34fr)_minmax(0,0.66fr)] md:grid-rows-1">
      <aside className="flex min-h-0 flex-col border-b border-[var(--border-primary)] md:border-b-0 md:border-r">
        <div className="flex h-12 shrink-0 items-center justify-between border-b border-[var(--border-secondary)] px-3">
          <span className="text-xs font-medium text-[var(--text-secondary)]">Agent 组</span>
          <button
            type="button"
            onClick={() => setDraft(newDraft())}
            disabled={!newDraft()}
            className="flex h-8 w-8 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)] disabled:opacity-40"
            title="新建 Agent 组"
            aria-label="新建 Agent 组"
          >
            <Plus size={16} />
          </button>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto py-1">
          {loading ? (
            <div className="flex h-full items-center justify-center">
              <LoaderCircle size={18} className="animate-spin text-[var(--text-tertiary)]" />
            </div>
          ) : groups.length === 0 ? (
            <div className="px-4 py-8 text-center text-xs text-[var(--text-tertiary)]">
              暂无 Agent 组
            </div>
          ) : (
            groups.map((group) => (
              <button
                key={group.group_id}
                type="button"
                onClick={() => setDraft(groupToDraft(group))}
                className={`w-full border-l-2 px-3 py-2.5 text-left ${
                  draft?.groupId === group.group_id
                    ? 'border-[var(--accent)] bg-[var(--bg-sidebar-active)]'
                    : 'border-transparent hover:bg-[var(--bg-hover)]'
                }`}
              >
                <span className="block truncate text-sm font-medium text-[var(--text-primary)]">
                  {group.name}
                </span>
                <span className="mt-0.5 block text-[11px] text-[var(--text-tertiary)]">
                  {group.members.length} 个成员
                </span>
              </button>
            ))
          )}
        </div>
      </aside>

      <main className="min-h-0 overflow-y-auto p-4">
        {draft ? (
          <div className="mx-auto flex w-full max-w-2xl flex-col gap-4">
            <label className="flex flex-col gap-1.5 text-xs text-[var(--text-secondary)]">
              组名
              <input
                value={draft.name}
                onChange={(event) =>
                  setDraft((current) =>
                    current ? { ...current, name: event.target.value } : current
                  )
                }
                className="h-9 rounded-md border border-[var(--border-primary)] bg-[var(--bg-input)] px-3 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--border-focus)]"
                aria-label="Agent 组名"
              />
            </label>

            <label className="flex flex-col gap-1.5 text-xs text-[var(--text-secondary)]">
              领导会话
              <select
                value={draft.leaderKey}
                onChange={(event) => handleLeaderChange(event.target.value)}
                className="h-9 rounded-md border border-[var(--border-primary)] bg-[var(--bg-input)] px-3 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--border-focus)]"
                aria-label="Agent 组领导会话"
              >
                {endpoints.map((endpoint) => {
                  const key = addressKey(endpoint.address);
                  return (
                    <option key={key} value={key}>
                      {endpointLabel(key)}
                    </option>
                  );
                })}
              </select>
            </label>

            <section className="border-t border-[var(--border-secondary)] pt-4">
              <div className="mb-2 flex items-center justify-between">
                <span className="text-xs font-medium text-[var(--text-secondary)]">成员角色</span>
                <button
                  type="button"
                  onClick={addMember}
                  className="flex h-8 items-center gap-1.5 rounded-md px-2 text-xs text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-[var(--text-primary)]"
                >
                  <UserRoundPlus size={14} />
                  添加成员
                </button>
              </div>
              <div className="flex flex-col gap-2">
                {draft.members.map((member, index) => (
                  <div
                    key={`${member.endpointKey}-${index}`}
                    className="grid grid-cols-[minmax(100px,0.35fr)_minmax(150px,0.65fr)_32px] gap-2"
                  >
                    <input
                      value={member.role}
                      onChange={(event) =>
                        setDraft((current) => {
                          if (!current) return current;
                          return {
                            ...current,
                            members: current.members.map((item, itemIndex) =>
                              itemIndex === index ? { ...item, role: event.target.value } : item
                            ),
                          };
                        })
                      }
                      className="h-9 min-w-0 rounded-md border border-[var(--border-primary)] bg-[var(--bg-input)] px-2 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--border-focus)]"
                      placeholder="Subagent 角色"
                      aria-label={`成员 ${index + 1} 角色`}
                    />
                    <select
                      value={member.endpointKey}
                      onChange={(event) =>
                        setDraft((current) => {
                          if (!current) return current;
                          return {
                            ...current,
                            members: current.members.map((item, itemIndex) =>
                              itemIndex === index
                                ? { ...item, endpointKey: event.target.value }
                                : item
                            ),
                          };
                        })
                      }
                      className="h-9 min-w-0 rounded-md border border-[var(--border-primary)] bg-[var(--bg-input)] px-2 text-sm text-[var(--text-primary)] outline-none focus:border-[var(--border-focus)]"
                      aria-label={`成员 ${index + 1} 会话`}
                    >
                      {endpoints
                        .filter((endpoint) => addressKey(endpoint.address) !== draft.leaderKey)
                        .map((endpoint) => {
                          const key = addressKey(endpoint.address);
                          return (
                            <option key={key} value={key}>
                              {endpointLabel(key)}
                            </option>
                          );
                        })}
                    </select>
                    <button
                      type="button"
                      onClick={() =>
                        setDraft((current) =>
                          current
                            ? {
                                ...current,
                                members: current.members.filter(
                                  (_item, itemIndex) => itemIndex !== index
                                ),
                              }
                            : current
                        )
                      }
                      className="flex h-9 w-8 items-center justify-center rounded-md text-[var(--text-tertiary)] hover:bg-[var(--bg-hover)] hover:text-[var(--color-error-text)]"
                      title="移除成员"
                      aria-label={`移除成员 ${index + 1}`}
                    >
                      <Trash2 size={14} />
                    </button>
                  </div>
                ))}
              </div>
            </section>

            {error && (
              <div className="text-xs text-[var(--color-error-text)]" role="alert">
                {error}
              </div>
            )}

            <div className="flex items-center justify-end gap-2 border-t border-[var(--border-secondary)] pt-4">
              {draft.groupId && (
                <button
                  type="button"
                  onClick={() => void deleteGroup()}
                  disabled={saving}
                  className="flex h-9 items-center gap-2 rounded-md px-3 text-sm text-[var(--color-error-text)] hover:bg-[var(--bg-hover)] disabled:opacity-50"
                >
                  <Trash2 size={15} />
                  删除
                </button>
              )}
              <button
                type="button"
                onClick={() => void saveGroup()}
                disabled={saving || draft.members.length === 0}
                className="flex h-9 items-center gap-2 rounded-md bg-[var(--accent)] px-3 text-sm font-medium text-[var(--text-on-accent)] hover:opacity-90 disabled:opacity-45"
              >
                {saving ? <LoaderCircle size={15} className="animate-spin" /> : <Save size={15} />}
                保存
              </button>
            </div>
          </div>
        ) : (
          <div className="flex h-full items-center justify-center text-xs text-[var(--text-tertiary)]">
            至少需要两个可用会话
          </div>
        )}
      </main>
    </div>
  );
}
