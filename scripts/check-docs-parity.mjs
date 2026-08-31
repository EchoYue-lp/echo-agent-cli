import { existsSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const docsRoot = join(repositoryRoot, 'docs');
const manifestPath = join(docsRoot, 'doc-parity-manifest.json');
const languageRoots = { zh: join(docsRoot, 'zh'), en: join(docsRoot, 'en') };

function markdownFilePaths(root) {
  if (!existsSync(root)) return [];
  return readdirSync(root, { withFileTypes: true })
    .flatMap((entry) => {
      const path = join(root, entry.name);
      return entry.isDirectory() ? markdownFilePaths(path) : [path];
    })
    .filter((path) => path.endsWith('.md'));
}

function markdownFiles(root) {
  return markdownFilePaths(root)
    .map((path) => relative(root, path).replaceAll('\\', '/'))
    .sort();
}

function readText(path) {
  return readFileSync(path, 'utf8');
}

function heading(text) {
  return text.match(/^#\s+(.+)$/m)?.[1]?.trim() ?? '';
}

function adrIdentity(text) {
  const title = heading(text);
  const number = title.match(/\bADR[- ]?(\d{4})\b/i)?.[1] ?? '';
  const statusValue =
    text.match(/^(?:>|-\s*)?\s*(?:status|状态)\s*[:：]\s*(.+)$/im)?.[1]?.trim() ??
    text.match(/^##\s*(?:status|状态)\s*\n+\s*(?:>|-\s*)?(.+)$/im)?.[1]?.trim() ??
    '';
  const normalizedStatus = statusValue.toLowerCase();
  const status = normalizedStatus.includes('proposed')
    ? 'proposed'
    : normalizedStatus.includes('reference snapshot')
      ? 'reference snapshot'
      : normalizedStatus.includes('accepted') || normalizedStatus.includes('采纳')
        ? 'accepted'
        : normalizedStatus;
  const superseded =
    text.match(/^(?:>|-\s*)?\s*(?:superseded[- ]by|被替代于?)\s*[:：]\s*(.+)$/im)?.[1]?.trim() ?? '';
  return { number, status, superseded };
}

function hasHan(text) {
  return /\p{Script=Han}/u.test(text);
}

function hasLatin(text) {
  return /[A-Za-z]/.test(text);
}

function prose(text) {
  return text
    .replace(/```[\s\S]*?```/g, '')
    .replace(/`[^`]*`/g, '')
    .replace(/!?\[[^\]]*\]\([^)]*\)/g, '')
    .replace(/https?:\/\/\S+/g, '');
}

function generatedEntries() {
  const entries = [];
  for (const legacyPath of markdownFiles(docsRoot).filter(
    (path) => !path.startsWith('zh/') && !path.startsWith('en/') && path !== 'doc-parity-manifest.json',
  )) {
    const legacyText = readText(join(docsRoot, legacyPath));
    let targetPath = legacyPath;
    let kind = 'reference';
    if (legacyPath === 'README.md') {
      kind = 'index';
    } else if (legacyPath === 'MASTER-PLAN.md') {
      targetPath = 'project-status.md';
      kind = 'project-status';
    } else if (legacyPath === 'architecture.md') {
      targetPath = 'architecture/overview.md';
      kind = 'architecture';
    } else if (legacyPath === 'persistence.md') {
      targetPath = 'architecture/persistence.md';
      kind = 'architecture';
    } else if (legacyPath === 'architecture/runtime-task-service.md') {
      targetPath = 'architecture/runtime.md';
      kind = 'architecture';
    } else if (legacyPath === 'architecture/providers.md') {
      kind = 'architecture';
    } else if (legacyPath === 'skill-sync.md') {
      targetPath = 'operations/skill-sync.md';
      kind = 'operations';
    } else if (legacyPath.startsWith('adr/')) {
      kind = 'adr';
    }
    entries.push({
      legacyPath,
      targetPath,
      kind,
      sourceLanguage: hasHan(prose(legacyText)) ? 'zh-mixed' : 'en',
      translationStatus: 'migration_pending',
    });
  }
  return entries;
}

function writeManifest() {
  const manifest = {
    schemaVersion: 1,
    authority:
      'zh is the editorial source; en is publishable only after semantic review. Legacy paths remain until Plan 04 migrates each entry.',
    entries: generatedEntries(),
  };
  writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  console.log(`Wrote ${manifest.entries.length} migration entries to ${relative(repositoryRoot, manifestPath)}`);
}

function checkManifest() {
  const errors = [];
  if (!existsSync(manifestPath)) {
    errors.push(`Missing manifest: ${relative(repositoryRoot, manifestPath)}`);
    return errors;
  }

  let manifest;
  try {
    manifest = JSON.parse(readText(manifestPath));
  } catch (error) {
    errors.push(`Invalid JSON manifest: ${error instanceof Error ? error.message : String(error)}`);
    return errors;
  }
  if (manifest.schemaVersion !== 1 || !Array.isArray(manifest.entries)) {
    errors.push('Manifest must contain schemaVersion=1 and an entries array.');
    return errors;
  }

  const scope = process.argv.includes('--scope')
    ? process.argv[process.argv.indexOf('--scope') + 1]
    : undefined;
  const entries = scope ? manifest.entries.filter((entry) => entry.kind === scope) : manifest.entries;
  const targetPaths = new Set();
  let adrCount = 0;
  for (const entry of entries) {
    if (!entry || typeof entry !== 'object') {
      errors.push('Manifest contains a non-object entry.');
      continue;
    }
    const { legacyPath, targetPath, kind, translationStatus } = entry;
    if (typeof legacyPath !== 'string' || typeof targetPath !== 'string') {
      errors.push('Every manifest entry requires string legacyPath and targetPath.');
      continue;
    }
    if (targetPaths.has(targetPath)) errors.push(`Duplicate targetPath: ${targetPath}`);
    targetPaths.add(targetPath);
    if (kind === 'adr') adrCount += 1;
    const legacyExists = existsSync(join(docsRoot, legacyPath));
    if (entry.legacyRemoved && legacyExists) {
      errors.push(`Legacy source still exists after removal: docs/${legacyPath}`);
    }
    if (!entry.legacyRemoved && !legacyExists) {
      errors.push(`Legacy source is missing: docs/${legacyPath}`);
    }
    if (translationStatus !== 'reviewed') {
      errors.push(`Translation is not reviewed: ${targetPath} (${translationStatus ?? 'missing status'})`);
    }
  }

  const zhPaths = markdownFiles(languageRoots.zh);
  const enPaths = markdownFiles(languageRoots.en);
  if (!existsSync(languageRoots.zh)) errors.push('Missing docs/zh; migrate the first source batch before publishing.');
  if (!existsSync(languageRoots.en)) errors.push('Missing docs/en; reviewed translations are required before publishing.');
  const expected = [...targetPaths].sort();
  for (const path of expected) {
    if (!zhPaths.includes(path)) errors.push(`Missing zh pair: ${path}`);
    if (!enPaths.includes(path)) errors.push(`Missing en pair: ${path}`);
    const zhPath = join(languageRoots.zh, path);
    const enPath = join(languageRoots.en, path);
    if (!existsSync(zhPath) || !existsSync(enPath)) continue;
    const zhText = readText(zhPath);
    const enText = readText(enPath);
    const zhProse = prose(zhText);
    const enProse = prose(enText);
    if (!hasHan(zhProse)) errors.push(`zh page has no Han-language signal: ${path}`);
    if (!hasLatin(enProse) || hasHan(enProse)) errors.push(`en page has an invalid language signal: ${path}`);
    if (/(?:migration_pending|unreviewed|not yet reviewed|pending translation|待翻译|未审阅)/i.test(enText)) {
      errors.push(`en page contains an unreviewed marker: ${path}`);
    }
    if (path.startsWith('adr/')) {
      const zhAdr = adrIdentity(zhText);
      const enAdr = adrIdentity(enText);
      if (!zhAdr.number || zhAdr.number !== enAdr.number) errors.push(`ADR number mismatch: ${path}`);
      if (zhAdr.status !== enAdr.status || zhAdr.superseded !== enAdr.superseded) {
        errors.push(`ADR status/superseded mismatch: ${path}`);
      }
    }
  }
  for (const path of zhPaths) {
    if (scope && !targetPaths.has(path)) continue;
    if (!targetPaths.has(path)) errors.push(`Unexpected zh page: ${path}`);
  }
  for (const path of enPaths) {
    if (scope && !targetPaths.has(path)) continue;
    if (!targetPaths.has(path)) errors.push(`Unexpected en page: ${path}`);
  }
  if (errors.length === 0) {
    console.log(`Docs parity passed${scope ? ` for ${scope}` : ''}: ${expected.length} pairs, ${adrCount} ADRs.`);
  }
  return errors;
}

if (process.argv.includes('--write-manifest')) {
  writeManifest();
} else {
  const errors = checkManifest();
  if (errors.length > 0) {
    console.error(`Docs parity blocked with ${errors.length} issue(s):`);
    for (const error of errors) console.error(`- ${error}`);
    process.exitCode = 1;
  }
}
