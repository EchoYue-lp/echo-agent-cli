import { useState } from 'react';
import { ChevronRight, ChevronDown, File, Folder } from 'lucide-react';

interface TreeNode {
  name: string;
  path: string;
  is_dir: boolean;
  children?: TreeNode[];
}

interface FileTreeProps {
  tree: TreeNode[];
  selectedFile: string | null;
  onSelect: (path: string) => void;
}

const FILE_COLORS: Record<string, string> = {
  rs: '#dea584', ts: '#3178c6', tsx: '#3178c6', js: '#f7df1e', jsx: '#61dafb',
  py: '#3572A5', go: '#00ADD8', java: '#b07219', rb: '#701516',
  json: '#292929', yaml: '#cb171e', yml: '#cb171e', toml: '#9c4221',
  md: '#083fa1', css: '#563d7c', html: '#e34c26',
};

function getIconColor(ext?: string): string {
  return ext ? (FILE_COLORS[ext] || '#8b949e') : '#8b949e';
}

function TreeItem({
  node, depth, selectedFile, onSelect,
}: {
  node: TreeNode; depth: number; selectedFile: string | null; onSelect: (path: string) => void;
}) {
  const [expanded, setExpanded] = useState(depth === 0);

  const handleClick = () => {
    if (node.is_dir) {
      setExpanded(!expanded);
    } else {
      onSelect(node.path);
    }
  };

  const ext = node.name.split('.').pop();
  const isSelected = selectedFile === node.path;

  return (
    <div>
      <button
        onClick={handleClick}
        className="flex w-full items-center gap-1.5 rounded px-2 py-1 text-left text-sm transition-colors"
        style={{
          paddingLeft: `${depth * 16 + 8}px`,
          background: isSelected ? 'var(--bg-sidebar-active)' : 'transparent',
          color: 'var(--text-primary)',
        }}
        onMouseEnter={(e) => {
          if (!isSelected) e.currentTarget.style.background = 'var(--bg-hover)';
        }}
        onMouseLeave={(e) => {
          if (!isSelected) e.currentTarget.style.background = 'transparent';
        }}
      >
        {node.is_dir ? (
          expanded ? <ChevronDown size={14} style={{ color: 'var(--text-tertiary)' }} /> : <ChevronRight size={14} style={{ color: 'var(--text-tertiary)' }} />
        ) : (
          <span style={{ width: 14 }} />
        )}
        {node.is_dir ? (
          <Folder size={14} style={{ color: '#54aeff' }} />
        ) : (
          <File size={14} style={{ color: getIconColor(ext) }} />
        )}
        <span className="truncate">{node.name}</span>
      </button>
      {node.is_dir && expanded && node.children && (
        <div>
          {node.children.map((child) => (
            <TreeItem key={child.path} node={child} depth={depth + 1} selectedFile={selectedFile} onSelect={onSelect} />
          ))}
        </div>
      )}
    </div>
  );
}

export function FileTree({ tree, selectedFile, onSelect }: FileTreeProps) {
  return (
    <div className="py-1">
      {tree.map((node) => (
        <TreeItem key={node.path} node={node} depth={0} selectedFile={selectedFile} onSelect={onSelect} />
      ))}
    </div>
  );
}
