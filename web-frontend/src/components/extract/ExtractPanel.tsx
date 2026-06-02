import { useEffect, useState } from 'react';
import { FileJson, Play, CheckCircle, AlertCircle } from 'lucide-react';
import { extractApi } from '../../api/endpoints';
import type { ExtractExample, ValidateSchemaResponse } from '../../types/api';

const DEFAULT_SCHEMA = JSON.stringify(
  {
    type: 'object',
    properties: {
      name: { type: 'string', description: 'The name' },
      value: { type: 'number', description: 'The value' },
    },
    required: ['name'],
  },
  null,
  2
);

export function ExtractPanel() {
  const [input, setInput] = useState('');
  const [schema, setSchema] = useState(DEFAULT_SCHEMA);
  const [schemaName, setSchemaName] = useState('');
  const [result, setResult] = useState<unknown>(null);
  const [validation, setValidation] = useState<ValidateSchemaResponse | null>(null);
  const [examples, setExamples] = useState<ExtractExample[]>([]);
  const [extracting, setExtracting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    extractApi.getExamples().then(setExamples).catch(console.error);
  }, []);

  const extract = async () => {
    if (!input.trim() || !schema.trim()) return;
    setExtracting(true);
    setError(null);
    setResult(null);
    try {
      const parsedSchema = JSON.parse(schema);
      const res = await extractApi.extract(input, parsedSchema, schemaName || undefined);
      setResult(res.data);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : '提取失败');
    }
    setExtracting(false);
  };

  const validate = async () => {
    try {
      const parsedSchema = JSON.parse(schema);
      const res = await extractApi.validateSchema(parsedSchema);
      setValidation(res);
    } catch (e: unknown) {
      setValidation({ valid: false, errors: [e instanceof Error ? e.message : 'Invalid JSON'] });
    }
  };

  const loadExample = (ex: ExtractExample) => {
    setSchema(JSON.stringify(ex.schema, null, 2));
    setInput(ex.example_input);
    setSchemaName(ex.name);
  };

  return (
    <div className="p-3 space-y-3">
      <h3 className="text-sm font-semibold" style={{ color: 'var(--text-primary)' }}>
        结构化提取
      </h3>

      {/* Examples */}
      {examples.length > 0 && (
        <div>
          <span
            className="text-[10px] font-medium uppercase tracking-wider"
            style={{ color: 'var(--text-tertiary)' }}
          >
            示例
          </span>
          <div className="mt-1 flex flex-wrap gap-1">
            {examples.map((ex, i) => (
              <button
                key={i}
                onClick={() => loadExample(ex)}
                className="rounded-md px-2 py-0.5 text-[10px] transition-colors"
                style={{ background: 'var(--bg-hover)', color: 'var(--text-secondary)' }}
              >
                {ex.name}
              </button>
            ))}
          </div>
        </div>
      )}

      {/* Input */}
      <div>
        <label
          className="text-[10px] font-medium uppercase tracking-wider"
          style={{ color: 'var(--text-tertiary)' }}
        >
          输入文本
        </label>
        <textarea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          placeholder="输入自然语言文本以提取结构化数据..."
          rows={3}
          className="mt-1 w-full rounded-lg border px-3 py-2 text-xs"
          style={{
            background: 'var(--bg-input)',
            borderColor: 'var(--border-primary)',
            color: 'var(--text-primary)',
          }}
        />
      </div>

      {/* Schema Name */}
      <input
        value={schemaName}
        onChange={(e) => setSchemaName(e.target.value)}
        placeholder="模式名称（可选）"
        className="w-full rounded-lg border px-3 py-1.5 text-xs"
        style={{
          background: 'var(--bg-input)',
          borderColor: 'var(--border-primary)',
          color: 'var(--text-primary)',
        }}
      />

      {/* Schema editor */}
      <div>
        <div className="flex items-center justify-between">
          <label
            className="text-[10px] font-medium uppercase tracking-wider"
            style={{ color: 'var(--text-tertiary)' }}
          >
            JSON 模式
          </label>
          <button
            onClick={validate}
            className="flex items-center gap-1 text-[10px]"
            style={{ color: 'var(--accent)' }}
          >
            <FileJson size={10} /> 验证
          </button>
        </div>
        <textarea
          value={schema}
          onChange={(e) => {
            setSchema(e.target.value);
            setValidation(null);
          }}
          rows={8}
          className="mt-1 w-full rounded-lg border px-3 py-2 font-mono text-[11px] leading-relaxed"
          style={{
            background: 'var(--bg-code)',
            borderColor: 'var(--border-primary)',
            color: 'var(--color-code-text)',
          }}
        />
      </div>

      {/* Validation result */}
      {validation && (
        <div
          className="flex items-center gap-2 rounded-lg px-3 py-2 text-xs"
          style={{
            background: validation.valid ? 'var(--color-success-bg)' : 'var(--color-error-bg)',
          }}
        >
          {validation.valid ? (
            <>
              <CheckCircle size={14} style={{ color: 'var(--color-success)' }} />{' '}
              <span style={{ color: 'var(--color-success)' }}>模式有效</span>
            </>
          ) : (
            <>
              <AlertCircle size={14} style={{ color: 'var(--color-error)' }} />{' '}
              <span style={{ color: 'var(--color-error)' }}>{validation.errors.join(', ')}</span>
            </>
          )}
        </div>
      )}

      {/* Extract button */}
      <button
        onClick={extract}
        disabled={extracting || !input.trim() || !schema.trim()}
        className="flex w-full items-center justify-center gap-2 rounded-lg py-2.5 text-xs font-medium transition-colors"
        style={{
          background:
            extracting || !input.trim() || !schema.trim()
              ? 'var(--border-primary)'
              : 'var(--accent)',
          color: extracting || !input.trim() || !schema.trim() ? 'var(--text-tertiary)' : 'white',
        }}
      >
        {extracting ? (
          <>
            <div className="spinner" /> 提取中...
          </>
        ) : (
          <>
            <Play size={12} /> 提取
          </>
        )}
      </button>

      {/* Error */}
      {error && (
        <div
          className="rounded-lg border-l-[3px] p-3 text-xs"
          style={{ borderColor: 'var(--color-error)', background: 'var(--color-error-bg)' }}
        >
          <p style={{ color: 'var(--color-error)' }}>{error}</p>
        </div>
      )}

      {/* Result */}
      {result !== null && (
        <div>
          <span
            className="text-[10px] font-medium uppercase tracking-wider"
            style={{ color: 'var(--text-tertiary)' }}
          >
            提取的数据
          </span>
          <pre
            className="mt-1 max-h-64 overflow-auto rounded-lg p-3 text-[11px] leading-relaxed"
            style={{ background: 'var(--bg-code)', color: 'var(--color-code-text)' }}
          >
            {JSON.stringify(result, null, 2)}
          </pre>
        </div>
      )}
    </div>
  );
}
