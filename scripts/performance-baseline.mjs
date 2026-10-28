#!/usr/bin/env node
// Measurement tooling only. This module never starts Cargo or a benchmark.
import { createHash } from "node:crypto";
import { lstat, readdir, readFile, writeFile } from "node:fs/promises";
import { isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

export const protocol = Object.freeze({
  version: 2,
  conversationCounts: [10, 100, 1000],
  messagesPerConversation: 10,
  runCounts: [10, 100, 1000],
  journalEventCounts: [10000, 100000],
  guiTaskCounts: [1, 10, 50, 100],
  maxGraphDepth: 10,
  memoryEntryCounts: [10, 100, 1000],
  memoryHistoryCounts: [0, 1000, 10000],
  nativeHistory: Object.freeze({
    eligibleMessageCounts: [1, 2],
    journalEventCounts: [4097, 8193],
    eventArgumentValues: [0, 4097, 8193],
    defaultRetentionFixtureSelector: 0,
    zeroEventArgument:
      "select actual default retention fixture; observed records remain nonzero and are saved in fixture facts",
    callerMessageCounts: Object.freeze({
      "gui-loader": [1, 2],
      "fork-caller": [2],
      "safe-point-append": [2],
    }),
    checkpointArgumentValues: ["valid", "missing", "tail"],
    safePointMode: "process-cold",
    safePointCheckpoint: "valid",
    alignedHead: 8193,
    replayTailEvents: 4096,
    checkpointStates: ["aligned", "missing", "tail4096", "default-pruned"],
    callers: ["gui-loader", "fork-caller", "safe-point-append"],
    retention: "actual ChatEventRetention::default; no policy override",
    ioCounts:
      "separate logical attempts/successes/failures; not OS syscall counts or latency",
    observer:
      "test-utils private tracing; observer and collector overhead included; does not establish production latency",
    scope:
      "ordinary caller Rust pipeline; no renderer layout/paint or external network",
  }),
  warmup: 5,
  hotSamples: 30,
  processColdSamples: 10,
  successfulMemoryWrites: 20,
  maxWritesPerManager: 50,
  runtimeReplayTailEvents: 512,
  memoryHistoryUnit:
    "completed mutations; operation records=2x, audit entries=1x",
  processResources:
    "child CPU/peak RSS/block-operation counts; never per-operation or physical-byte throughput",
  coldDefinition: "fresh process; OS page cache uncontrolled",
  quantiles: "nearest rank; p99 only when N >= 100",
});

export function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

export function canonicalJson(value) {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value !== null && typeof value === "object") {
    return `{${Object.keys(value)
      .sort((left, right) =>
        Buffer.compare(Buffer.from(left), Buffer.from(right)),
      )
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
}

function within(root, path) {
  const rel = relative(root, path);
  return (
    rel === "" ||
    (!isAbsolute(rel) && rel !== ".." && !rel.startsWith(`..${sep}`))
  );
}

async function entries(root, directory = root) {
  const results = [];
  const names = (await readdir(directory)).sort((left, right) =>
    Buffer.compare(Buffer.from(left), Buffer.from(right)),
  );
  for (const name of names) {
    const path = resolve(directory, name);
    const stat = await lstat(path);
    if (stat.isSymbolicLink())
      throw new Error(`dataset contains symlink: ${path}`);
    if (stat.isDirectory()) {
      results.push(...(await entries(root, path)));
    } else if (stat.isFile()) {
      const bytes = await readFile(path);
      results.push({
        path: relative(root, path).split(sep).join("/"),
        bytes: bytes.length,
        sha256: sha256(bytes),
      });
    } else {
      throw new Error(`dataset contains non-regular entry: ${path}`);
    }
  }
  return results.sort((left, right) =>
    Buffer.compare(Buffer.from(left.path), Buffer.from(right.path)),
  );
}

function manifestIdentity(files) {
  return sha256(canonicalJson(files));
}

export async function freezeDataset(root, output, metadata) {
  root = resolve(root);
  output = resolve(output);
  if (within(root, output))
    throw new Error("manifest must be outside the frozen dataset");
  const stat = await lstat(root);
  if (!stat.isDirectory() || stat.isSymbolicLink())
    throw new Error("dataset root must be a real directory");
  const files = await entries(root);
  if (files.length === 0) throw new Error("cannot freeze an empty dataset");
  const manifest = {
    protocol,
    protocol_sha256: sha256(canonicalJson(protocol)),
    dataset_sha256: manifestIdentity(files),
    // Metadata records source/lock/features/units. It cannot prove a measurement ran.
    metadata,
    files,
  };
  await writeFile(output, `${JSON.stringify(manifest, null, 2)}\n`, {
    flag: "wx",
  });
  return manifest;
}

export async function verifyDataset(root, manifest) {
  root = resolve(root);
  const rootStat = await lstat(root);
  if (!rootStat.isDirectory() || rootStat.isSymbolicLink())
    throw new Error("dataset root must be a real directory");
  if (!Array.isArray(manifest.files) || manifest.files.length === 0)
    throw new Error("manifest files missing");
  if (manifestIdentity(manifest.files) !== manifest.dataset_sha256)
    throw new Error("manifest identity mismatch");
  if (sha256(canonicalJson(manifest.protocol)) !== manifest.protocol_sha256)
    throw new Error("protocol identity mismatch");
  for (const entry of manifest.files) {
    if (
      typeof entry.path !== "string" ||
      entry.path.length === 0 ||
      isAbsolute(entry.path)
    ) {
      throw new Error("manifest path must be relative");
    }
    const path = resolve(root, entry.path);
    if (!within(root, path)) throw new Error("manifest path escapes dataset");
  }
  const actual = await entries(root);
  if (JSON.stringify(actual) !== JSON.stringify(manifest.files))
    throw new Error("dataset bytes or file set changed");
  return {
    dataset_sha256: manifest.dataset_sha256,
    files: actual.length,
    verified: true,
  };
}

function finiteNanoseconds(value) {
  if (typeof value !== "string" || !/^\d+$/.test(value))
    throw new Error("elapsed_ns must be an integer string");
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 0)
    throw new Error("elapsed_ns exceeds safe numeric range");
  return parsed;
}

function nearestRank(sorted, probability) {
  return sorted.at(Math.ceil(sorted.length * probability) - 1);
}

export function summarizeSamples(rows) {
  if (!Array.isArray(rows) || rows.length === 0) throw new Error("no samples");
  const groups = new Map();
  for (const row of rows) {
    if (row.correctness?.ok !== true)
      throw new Error(
        "incorrect or failed operation cannot be a successful sample",
      );
    for (const field of [
      "scenario",
      "dataset_sha256",
      "source_sha256",
      "protocol_sha256",
      "environment_id",
      "mode",
      "outcome",
    ]) {
      if (typeof row[field] !== "string" || row[field].length === 0)
        throw new Error(`sample missing ${field}`);
    }
    if (!["warmup", "measurement"].includes(row.phase))
      throw new Error("unknown sample phase");
    if (!["success", "preview_only", "expected_refusal"].includes(row.outcome))
      throw new Error("unknown operation outcome");
    if (!Number.isSafeInteger(row.sample_index) || row.sample_index < 0)
      throw new Error("invalid sample index");
    const elapsed = finiteNanoseconds(row.elapsed_ns);
    const identity = Object.fromEntries(
      [
        "scenario",
        "dataset_sha256",
        "source_sha256",
        "protocol_sha256",
        "environment_id",
        "mode",
        "outcome",
      ].map((key) => [key, row[key]]),
    );
    const key = JSON.stringify(identity);
    const group = groups.get(key) ?? {
      identity,
      measured: [],
      warmup: 0,
      indices: new Set(),
    };
    const index = `${row.phase}:${row.sample_index}`;
    if (group.indices.has(index)) throw new Error("duplicate sample index");
    group.indices.add(index);
    if (row.phase === "warmup") group.warmup += 1;
    else group.measured.push(elapsed);
    groups.set(key, group);
  }
  return [...groups.values()].map(({ identity, measured, warmup }) => {
    if (measured.length === 0) throw new Error("group has no measured samples");
    measured.sort((left, right) => left - right);
    const middle = Math.floor(measured.length / 2);
    const median =
      measured.length % 2 === 0
        ? measured.at(middle - 1) +
          (measured.at(middle) - measured.at(middle - 1)) / 2
        : measured.at(middle);
    const result = {
      ...identity,
      unit: "ns",
      n: measured.length,
      warmup_n: warmup,
      min: measured.at(0),
      median,
      max: measured.at(-1),
      empirical_quantile_method: "nearest rank",
      uncertainty: "descriptive samples; no confidence interval or product SLA",
      native_layout_paint: "not established by timing rows",
      cpu_rss_io:
        "attach separately scoped actual process observations; not inferred from file sizes",
    };
    if (measured.length >= 20) {
      result.p90 = nearestRank(measured, 0.9);
      result.p95 = nearestRank(measured, 0.95);
    }
    if (measured.length >= 100) result.p99 = nearestRank(measured, 0.99);
    return result;
  });
}

async function main() {
  const [command, ...args] = process.argv.slice(2);
  if (command === "protocol" && args.length === 0) {
    console.log(
      JSON.stringify(
        { protocol, protocol_sha256: sha256(canonicalJson(protocol)) },
        null,
        2,
      ),
    );
  } else if (command === "freeze" && args.length === 3) {
    console.log(
      JSON.stringify(
        await freezeDataset(
          args[0],
          args[1],
          JSON.parse(await readFile(args[2], "utf8")),
        ),
      ),
    );
  } else if (command === "verify" && args.length === 2) {
    console.log(
      JSON.stringify(
        await verifyDataset(
          args[0],
          JSON.parse(await readFile(args[1], "utf8")),
        ),
      ),
    );
  } else if (command === "summarize" && args.length === 2) {
    const sampleBytes = await readFile(args[0]);
    const rows = sampleBytes
      .toString("utf8")
      .split("\n")
      .filter((line) => line.trim())
      .map((line) => JSON.parse(line));
    await writeFile(
      args[1],
      `${JSON.stringify({ schema_version: 1, samples: { path: resolve(args[0]), sha256: sha256(sampleBytes), rows: rows.length }, groups: summarizeSamples(rows) }, null, 2)}\n`,
      { flag: "wx" },
    );
  } else {
    throw new Error(
      "usage: performance-baseline.mjs protocol | freeze DATASET MANIFEST METADATA.json | verify DATASET MANIFEST | summarize SAMPLES.jsonl OUTPUT.json",
    );
  }
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  main().catch((error) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
