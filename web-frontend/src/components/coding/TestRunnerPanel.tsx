
interface TestResult {
  name: string;
  status: 'passed' | 'failed' | 'skipped';
  duration?: number;
  error?: string;
}

interface TestRunnerPanelProps {
  results: TestResult[];
  totalTests?: number;
  passedTests?: number;
  failedTests?: number;
  duration?: number;
}

/**
 * TestRunnerPanel - 测试运行结果展示组件
 * Displays test execution results with pass/fail status
 */
export function TestRunnerPanel({
  results,
  totalTests = 0,
  passedTests = 0,
  failedTests = 0,
  duration = 0,
}: TestRunnerPanelProps) {
  const passRate = totalTests > 0 ? Math.round((passedTests / totalTests) * 100) : 0;
  const failRate = totalTests > 0 ? Math.round((failedTests / totalTests) * 100) : 0;

  return (
    <div className="rounded-xl border border-[var(--border-primary)] bg-[var(--bg-primary)] overflow-hidden shadow-sm">
      {/* Summary Header */}
      <div className="px-5 py-4 bg-[var(--bg-secondary)] border-b border-[var(--border-primary)]">
        <div className="flex items-center justify-between mb-3">
          <h3 className="text-sm font-semibold text-[var(--text-primary)]">Test Results</h3>
          <span className="text-xs text-[var(--text-tertiary)]">{duration}ms</span>
        </div>
        <div className="flex items-center gap-4 mb-3">
          <div className="flex-1">
            <div className="flex items-baseline gap-1">
              <span className="text-2xl font-bold text-[var(--text-primary)]">{passRate}%</span>
              <span className="text-xs text-[var(--text-tertiary)]">pass rate</span>
            </div>
          </div>
          <div className="flex items-center gap-4 text-xs">
            <div className="flex items-center gap-1.5">
              <div className="w-2 h-2 rounded-full bg-green-500" />
              <span className="text-[var(--text-secondary)]">{passedTests} passed</span>
            </div>
            <div className="flex items-center gap-1.5">
              <div className="w-2 h-2 rounded-full bg-red-500" />
              <span className="text-[var(--text-secondary)]">{failedTests} failed</span>
            </div>
          </div>
        </div>
        {/* Progress Bar */}
        <div className="flex h-2 rounded-full overflow-hidden bg-[var(--bg-hover)]">
          {passedTests > 0 && (
            <div
              className="bg-green-500 transition-all duration-500"
              style={{ width: `${passRate}%` }}
            />
          )}
          {failedTests > 0 && (
            <div
              className="bg-red-500 transition-all duration-500"
              style={{ width: `${failRate}%` }}
            />
          )}
        </div>
      </div>

      {/* Results List */}
      <div className="max-h-80 overflow-y-auto">
        {results.length === 0 ? (
          <div className="py-8 text-center text-sm text-[var(--text-tertiary)]">
            No tests run yet
          </div>
        ) : (
          results.map((result, index) => (
            <div
              key={index}
              className="flex items-start gap-3 px-5 py-3 border-b border-[var(--border-primary)] last:border-b-0 hover:bg-[var(--bg-hover)] transition-colors"
            >
              <div className="mt-0.5">
                {result.status === 'passed' ? (
                  <div className="w-5 h-5 rounded-full bg-green-100 dark:bg-green-900/30 flex items-center justify-center">
                    <svg className="w-3 h-3 text-green-600 dark:text-green-400" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={3} d="M5 13l4 4L19 7" />
                    </svg>
                  </div>
                ) : result.status === 'failed' ? (
                  <div className="w-5 h-5 rounded-full bg-red-100 dark:bg-red-900/30 flex items-center justify-center">
                    <svg className="w-3 h-3 text-red-600 dark:text-red-400" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={3} d="M6 18L18 6M6 6l12 12" />
                    </svg>
                  </div>
                ) : (
                  <div className="w-5 h-5 rounded-full bg-gray-100 dark:bg-gray-800 flex items-center justify-center">
                    <span className="text-xs text-gray-500 dark:text-gray-400">⊘</span>
                  </div>
                )}
              </div>
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className="text-sm font-medium text-[var(--text-primary)] truncate">
                    {result.name}
                  </span>
                  <span
                    className={`text-xs px-1.5 py-0.5 rounded-md font-medium ${
                      result.status === 'passed'
                        ? 'bg-green-50 text-green-700 dark:bg-green-900/20 dark:text-green-400'
                        : result.status === 'failed'
                        ? 'bg-red-50 text-red-700 dark:bg-red-900/20 dark:text-red-400'
                        : 'bg-gray-100 text-gray-600 dark:bg-gray-800 dark:text-gray-400'
                    }`}
                  >
                    {result.status}
                  </span>
                </div>
                {result.duration && (
                  <span className="text-xs text-[var(--text-tertiary)] mt-0.5 block">
                    {result.duration}ms
                  </span>
                )}
                {result.error && (
                  <div className="mt-2 p-2.5 rounded-lg bg-red-50 dark:bg-red-900/10 border border-red-100 dark:border-red-900/20">
                    <p className="text-xs text-red-700 dark:text-red-300 font-mono leading-relaxed">{result.error}</p>
                  </div>
                )}
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
}

export default TestRunnerPanel;
