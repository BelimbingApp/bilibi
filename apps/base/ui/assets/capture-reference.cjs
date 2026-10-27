// Refresh only when deliberately updating the pinned Bilimbi source snapshot.
const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const { Scanner } = require('@tailwindcss/oxide');
const source = process.argv[2];
if (!source) throw new Error('Usage: node capture-reference.cjs /path/to/pinned/bilimbi');
const provenance = JSON.parse(fs.readFileSync(path.join(__dirname, 'provenance.json')));
const head = execFileSync('git', ['-C', source, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
if (head !== provenance.bilimbi_sha) throw new Error(`Expected Bilimbi ${provenance.bilimbi_sha}, got ${head}`);
// Exactly the source directories declared by Bilimbi's CSS entrypoint.
const directories = [
  'apps/web/assets/css', 'apps/web/assets/js', 'apps/web/lib/bilimbi_web',
  'apps/base', 'apps/core', 'apps/domains', 'apps/extensions',
];
const scanner = new Scanner({ sources: directories.map(directory => ({
  base: path.resolve(source, directory), pattern: '**/*', negated: false,
})) });
fs.writeFileSync(path.join(__dirname, 'reference-candidates.txt'), scanner.scan().filter(candidate => !candidate.includes('hero-')).sort().join('\n') + '\n');
