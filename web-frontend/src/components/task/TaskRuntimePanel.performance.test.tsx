/// <reference types="node" />
// @vitest-environment jsdom
// Opt-in real-component observations. The input is exported by the Rust
// TaskRevisionService harness, never an unvalidated oversized setState graph.
import { createHash } from 'node:crypto';
import { appendFileSync, readFileSync } from 'node:fs';
import { Profiler, type ProfilerOnRenderCallback } from 'react';
import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { PlanRevision, TaskRun, TodoItem } from '../../generated';
import { useTaskRuntimeStore } from '../../stores/taskRuntimeStore';
import { useSubagentRunStore } from '../../stores/subagentRunStore';
import { TaskRuntimePanel } from './TaskRuntimePanel';

type Snapshot = { stage: string; run: TaskRun; plan: PlanRevision; todos: TodoItem[] };
type Fixture = {
  qualification: string;
  objects: number;
  graph_depth: number;
  snapshots: Snapshot[];
};

const fixturePath = process.env.EKO_PERF_GUI_FIXTURE;
let previousRootClass: string | null;
let previousWidth: PropertyDescriptor | undefined;
let previousHeight: PropertyDescriptor | undefined;

function required(name: string): string {
  const value = process.env[name];
  if (!value?.trim()) throw new Error(`${name} is required`);
  return value;
}

function loadFixture(): Fixture {
  const bytes = readFileSync(required('EKO_PERF_GUI_FIXTURE'));
  expect(createHash('sha256').update(bytes).digest('hex')).toBe(
    required('EKO_PERF_GUI_FIXTURE_SHA256')
  );
  const fixture = JSON.parse(bytes.toString('utf8')) as Fixture;
  expect(fixture.qualification).toBe('commit_task_plan + TaskRevisionService.apply_patch');
  expect([1, 10, 50, 100]).toContain(fixture.objects);
  expect(fixture.graph_depth).toBe(1);
  expect(fixture.snapshots.map((snapshot) => snapshot.stage)).toEqual([
    'initial',
    'single-row',
    'batch',
    'run-state',
  ]);
  for (const [index, snapshot] of fixture.snapshots.entries()) {
    expect(snapshot.plan.revision).toBe(Math.min(index + 1, 3));
    expect(snapshot.plan.run_id).toBe(snapshot.run.run_id);
    expect(snapshot.plan.goal_sha256).toBe(snapshot.run.goal_sha256);
    expect(snapshot.plan.goal_revision).toBe(snapshot.run.goal_revision);
    expect(snapshot.plan.tasks.length).toBe(fixture.objects);
    expect(snapshot.run.status).toBe(index === 3 ? 'running' : 'paused');
    expect(snapshot.todos.length).toBe(fixture.objects);
    expect(new Set(snapshot.plan.tasks.map((task) => task.id)).size).toBe(fixture.objects);
    for (const task of snapshot.plan.tasks) {
      const todo = snapshot.todos.find((candidate) => candidate.task_id === task.id);
      expect(todo?.title).toBe(task.title);
      expect(todo?.run_id).toBe(snapshot.run.run_id);
    }
  }
  return fixture;
}

function publish(snapshot: Snapshot): void {
  useTaskRuntimeStore.setState({
    activeRun: snapshot.run,
    plan: snapshot.plan,
    todos: snapshot.todos,
    artifacts: [],
    events: [],
    recoveryBlockers: [],
    continuation: null,
    backgroundCells: [],
    completionGate: null,
    error: null,
  });
}

beforeEach(() => {
  previousRootClass = document.documentElement.getAttribute('class');
  previousWidth = Object.getOwnPropertyDescriptor(window, 'innerWidth');
  previousHeight = Object.getOwnPropertyDescriptor(window, 'innerHeight');
});

afterEach(() => {
  cleanup();
  useTaskRuntimeStore.getState().reset();
  useSubagentRunStore.setState({ runs: {} });
  if (previousRootClass === null) document.documentElement.removeAttribute('class');
  else document.documentElement.setAttribute('class', previousRootClass);
  if (previousWidth) Object.defineProperty(window, 'innerWidth', previousWidth);
  else Reflect.deleteProperty(window, 'innerWidth');
  if (previousHeight) Object.defineProperty(window, 'innerHeight', previousHeight);
  else Reflect.deleteProperty(window, 'innerHeight');
});

