// Refresh only when deliberately updating the pinned Bilimbi domain snapshots.
// Platform candidates cover mounted Domains, so every utility the
// Bilimbi domain sources use is already generated from this candidate list.
const fs = require('node:fs');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const { Scanner } = require('@tailwindcss/oxide');
const provenance = JSON.parse(fs.readFileSync(path.join(__dirname, 'provenance.json')));
const pinned = provenance.domain_sources;
const sources = Object.fromEntries(process.argv.slice(2).map(arg => arg.split(/=(.*)/s)));
const missing = Object.keys(pinned).filter(container => !sources[container]);
if (missing.length) {
  throw new Error('Usage: node capture-domains.cjs ' + Object.keys(pinned).map(c => `${c}=/path/to/pinned/${c}`).join(' '));
}
const candidates = new Set();
for (const [container, sha] of Object.entries(pinned)) {
  const source = path.resolve(sources[container]);
  const head = execFileSync('git', ['-C', source, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  if (head !== sha) throw new Error(`Expected ${container} ${sha}, got ${head}`);
  if (execFileSync('git', ['-C', source, 'status', '--porcelain'], { encoding: 'utf8' }).trim()) {
    throw new Error(`${container} has uncommitted changes; scan a clean pinned checkout`);
  }
  // Module sources and priv assets only: lib holds the HEEx and function components.
  const scanner = new Scanner({ sources: ['lib', 'priv'].map(directory => ({
    base: source, pattern: `*/${directory}/**/*`, negated: false,
  })) });
  for (const candidate of scanner.scan()) candidates.add(candidate);
}
fs.writeFileSync(path.join(__dirname, 'domain-candidates.txt'), [...candidates].sort().join('\n') + '\n');
