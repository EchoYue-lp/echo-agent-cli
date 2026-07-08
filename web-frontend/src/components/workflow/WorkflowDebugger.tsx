import { useState, useCallback } from 'react';

interface WorkflowNode {
  id: string;
  label: string;
  status: 'success' | 'failure' | 'running' | 'pending';
  input?: string;
  output?: string;
  duration?: number;
  x: number;
  y: number;
}

interface WorkflowEdge {
  from: string;
  to: string;
  label?: string;
}

interface WorkflowDebuggerProps {
  nodes: WorkflowNode[];
  edges: WorkflowEdge[];
  onNodeClick?: (node: WorkflowNode) => void;
  onStepForward?: () => void;
  onStepBack?: () => void;
  onReset?: () => void;
}

const statusConfig = {
  success: {
    color: 'bg-green-500',
    border: 'border-green-500',
    text: 'text-green-600 dark:text-green-400',
    bg: 'bg-green-50 dark:bg-green-900/20',
  },
  failure: {
    color: 'bg-red-500',
    border: 'border-red-500',
    text: 'text-red-600 dark:text-red-400',
    bg: 'bg-red-50 dark:bg-red-900/20',
  },
  running: {
    color: 'bg-amber-500',
    border: 'border-amber-500',
    text: 'text-amber-600 dark:text-amber-400',
    bg: 'bg-amber-50 dark:bg-amber-900/20',
  },
  pending: {
    color: 'bg-gray-300 dark:bg-gray-700',
    border: 'border-gray-300 dark:border-gray-700',
    text: 'text-gray-500 dark:text-gray-400',
    bg: 'bg-gray-50 dark:bg-gray-800/50',
  },
};