describe.runIf(Boolean(fixturePath))('TaskRuntimePanel qualified performance observations', () => {
  for (const operation of ['mount', 'single-row', 'batch', 'run-state'] as const) {
    it(`records ${operation} of a Rust-accepted graph with matching DOM`, () => {
      const fixture = loadFixture();
      const initial = fixture.snapshots.find(
        (snapshot) =>
          snapshot.stage ===
          (operation === 'batch' ? 'single-row' : operation === 'run-state' ? 'batch' : 'initial')
      );
      const updated = fixture.snapshots.find((snapshot) => snapshot.stage === operation);
      if (!initial || (operation !== 'mount' && !updated)) {
        throw new Error('qualified snapshot missing');
      }
      document.documentElement.classList.add('dark');
      Object.defineProperty(window, 'innerWidth', { configurable: true, value: 1400 });
      Object.defineProperty(window, 'innerHeight', { configurable: true, value: 900 });
      for (let ordinal = 0; ordinal < 35; ordinal += 1) {
        cleanup();
        useSubagentRunStore.setState({ runs: {} });
        publish(initial);
        const commits: { phase: string; actualDurationMs: number; baseDurationMs: number }[] = [];
        const onRender: ProfilerOnRenderCallback = (_id, phase, actualDuration, baseDuration) => {
          commits.push({ phase, actualDurationMs: actualDuration, baseDurationMs: baseDuration });
        };
        const element = (
          <Profiler id="TaskRuntimePanel" onRender={onRender}>
            <TaskRuntimePanel />
          </Profiler>
        );
        if (operation !== 'mount') {
          render(element);
          commits.length = 0;
        }
        const started = performance.now();
        if (operation === 'mount') render(element);
        else if (updated) act(() => publish(updated));
        const elapsedMs = performance.now() - started;
        const rendered = operation === 'mount' ? initial : updated;
        if (!rendered) throw new Error('rendered snapshot missing');
        expect(screen.getByText(rendered.run.goal)).toBeTruthy();
        expect(
          screen.getByText(rendered.run.status === 'running' ? '执行中' : '已暂停')
        ).toBeTruthy();
        for (const todo of rendered.todos) {
          expect(screen.getByTitle(todo.title)).toBeTruthy();
        }
        expect(commits.length).toBeGreaterThan(0);
        appendFileSync(
          required('EKO_PERF_SAMPLES'),
          `${JSON.stringify({
            scenario: `gui/TaskRuntimePanel/${operation}`,
            dataset_sha256: required('EKO_PERF_DATASET_SHA256'),
            source_sha256: required('EKO_PERF_SOURCE_SHA256'),
            protocol_sha256: required('EKO_PERF_PROTOCOL_SHA256'),
            environment_id: required('EKO_PERF_ENVIRONMENT_ID'),
            mode: 'jsdom/react-test-development',
            outcome: 'success',
            process_observation_id: required('EKO_PERF_PROCESS_ID'),
            phase: ordinal < 5 ? 'warmup' : 'measurement',
            sample_index: ordinal < 5 ? ordinal : ordinal - 5,
            elapsed_ns: String(Math.round(elapsedMs * 1_000_000)),
            profiler_commits: commits,
            dom_elements: document.querySelectorAll('*').length,
            correctness: {
              ok: true,
              tasks: fixture.objects,
              plan_revision: rendered.plan.revision,
            },
            clock: 'performance.now; milliseconds converted to ns, not ns precision',
            viewport: { width: 1400, height: 900 },
            theme: 'dark',
            subagent_history: 0,
            native_layout_paint: 'not_measured',
            cpu_rss_io: 'separate whole-process observation; not per render',
            mock_boundary: 'typed stores receive accepted fixture; no backend/LLM/network/polling',
          })}\n`
        );
      }
    });
  }
});
