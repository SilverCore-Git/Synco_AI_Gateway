// Règles de détection de secrets pour un dépôt public.
// Les motifs sensibles sont assemblés par concaténation pour que ce fichier ne se détecte pas lui-même.
// Une ligne contenant `check-secrets:allow` est ignorée (à justifier en revue).

export const ALLOW_MARKER = 'check-secrets:allow';

/** Fichiers qui ne doivent jamais être versionnés, quel que soit leur contenu. */
export const FORBIDDEN_FILES = [
  { rule: 'cle-ou-certificat', re: /\.(pem|key|p12|pfx|jks|keystore|crx|xpi)$/i },
  { rule: 'fichier-env', re: /(^|\/)\.env(?!\.(?:example|sample)$)(\..*)?$/i },
  { rule: 'fichier-identifiants', re: /(^|\/)(\.npmrc|\.netrc|credentials\.json|[^/]*\.credentials\.json)$/i },
  { rule: 'cle-ssh', re: /(^|\/)id_(rsa|dsa|ecdsa|ed25519)(\.pub)?$/ },
  { rule: 'artefact-de-build', re: /(^|\/)(dist|node_modules|web-ext-artifacts)\// },
  { rule: 'archive', re: /\.(zip|tgz)$/i },
];

const PRIVATE_IP = [
  '\\b10\\.\\d{1,3}\\.\\d{1,3}\\.\\d{1,3}\\b',
  '\\b192\\.168\\.\\d{1,3}\\.\\d{1,3}\\b',
  '\\b172\\.(?:1[6-9]|2\\d|3[01])\\.\\d{1,3}\\.\\d{1,3}\\b',
].join('|');

/** Motifs de contenu. Les correspondances ne sont jamais affichées, seulement le nom de la règle. */
export const CONTENT_RULES = [
  { rule: 'bloc-cle-privee', re: new RegExp('-----BEGIN ' + '(?:[A-Z0-9 ]+ )?PRIVATE ' + 'KEY') },
  { rule: 'jeton-github', re: new RegExp('\\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_' + 'pat_[A-Za-z0-9_]{50,})\\b') },
  { rule: 'jeton-npm', re: new RegExp('\\bnpm_' + '[A-Za-z0-9]{36}\\b') },
  { rule: 'cle-aws', re: new RegExp('\\b(?:AK' + 'IA|AS' + 'IA)[0-9A-Z]{16}\\b') },
  { rule: 'cle-api-google', re: new RegExp('\\bAI' + 'za[0-9A-Za-z_-]{35}\\b') },
  { rule: 'secret-oauth-google', re: new RegExp('GOC' + 'SPX-[A-Za-z0-9_-]{20,}') },
  { rule: 'jeton-slack', re: new RegExp('\\bxo' + 'x[abprs]-[A-Za-z0-9-]{10,}') },
  { rule: 'jwt', re: new RegExp('\\bey' + 'J[A-Za-z0-9_-]{10,}\\.ey' + 'J[A-Za-z0-9_-]{10,}\\.[A-Za-z0-9_-]{10,}') },
  { rule: 'emetteur-amo', re: /\buser:\d{4,}:\d+\b/ },
  { rule: 'cle-manifeste-chrome', re: /"key"\s*:\s*"MI[A-Za-z0-9+/=]{50,}"/ },
  {
    rule: 'affectation-de-secret',
    re: /(?:api[_-]?secret|api[_-]?key|client[_-]?secret|refresh[_-]?token|access[_-]?token|password|passwd|secret|token)["']?\s*[:=]\s*["'`][^"'`\s]{16,}["'`]/i,
  },
  { rule: 'adresse-ip-privee', re: new RegExp(PRIVATE_IP) },
  { rule: 'hote-interne', re: /(?<![\w.-])(?!host\.docker\.internal\b)[a-z0-9-]+(?:\.[a-z0-9-]+)*\.(?:internal|lan|corp|intranet|home\.arpa)\b/i },
];

/** @returns {{ file: string, line: number | null, rule: string }[]} */
export function scanPath(path) {
  return FORBIDDEN_FILES.filter(({ re }) => re.test(path)).map(({ rule }) => ({ file: path, line: null, rule }));
}

/** @returns {{ file: string, line: number | null, rule: string }[]} */
export function scanText(path, text) {
  const findings = [];
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (line.includes(ALLOW_MARKER)) continue;
    for (const { rule, re } of CONTENT_RULES) {
      if (re.test(line)) findings.push({ file: path, line: i + 1, rule });
    }
  }
  return findings;
}

/** Un tampon contenant un octet nul est considéré comme binaire (non analysé sur son contenu). */
export function looksBinary(buf) {
  const n = Math.min(buf.length, 8000);
  for (let i = 0; i < n; i++) if (buf[i] === 0) return true;
  return false;
}