export function WorkflowDebugger({
  nodes,
  edges,
  onNodeClick,
  onStepForward,
  onStepBack,
  onReset,
}: WorkflowDebuggerProps) {
  const [selectedNode, setSelectedNode] = useState<WorkflowNode | null>(null);
  const [zoom, setZoom] = useState(1);

  const handleNodeClick = useCallback(
    (node: WorkflowNode) => {
      setSelectedNode(node);
      onNodeClick?.(node);
    },
    [onNodeClick]
  );

  const runningCount = nodes.filter((n) => n.status === 'running').length;
  const successCount = nodes.filter((n) => n.status === 'success').length;
  const failureCount = nodes.filter((n) => n.status === 'failure').length;

  return (
    <div className="h-full flex flex-col rounded-xl border border-[var(--border-primary)] bg-[var(--bg-primary)] overflow-hidden shadow-[var(--shadow-sm)]">
      {/* Toolbar */}
      <div className="flex items-center justify-between px-5 py-3 bg-[var(--bg-secondary)] border-b border-[var(--border-primary)]">
        <div className="flex items-center gap-2">
          <span className="text-sm font-semibold text-[var(--text-primary)]">Workflow</span>
          <div className="flex items-center gap-3 text-xs">
            <span className="flex items-center gap-1">
              <span className="w-2 h-2 rounded-full bg-green-500" />
              {successCount}
            </span>
            <span className="flex items-center gap-1">
              <span className="w-2 h-2 rounded-full bg-red-500" />
              {failureCount}
            </span>
            {runningCount > 0 && (
              <span className="flex items-center gap-1">
                <span className="w-2 h-2 rounded-full bg-amber-500 animate-pulse" />
                {runningCount} running
              </span>
            )}
          </div>
        </div>
        <div className="flex items-center gap-2">
          <div className="flex items-center gap-1 mr-3">
            <button
              onClick={() => setZoom((z) => Math.max(0.5, z - 0.1))}
              className="p-1.5 rounded-md hover:bg-[var(--bg-hover)] text-[var(--text-tertiary)]"
            >
              −
            </button>
            <span className="text-xs text-[var(--text-tertiary)] w-12 text-center">
              {Math.round(zoom * 100)}%
            </span>
            <button
              onClick={() => setZoom((z) => Math.min(2, z + 0.1))}
              className="p-1.5 rounded-md hover:bg-[var(--bg-hover)] text-[var(--text-tertiary)]"
            >
              +
            </button>
          </div>
          <div className="h-5 w-px bg-[var(--border-primary)] mx-1" />
          <button
            onClick={onStepBack}
            className="p-1.5 rounded-md hover:bg-[var(--bg-hover)] text-[var(--text-tertiary)]"
            title="Step Back"
          >
            <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                strokeWidth={2}
                d="M15 19l-7-7 7-7"
              />
            </svg>
          </button>
          <button
            onClick={onStepForward}
            className="p-1.5 rounded-md hover:bg-[var(--bg-hover)] text-[var(--text-tertiary)]"
            title="Step Forward"
          >
            <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M9 5l7 7-7 7" />
            </svg>
          </button>
          <button
            onClick={onReset}
            className="p-1.5 rounded-md hover:bg-[var(--bg-hover)] text-[var(--text-tertiary)]"
            title="Reset"
          >
            <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
              <path
                strokeLinecap="round"
                strokeLinejoin="round"
                strokeWidth={2}
                d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15"
              />
            </svg>
          </button>
        </div>
      </div>

      {/* Canvas */}
      <div className="flex-1 relative overflow-auto bg-[var(--bg-chat)]" style={{ minHeight: 400 }}>
        <div
          className="absolute inset-0"
          style={{ transform: `scale(${zoom})`, transformOrigin: 'top left' }}
        >
          {/* Grid background */}
          <div
            className="absolute inset-0 opacity-30"
            style={{
              backgroundImage: `radial-gradient(circle, var(--border-primary) 1px, transparent 1px)`,
              backgroundSize: '20px 20px',
            }}
          />
          {/* Edges */}
          <svg
            className="absolute inset-0 w-full h-full pointer-events-none"
            style={{ minWidth: 800, minHeight: 600 }}
          >
            <defs>
              <marker
                id="arrowhead"
                markerWidth="10"
                markerHeight="7"
                refX="9"
                refY="3.5"
                orient="auto"
              >
                <polygon points="0 0, 10 3.5, 0 7" fill="var(--text-tertiary)" />
              </marker>
            </defs>
            {edges.map((edge, i) => {
              const from = nodes.find((n) => n.id === edge.from);
              const to = nodes.find((n) => n.id === edge.to);
              if (!from || !to) return null;
              return (
                <line
                  key={i}
                  x1={from.x + 80}
                  y1={from.y + 40}
                  x2={to.x + 80}
                  y2={to.y + 40}
                  stroke="var(--border-primary)"
                  strokeWidth={2}
                  markerEnd="url(#arrowhead)"
                />
              );
            })}
          </svg>
          {/* Nodes */}
          {nodes.map((node) => {
            const config = statusConfig[node.status];
            return (
              <div
                key={node.id}
                className="absolute cursor-pointer"
                style={{ left: node.x, top: node.y, width: 160 }}
                onClick={() => handleNodeClick(node)}
              >
                <div
                  className={`rounded-xl border-2 ${config.border} bg-[var(--bg-primary)] shadow-[var(--shadow-sm)] hover:shadow-[var(--shadow-md)] transition-all p-3`}
                >
                  <div className="flex items-center gap-2 mb-1.5">
                    <span
                      className={`w-2.5 h-2.5 rounded-full ${config.color} ${node.status === 'running' ? 'animate-pulse' : ''}`}
                    />
                    <span className="text-xs font-medium text-[var(--text-primary)] truncate">
                      {node.label}
                    </span>
                  </div>
                  <div
                    className={`text-xs px-2 py-0.5 rounded-md ${config.bg} ${config.text} inline-block`}
                  >
                    {node.status}
                  </div>
                  {node.duration && (
                    <span className="text-xs text-[var(--text-tertiary)] mt-1 block">
                      {node.duration}ms
                    </span>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      </div>

      {/* Inspector */}
      {selectedNode && (
        <div className="absolute bottom-4 right-4 w-80 rounded-xl border border-[var(--border-primary)] bg-[var(--bg-primary)] shadow-[var(--shadow-lg)] p-4 animate-scale-in">
          <div className="flex items-center justify-between mb-3">
            <h4 className="text-sm font-semibold text-[var(--text-primary)]">
              {selectedNode.label}
            </h4>
            <button
              onClick={() => setSelectedNode(null)}
              className="text-[var(--text-tertiary)] hover:text-[var(--text-primary)]"
            >
              <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  strokeWidth={2}
                  d="M6 18L18 6M6 6l12 12"
                />
              </svg>
            </button>
          </div>
          <div className="space-y-3">
            <div className="flex items-center gap-2">
              <span className="text-xs text-[var(--text-tertiary)] w-16">Status</span>
              <span
                className={`text-xs px-2 py-0.5 rounded-md ${statusConfig[selectedNode.status].bg} ${statusConfig[selectedNode.status].text}`}
              >
                {selectedNode.status}
              </span>
            </div>
            {selectedNode.duration && (
              <div className="flex items-center gap-2">
                <span className="text-xs text-[var(--text-tertiary)] w-16">Duration</span>
                <span className="text-xs text-[var(--text-primary)]">
                  {selectedNode.duration}ms
                </span>
              </div>
            )}
            {selectedNode.input && (
              <div>
                <span className="text-xs text-[var(--text-tertiary)] block mb-1">Input</span>
                <pre className="text-xs bg-[var(--bg-secondary)] p-2 rounded-lg text-[var(--text-primary)] overflow-x-auto">
                  {selectedNode.input}
                </pre>
              </div>
            )}
            {selectedNode.output && (
              <div>
                <span className="text-xs text-[var(--text-tertiary)] block mb-1">Output</span>
                <pre className="text-xs bg-[var(--bg-secondary)] p-2 rounded-lg text-[var(--text-primary)] overflow-x-auto">
                  {selectedNode.output}
                </pre>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

export default WorkflowDebugger;
