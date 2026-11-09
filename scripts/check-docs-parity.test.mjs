import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, copyFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import test from 'node:test';

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const artifactRoot = join(scriptDirectory, '..', '.echo-semantic', 'local', 'docs-index-tests');
mkdirSync(artifactRoot, { recursive: true });

function fixture() {
  const root = mkdtempSync(join(artifactRoot, 'case-'));
  mkdirSync(join(root, 'scripts'));
  copyFileSync(
    join(scriptDirectory, 'check-docs-parity.mjs'),
    join(root, 'scripts/check-docs-parity.mjs')
  );
  const manifest = {
    schemaVersion: 2,
    authority: '中文正式文档是主维护来源；英语只在明确登记为reviewed时检查。',
    entries: [
      {
        targetPath: 'README.md',
        kind: 'index',
        sourceLanguage: 'zh',
        translationStatus: 'not_required',
      },
    ],
    engineeringEntries: [],
  };
  const put = (path, text) => {
    mkdirSync(dirname(join(root, path)), { recursive: true });
    writeFileSync(join(root, path), text);
  };
  const save = () => put('docs/doc-parity-manifest.json', JSON.stringify(manifest, null, 2) + '\n');
  put('docs/zh/README.md', '# 中文文档\n\n这是产品说明。\n');
  save();
  const run = (...args) => {
    const result = spawnSync(process.execPath, ['scripts/check-docs-parity.mjs', ...args], {
      cwd: root,
      encoding: 'utf8',
    });
    return { status: result.status, text: result.stdout + result.stderr };
  };
  return { root, manifest, put, save, run };
}

test('Chinese-only formal pages and an explicitly indexed engineering plan need no English replica', () => {
  const f = fixture();
  f.manifest.entries.push({
    targetPath: 'adr/0057-native.md',
    kind: 'adr',
    sourceLanguage: 'zh',
    translationStatus: 'not_required',
  });
  f.manifest.engineeringEntries.push({ path: 'MASTER-PLAN.md', kind: 'project-plan' });
  f.put('docs/zh/adr/0057-native.md', '# ADR 0057：原生消息\n\n## 状态\n\n已接受\n');
  f.put('docs/MASTER-PLAN.md', '# 当前工程计划\n\n本轮待办与验收，不是长期产品说明。\n');
  f.save();
  const result = f.run();
  assert.equal(result.status, 0, result.text);
});

test('a new engineering MASTER-PLAN path can coexist with the original migration record', () => {
  const f = fixture();
  f.manifest.entries.push({
    legacyPath: 'MASTER-PLAN.md',
    legacyRemoved: true,
    targetPath: 'project-status.md',
    kind: 'project-status',
    sourceLanguage: 'zh',
    translationStatus: 'not_required',
  });
  f.manifest.engineeringEntries.push({ path: 'MASTER-PLAN.md', kind: 'project-plan' });
  f.put('docs/zh/project-status.md', '# 产品状态\n\n正式状态边界。\n');
  f.put('docs/MASTER-PLAN.md', '# 本轮集成计划\n\n尚未完成的工作与证据。\n');
  f.save();
  assert.equal(f.run().status, 0);
  f.manifest.engineeringEntries = [];
  f.save();
  assert.equal(
    f.run().status,
    1,
    'removing engineering ownership must restore the real legacy-path failure'
  );
});

test('missing and unregistered Chinese pages remain blocked, including when a scope is requested', () => {
  const f = fixture();
  f.manifest.entries.push({
    targetPath: 'missing.md',
    kind: 'reference',
    translationStatus: 'not_required',
  });
  f.put('docs/zh/adr/0061-tools.md', '# ADR 0061：工具分类\n\n状态：已接受\n');
  f.save();
  const result = f.run('--scope', 'adr');
  assert.equal(result.status, 1);
  assert.match(result.text, /Missing zh page: missing\.md/);
  assert.match(result.text, /Unregistered zh page: adr\/0061-tools\.md/);
});

test('duplicate target paths and invalid kinds are rejected before scoped checks', () => {
  const f = fixture();
  f.manifest.entries.push({ ...f.manifest.entries[0] });
  f.manifest.entries.push({
    targetPath: 'adr/0061-tools.md',
    kind: 'reference',
    translationStatus: 'not_required',
  });
  f.put('docs/zh/adr/0061-tools.md', '# ADR 0061：工具\n\n状态：已接受\n');
  f.save();
  const result = f.run('--scope', 'adr');
  assert.equal(result.status, 1);
  assert.match(result.text, /Duplicate targetPath/);
  assert.match(result.text, /ADR page must use kind=adr/);
});

test('reviewed English entries retain language, review-marker and ADR identity checks', () => {
  const f = fixture();
  f.manifest.entries.push({
    targetPath: 'adr/0057-native.md',
    kind: 'adr',
    translationStatus: 'reviewed',
  });
  f.put('docs/zh/adr/0057-native.md', '# ADR 0057：原生消息\n\n状态：已接受\n');
  f.put('docs/en/adr/0057-native.md', '# ADR 0057: Native messages\n\nStatus: Accepted\n');
  f.save();
  assert.equal(f.run().status, 0);
  f.put(
    'docs/en/adr/0057-native.md',
    '# ADR 0058: Native messages\n\nStatus: Proposed\n\nnot yet reviewed\n'
  );
  const result = f.run();
  assert.equal(result.status, 1);
  assert.match(result.text, /ADR number mismatch/);
  assert.match(result.text, /ADR status\/superseded mismatch/);
  assert.match(result.text, /unreviewed marker/);
});

