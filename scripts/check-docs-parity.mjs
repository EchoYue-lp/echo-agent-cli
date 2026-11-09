import { existsSync, lstatSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
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
      : normalizedStatus.includes('accepted') ||
          normalizedStatus.includes('采纳') ||
          normalizedStatus.includes('已接受')
        ? 'accepted'
        : normalizedStatus;
  const superseded =
    text.match(/^(?:>|-\s*)?\s*(?:superseded[- ]by|被替代于?)\s*[:：]\s*(.+)$/im)?.[1]?.trim() ??
    '';
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

const kinds = new Set([
  'index',
  'reference',
  'architecture',
  'operations',
  'guide',
  'adr',
  'project-status',
]);
const engineeringKinds = new Set(['project-plan', 'handoff', 'audit', 'test-record']);

function parseOptions(args) {
  const options = { scope: undefined, write: false };
  for (let index = 0; index < args.length; index += 1) {
    const value = args[index];
    if (value === '--write-manifest' && !options.write) options.write = true;
    else if (value === '--scope' && options.scope === undefined) {
      const scope = args[++index];
      if (!kinds.has(scope)) throw new Error('--scope requires one registered document kind.');
      options.scope = scope;
    } else throw new Error(`Unknown or repeated option: ${value}`);
  }
  if (options.write && options.scope)
    throw new Error('--write-manifest cannot be combined with --scope.');
  return options;
}

function documentPath(value) {
  return (
    typeof value === 'string' &&
    value.endsWith('.md') &&
    !value.includes('\\') &&
    !value.includes(':') &&
    !value.includes('\0') &&
    value.split('/').every((part) => part && part !== '.' && part !== '..')
  );
}

function regularFile(path) {
  return existsSync(path) && lstatSync(path).isFile() && !lstatSync(path).isSymbolicLink();
}

function readManifest() {
  if (!regularFile(manifestPath))
    throw new Error(`Missing manifest: ${relative(repositoryRoot, manifestPath)}`);
  let manifest;
  try {
    manifest = JSON.parse(readText(manifestPath));
  } catch (error) {
    throw new Error(`Invalid JSON manifest: ${error.message}`);
  }
  if (
    !manifest ||
    manifest.schemaVersion !== 2 ||
    !Array.isArray(manifest.entries) ||
    !Array.isArray(manifest.engineeringEntries) ||
    typeof manifest.authority !== 'string'
  ) {
    throw new Error(
      'Manifest requires schemaVersion=2, authority, entries and engineeringEntries.'
    );
  }
  return manifest;
}

function inspectEntries(manifest, errors) {
  const targetPaths = new Set();
  for (const entry of manifest.entries) {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
      errors.push('Manifest contains a non-object entry.');
      continue;
    }
    const { targetPath, kind, translationStatus } = entry;
    if (!documentPath(targetPath) || targetPath.startsWith('zh/') || targetPath.startsWith('en/')) {
      errors.push(`Invalid targetPath: ${targetPath}`);
      continue;
    }
    if (targetPaths.has(targetPath)) errors.push(`Duplicate targetPath: ${targetPath}`);
    targetPaths.add(targetPath);
    if (!kinds.has(kind)) errors.push(`Invalid document kind: ${kind}`);
    if (targetPath.startsWith('adr/') && kind !== 'adr')
      errors.push(`ADR page must use kind=adr: ${targetPath}`);
    if (!['reviewed', 'not_required'].includes(translationStatus))
      errors.push(`Invalid translationStatus: ${targetPath}`);
    if (
      entry.legacyPath !== undefined &&
      (!documentPath(entry.legacyPath) || typeof entry.legacyRemoved !== 'boolean')
    ) {
      errors.push(
        `Legacy migration record requires a safe legacyPath and boolean legacyRemoved: ${targetPath}`
      );
    }
    if (entry.legacyPath === undefined && entry.legacyRemoved !== undefined) {
      errors.push(`legacyRemoved has no legacyPath: ${targetPath}`);
    }
  }
  return targetPaths;
}

function engineeringPaths(manifest, errors) {
  const paths = new Set();
  for (const entry of manifest.engineeringEntries) {
    if (!entry || !documentPath(entry.path) || !engineeringKinds.has(entry.kind)) {
      errors.push('Invalid engineering document registration.');
      continue;
    }
    if (entry.path.startsWith('zh/') || entry.path.startsWith('en/')) {
      errors.push(`Engineering document overlaps a language tree: ${entry.path}`);
      continue;
    }
    if (paths.has(entry.path)) errors.push(`Duplicate engineering path: ${entry.path}`);
    paths.add(entry.path);
    const path = join(docsRoot, entry.path);
    if (!regularFile(path)) errors.push(`Missing engineering document: ${entry.path}`);
    else if (!heading(readText(path)) || !hasHan(prose(readText(path)))) {
      errors.push(`Engineering document needs a Chinese heading and content: ${entry.path}`);
    }
  }
  return paths;
}

