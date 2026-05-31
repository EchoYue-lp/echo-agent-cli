import { useState, useRef, useEffect } from 'react';

interface TerminalProps {
  sessionId?: string;
}

export function Terminal({ sessionId }: TerminalProps) {
  const [lines, setLines] = useState<string[]>(['Welcome to Echo Agent Terminal', '$ ']);
  const [input, setInput] = useState('');
  const [connected, setConnected] = useState(false);
  const wsRef = useRef<WebSocket | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const id = sessionId || 'default';
    let ws: WebSocket;

    try {
      const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
      const wsUrl = `${protocol}//${window.location.host}/api/terminal/${id}/ws`;
      ws = new WebSocket(wsUrl);
      wsRef.current = ws;

      ws.onopen = () => setConnected(true);
      ws.onclose = () => setConnected(false);
      ws.onerror = () => setConnected(false);

      ws.onmessage = (event) => {
        try {
          const msg = JSON.parse(event.data);
          if (msg.type === 'output') {
            setLines((prev) => {
              const last = prev[prev.length - 1] || '';
              return [...prev.slice(0, -1), last + msg.data];
            });
          }
        } catch {
          // ignore
        }
      };
    } catch {
      setConnected(false);
    }

    return () => { ws?.close(); };
  }, [sessionId]);

  useEffect(() => {
    if (containerRef.current) {
      containerRef.current.scrollTop = containerRef.current.scrollHeight;
    }
  }, [lines]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      const ws = wsRef.current;
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: 'input', data: input + '\n' }));
        setLines((prev) => [...prev, '$ ']);
        setInput('');
      }
    }
  };

  return (
    <div className="flex flex-col h-full bg-black text-green-400 font-mono text-sm rounded-lg overflow-hidden">
      <div className="flex items-center justify-between px-3 py-1.5 border-b border-gray-700">
        <span className="text-xs text-gray-400">Terminal</span>
        <div className="flex items-center gap-2">
          <span className={`text-[10px] ${connected ? 'text-green-400' : 'text-red-400'}`}>
            {connected ? '● Connected' : '○ Disconnected'}
          </span>
        </div>
      </div>
      <div ref={containerRef} className="flex-1 overflow-y-auto p-2 whitespace-pre-wrap min-h-[200px]">
        {lines.map((line, i) => (
          <div key={i}>{line}</div>
        ))}
      </div>
      <div className="flex items-center border-t border-gray-700 p-2">
        <span className="text-green-400 mr-2">$</span>
        <input
          ref={inputRef}
          type="text"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={handleKeyDown}
          className="flex-1 bg-transparent text-green-400 outline-none"
          placeholder={connected ? '输入命令...' : '未连接'}
          disabled={!connected}
        />
      </div>
    </div>
  );
}