test('an undeclared or missing English publication cannot be labelled reviewed', () => {
  const f = fixture();
  f.put('docs/en/README.md', '# Documentation\n');
  assert.equal(f.run().status, 1);
  f.manifest.entries[0].translationStatus = 'reviewed';
  f.save();
  assert.equal(f.run().status, 0);
  f.manifest.entries.push({ targetPath: 'guide.md', kind: 'guide', translationStatus: 'reviewed' });
  f.put('docs/zh/guide.md', '# 操作指南\n\n正式操作。\n');
  f.save();
  assert.match(f.run().text, /Missing reviewed en page: guide.md/);
});

test('Chinese ADR filename identity and language remain checked independently of translations', () => {
  const f = fixture();
  f.manifest.entries.push({
    targetPath: 'adr/0061-tools.md',
    kind: 'adr',
    translationStatus: 'not_required',
  });
  f.put('docs/zh/adr/0061-tools.md', '# ADR 0057: Tools\n\nEnglish prose only.\n');
  f.save();
  const result = f.run();
  assert.equal(result.status, 1);
  assert.match(result.text, /ADR filename\/heading mismatch/);
  assert.match(result.text, /no Han-language signal/);
});

test('engineering registration must name a real plan outside the formal language trees', () => {
  const f = fixture();
  f.manifest.engineeringEntries = [
    { path: 'missing-plan.md', kind: 'project-plan' },
    { path: 'zh/README.md', kind: 'project-plan' },
  ];
  f.save();
  const result = f.run();
  assert.equal(result.status, 1);
  assert.match(result.text, /Missing engineering document/);
  assert.match(result.text, /Engineering document overlaps a language tree/);
});

test('path traversal, non-object entries and malformed manifests cannot hide coverage', () => {
  const f = fixture();
  f.manifest.entries.push(
    { targetPath: '../outside.md', kind: 'reference', translationStatus: 'not_required' },
    null
  );
  f.save();
  assert.equal(f.run().status, 1);
  f.put('docs/doc-parity-manifest.json', '{broken');
  assert.match(f.run().text, /Invalid JSON manifest/);
  f.put('docs/doc-parity-manifest.json', JSON.stringify({ schemaVersion: 999, entries: [] }));
  assert.equal(f.run().status, 1);
});

test('unknown, empty or conflicting command options fail instead of reporting an empty green scope', () => {
  const f = fixture();
  for (const args of [
    ['--unknown'],
    ['--scope'],
    ['--scope', 'garbage'],
    ['--scope', 'adr', '--scope', 'guide'],
    ['--write-manifest', '--scope', 'adr'],
  ]) {
    const result = f.run(...args);
    assert.equal(result.status, 1, `${args.join(' ')}: ${result.text}`);
  }
});

test('write-manifest appends current Chinese pages without inventing a legacy source or English review', () => {
  const f = fixture();
  f.manifest.entries[0].legacyPath = 'README.md';
  f.manifest.entries[0].legacyRemoved = false;
  f.put('docs/README.md', '# 文档入口\n');
  f.put('docs/zh/guides/new.md', '# 新操作指南\n\n当前功能。\n');
  f.save();
  const result = f.run('--write-manifest');
  assert.equal(result.status, 0, result.text);
  const written = JSON.parse(readFileSync(join(f.root, 'docs/doc-parity-manifest.json'), 'utf8'));
  assert.equal(
    written.entries.find((entry) => entry.targetPath === 'README.md').legacyRemoved,
    false
  );
  const added = written.entries.find((entry) => entry.targetPath === 'guides/new.md');
  assert.equal(added.translationStatus, 'not_required');
  assert.equal(added.legacyPath, undefined);
  assert.equal(f.run().status, 0);
});

test('invalid migration metadata or review state still blocks and write cannot erase the rejection', () => {
  const f = fixture();
  f.manifest.entries[0].legacyPath = 'README.md';
  f.manifest.entries[0].legacyRemoved = 'true';
  f.manifest.entries[0].translationStatus = 'pending';
  f.save();
  const original = readFileSync(join(f.root, 'docs/doc-parity-manifest.json'), 'utf8');
  const result = f.run('--write-manifest');
  assert.equal(result.status, 1);
  assert.match(result.text, /boolean legacyRemoved/);
  assert.match(result.text, /Invalid translationStatus/);
  assert.equal(readFileSync(join(f.root, 'docs/doc-parity-manifest.json'), 'utf8'), original);
});

test('index refresh never removes a missing registered page or demotes an existing reviewed translation', () => {
  const f = fixture();
  f.manifest.entries.push({
    targetPath: 'missing.md',
    kind: 'reference',
    translationStatus: 'reviewed',
  });
  f.save();
  const result = f.run('--write-manifest');
  assert.equal(result.status, 1);
  assert.match(result.text, /Missing zh page: missing.md/);
  const written = JSON.parse(readFileSync(join(f.root, 'docs/doc-parity-manifest.json'), 'utf8'));
  assert.equal(
    written.entries.find((entry) => entry.targetPath === 'missing.md').translationStatus,
    'reviewed'
  );
});