function checkManifest(manifest, scope) {
  const errors = [];
  const targetPaths = inspectEntries(manifest, errors);
  const engineering = engineeringPaths(manifest, errors);
  if (errors.length) return errors;
  const zhPaths = markdownFiles(languageRoots.zh);
  const enPaths = markdownFiles(languageRoots.en);
  if (!existsSync(languageRoots.zh)) errors.push('Missing docs/zh editorial source.');
  // Inventory coverage is global even when a scoped content check is requested.
  for (const path of targetPaths) {
    if (!zhPaths.includes(path) || !regularFile(join(languageRoots.zh, path)))
      errors.push(`Missing zh page: ${path}`);
  }
  for (const path of zhPaths)
    if (!targetPaths.has(path)) errors.push(`Unregistered zh page: ${path}`);
  for (const path of enPaths) {
    const entry = manifest.entries.find((entry) => entry.targetPath === path);
    if (!entry || entry.translationStatus !== 'reviewed')
      errors.push(`English page is not declared reviewed: ${path}`);
  }
  // A root engineering plan is an explicitly checked document role, not an ignored legacy source.
  const retainedLegacy = new Set(
    manifest.entries
      .filter((entry) => entry.legacyPath && !entry.legacyRemoved)
      .map((entry) => entry.legacyPath)
  );
  for (const entry of readdirSync(docsRoot, { withFileTypes: true })) {
    if (
      entry.name.endsWith('.md') &&
      !engineering.has(entry.name) &&
      !retainedLegacy.has(entry.name)
    ) {
      errors.push(
        `Root document is not registered as engineering or retained legacy: ${entry.name}`
      );
    }
  }
  const selected = manifest.entries.filter((entry) => !scope || entry.kind === scope);
  if (scope && selected.length === 0) errors.push(`No registered documents for scope: ${scope}`);
  for (const entry of manifest.entries) {
    if (entry.legacyPath) {
      const legacyExists = regularFile(join(docsRoot, entry.legacyPath));
      // The migration record stays historical when the same filename is reused for a current engineering plan.
      if (entry.legacyRemoved && legacyExists && !engineering.has(entry.legacyPath)) {
        errors.push(`Legacy source still exists after removal: docs/${entry.legacyPath}`);
      }
      if (!entry.legacyRemoved && !legacyExists)
        errors.push(`Legacy source is missing: docs/${entry.legacyPath}`);
    }
  }
  for (const entry of selected) {
    const { targetPath, translationStatus } = entry;
    const zhPath = join(languageRoots.zh, targetPath);
    if (!regularFile(zhPath)) continue;
    const zhText = readText(zhPath);
    if (!heading(zhText)) errors.push(`Missing zh heading: ${targetPath}`);
    if (!hasHan(prose(zhText))) errors.push(`zh page has no Han-language signal: ${targetPath}`);
    const zhAdr = adrIdentity(zhText);
    if (entry.kind === 'adr') {
      const number = targetPath.match(/^adr\/(\d{4})-/)?.[1];
      if (!number || number !== zhAdr.number)
        errors.push(`ADR filename/heading mismatch: ${targetPath}`);
    }
    if (translationStatus !== 'reviewed') continue;
    const enPath = join(languageRoots.en, targetPath);
    if (!regularFile(enPath)) {
      errors.push(`Missing reviewed en page: ${targetPath}`);
      continue;
    }
    const enText = readText(enPath);
    if (!hasLatin(prose(enText)) || hasHan(prose(enText)))
      errors.push(`en page has an invalid language signal: ${targetPath}`);
    if (
      /(?:migration_pending|unreviewed|not yet reviewed|pending translation|待翻译|未审阅)/i.test(
        enText
      )
    ) {
      errors.push(`en page contains an unreviewed marker: ${targetPath}`);
    }
    if (entry.kind === 'adr') {
      const enAdr = adrIdentity(enText);
      if (zhAdr.number !== enAdr.number) errors.push(`ADR number mismatch: ${targetPath}`);
      if (zhAdr.status !== enAdr.status || zhAdr.superseded !== enAdr.superseded)
        errors.push(`ADR status/superseded mismatch: ${targetPath}`);
    }
  }
  if (!errors.length)
    console.log(
      `Docs index passed${scope ? ` for ${scope}` : ''}: ${targetPaths.size} zh pages, ${enPaths.length} reviewed en pages, ${engineering.size} engineering documents.`
    );
  return errors;
}

function writeManifest(manifest) {
  const structural = [];
  const paths = inspectEntries(manifest, structural);
  engineeringPaths(manifest, structural);
  if (structural.length) throw new Error(structural.join('\n'));
  // Index new Chinese pages without manufacturing a legacy migration or an English review receipt.
  for (const targetPath of markdownFiles(languageRoots.zh)) {
    if (paths.has(targetPath)) continue;
    const kind = targetPath.startsWith('adr/')
      ? 'adr'
      : targetPath.startsWith('guides/')
        ? 'guide'
        : targetPath.startsWith('architecture/')
          ? 'architecture'
          : targetPath.startsWith('operations/')
            ? 'operations'
            : 'reference';
    manifest.entries.push({
      targetPath,
      kind,
      sourceLanguage: 'zh',
      translationStatus: 'not_required',
    });
  }
  writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + '\n');
  console.log(`Updated Chinese document inventory: ${relative(repositoryRoot, manifestPath)}`);
}

try {
  const options = parseOptions(process.argv.slice(2));
  const manifest = readManifest();
  if (options.write) writeManifest(manifest);
  const errors = checkManifest(manifest, options.scope);
  if (errors.length) {
    console.error(`Docs index blocked with ${errors.length} issue(s):`);
    for (const error of errors) console.error(`- ${error}`);
    process.exitCode = 1;
  }
} catch (error) {
  console.error(`Docs index blocked: ${error.message}`);
  process.exitCode = 1;
}
