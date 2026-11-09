import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  canonicalJson,
  freezeDataset,
  protocol,
  sha256,
  summarizeSamples,
  verifyDataset,
} from "./performance-baseline.mjs";

async function fixture(context) {
  const root = await mkdtemp(join(tmpdir(), "eko-perf-correctness-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  const dataset = join(root, "dataset");
  await mkdir(dataset);
  await writeFile(join(dataset, "messages.json"), "中文 🧪\n");
  return { root, dataset, manifest: join(root, "manifest.json") };
}

test("freeze and verify bind exact UTF-8 bytes and metadata", async (context) => {
  const paths = await fixture(context);
  const manifest = await freezeDataset(paths.dataset, paths.manifest, {
    conversations: 10,
  });
  assert.equal(manifest.files.at(0).bytes, Buffer.byteLength("中文 🧪\n"));
  assert.equal(manifest.files.at(0).sha256, sha256("中文 🧪\n"));
  assert.equal((await verifyDataset(paths.dataset, manifest)).verified, true);
  await writeFile(join(paths.dataset, "messages.json"), "中文 🧪 changed\n");
  await assert.rejects(
    verifyDataset(paths.dataset, manifest),
    /bytes or file set/,
  );
});

test("file-set changes, symlinks, empty sets and in-dataset manifests are rejected", async (context) => {
  const paths = await fixture(context);
  const manifest = await freezeDataset(paths.dataset, paths.manifest, {});
  await writeFile(join(paths.dataset, "extra.json"), "{}");
  await assert.rejects(verifyDataset(paths.dataset, manifest), /file set/);
  await assert.rejects(
    freezeDataset(paths.dataset, join(paths.dataset, "manifest.json"), {}),
    /outside/,
  );
  await symlink(paths.manifest, join(paths.dataset, "link"));
  await assert.rejects(verifyDataset(paths.dataset, manifest), /symlink/);
  const empty = join(paths.root, "empty");
  await mkdir(empty);
  await assert.rejects(
    freezeDataset(empty, join(paths.root, "empty-manifest.json"), {}),
    /empty/,
  );
});

test("freeze preserves an existing manifest and verification rejects identity tampering", async (context) => {
  const paths = await fixture(context);
  const manifest = await freezeDataset(paths.dataset, paths.manifest, {});
  await assert.rejects(freezeDataset(paths.dataset, paths.manifest, {}), {
    code: "EEXIST",
  });
  manifest.files.at(0).path = "../outside";
  await assert.rejects(verifyDataset(paths.dataset, manifest), /identity/);
});

function rows(count) {
  return Array.from({ length: count }, (_, sample_index) => ({
    scenario: "correctness-only-statistics-fixture",
    dataset_sha256: "synthetic-unit-test",
    source_sha256: "synthetic-unit-test",
    protocol_sha256: "synthetic-unit-test",
    environment_id: "node-test-no-performance-claim",
    mode: "unit-test",
    outcome: "success",
    phase: "measurement",
    sample_index,
    elapsed_ns: String(sample_index + 1),
    correctness: { ok: true },
  }));
}

test("raw sample count and descriptive quantiles never invent a small-sample p99", () => {
  const summary = summarizeSamples(rows(30)).at(0);
  assert.deepEqual(
    [summary.n, summary.min, summary.median, summary.max],
    [30, 1, 15.5, 30],
  );
  assert.equal(summary.p95, 29);
  assert.equal(Object.hasOwn(summary, "p99"), false);
  assert.equal(summarizeSamples(rows(100)).at(0).p99, 99);
  assert.equal(Object.hasOwn(summarizeSamples(rows(10)).at(0), "p95"), false);
});

test("warmup is separate, identities cannot be pooled, failures cannot look fast", () => {
  const samples = rows(30);
  const warmup = rows(5).map((row) => ({ ...row, phase: "warmup" }));
  assert.equal(summarizeSamples([...warmup, ...samples]).at(0).warmup_n, 5);
  const other = rows(1).map((row) => ({ ...row, dataset_sha256: "other" }));
  assert.equal(summarizeSamples([...samples, ...other]).length, 2);
  assert.throws(
    () => summarizeSamples([{ ...samples.at(0), correctness: { ok: false } }]),
    /failed operation/,
  );
  assert.throws(
    () => summarizeSamples([...samples, samples.at(0)]),
    /duplicate/,
  );
  assert.throws(
    () => summarizeSamples([{ ...samples.at(0), elapsed_ns: "NaN" }]),
    /integer string/,
  );
  assert.throws(
    () =>
      summarizeSamples([{ ...samples.at(0), elapsed_ns: "9007199254740992" }]),
    /safe numeric/,
  );
  assert.throws(
    () => summarizeSamples([{ ...samples.at(0), mode: "" }]),
    /missing mode/,
  );
});

test("protocol preserves distinct workload units and default memory guard budget", () => {
  assert.equal(protocol.version, 2);
  assert.deepEqual(protocol.conversationCounts, [10, 100, 1000]);
  assert.equal(protocol.messagesPerConversation, 10);
  assert.deepEqual(protocol.runCounts, [10, 100, 1000]);
  assert.deepEqual(protocol.guiTaskCounts, [1, 10, 50, 100]);
  assert.deepEqual(protocol.journalEventCounts, [10000, 100000]);
  assert.deepEqual(protocol.memoryEntryCounts, [10, 100, 1000]);
  assert.deepEqual(protocol.memoryHistoryCounts, [0, 1000, 10000]);
  assert.deepEqual(protocol.nativeHistory.eligibleMessageCounts, [1, 2]);
  assert.deepEqual(protocol.nativeHistory.journalEventCounts, [4097, 8193]);
  assert.deepEqual(protocol.nativeHistory.eventArgumentValues, [0, 4097, 8193]);
  assert.equal(protocol.nativeHistory.defaultRetentionFixtureSelector, 0);
  assert.deepEqual(protocol.nativeHistory.callerMessageCounts, {
    "gui-loader": [1, 2],
    "fork-caller": [2],
    "safe-point-append": [2],
  });
  assert.deepEqual(protocol.nativeHistory.checkpointArgumentValues, [
    "valid",
    "missing",
    "tail",
  ]);
  assert.equal(protocol.nativeHistory.safePointMode, "process-cold");
  assert.equal(protocol.nativeHistory.safePointCheckpoint, "valid");
  assert.match(protocol.nativeHistory.observer, /overhead included/);
  assert.equal(protocol.nativeHistory.alignedHead, 8193);
  assert.equal(protocol.nativeHistory.replayTailEvents, 4096);
  assert.deepEqual(protocol.nativeHistory.checkpointStates, [
    "aligned",
    "missing",
    "tail4096",
    "default-pruned",
  ]);
  assert.deepEqual(protocol.nativeHistory.callers, [
    "gui-loader",
    "fork-caller",
    "safe-point-append",
  ]);
  assert.equal(protocol.hotSamples, 30);
  assert.equal(protocol.processColdSamples, 10);
  assert.equal(
    protocol.successfulMemoryWrites + protocol.warmup <=
      protocol.maxWritesPerManager,
    true,
  );
});

test("canonical bytes match the Python UTF-8 fixture contract", () => {
  assert.equal(
    canonicalJson({ b: [2, 1], a: "中文 🧪" }),
    '{"a":"中文 🧪","b":[2,1]}',
  );
  assert.equal(canonicalJson({ "🧪": 1, "\uE000": 2 }), '{"\uE000":2,"🧪":1}');
});

test("refusal and partial preview cannot be pooled into complete-success timing", () => {
  const sample = rows(1).at(0);
  const groups = summarizeSamples(
    ["success", "preview_only", "expected_refusal"].map((outcome) => ({
      ...sample,
      outcome,
    })),
  );
  assert.equal(groups.length, 3);
  assert.deepEqual(
    groups.map((group) => group.outcome),
    ["success", "preview_only", "expected_refusal"],
  );
  assert.throws(
    () => summarizeSamples([{ ...sample, outcome: "fast-but-unknown" }]),
    /outcome/,
  );
});
