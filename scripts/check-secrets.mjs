#!/usr/bin/env node
// Contrôle de secrets avant commit.
//   node scripts/check-secrets.mjs          → fichiers indexés (git add), version de l'index
//   node scripts/check-secrets.mjs --all    → tous les fichiers suivis ou non ignorés
// Code de sortie 1 si un problème est trouvé. Les valeurs détectées ne sont jamais affichées.
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { looksBinary, scanPath, scanText } from './lib/secrets.mjs';

const all = process.argv.includes('--all');

const git = (args) => execFileSync('git', args, { maxBuffer: 256 * 1024 * 1024 });
const names = (buf) => buf.toString('utf8').split('\0').filter(Boolean);

const files = all
  ? names(git(['ls-files', '-co', '--exclude-standard', '-z']))
  : names(git(['diff', '--cached', '--name-only', '--diff-filter=ACMR', '-z']));

const findings = [];
for (const file of files) {
  findings.push(...scanPath(file));
  let buf;
  try {
    buf = all ? readFileSync(file) : git(['show', `:${file}`]);
  } catch {
    continue; // supprimé entre-temps
  }
  if (looksBinary(buf)) continue;
  findings.push(...scanText(file, buf.toString('utf8')));
}

if (findings.length > 0) {
  console.error('check-secrets : commit refusé, éléments sensibles détectés :');
  for (const f of findings) console.error(`  ${f.file}${f.line ? `:${f.line}` : ''}  [${f.rule}]`);
  console.error('Retire-les (ou, si c\'est un faux positif justifié, ajoute « check-secrets:allow » sur la ligne).');
  process.exit(1);
}
console.log(`check-secrets : OK (${files.length} fichier${files.length > 1 ? 's' : ''} analysé${files.length > 1 ? 's' : ''})`);
