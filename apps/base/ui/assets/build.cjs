// One-shot build of the Tailwind assets. Cargo embeds output
// from OUT_DIR; the optional local app.css is ignored by Git. Node is build-only.
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { execFileSync } = require('node:child_process');

const args = process.argv.slice(2);
const check = args.length === 1 && args[0] === '--check';
const explicitOutput = args.length === 2 && args[0] === '--output';
if (args.length && !check && !explicitOutput) {
  throw new Error('Usage: build-assets.sh [--check | --output <path>]');
}

// Tailwind skips gitignored mounts even under a listed directory. Scan each
// container explicitly. --check still requires its utilities to be supplied by
// the Platform's components or pinned candidates: Domains own no CSS.
const apps = path.join(__dirname, '../../..');
const mounted = ['domains', 'extensions'].flatMap(layer => {
  const root = path.join(apps, layer);
  return fs.readdirSync(root, { withFileTypes: true })
    .filter(entry => entry.isDirectory() && !entry.name.startsWith('.')
      && fs.existsSync(path.join(root, entry.name, 'bilibi.container.toml')))
    .map(entry => path.join(root, entry.name)).sort();
});
const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'bilibi-css-'));
try {
  const entry = path.join(directory, 'input.css');
  const output = path.join(directory, 'app.css');
  function compile(containers) {
    fs.writeFileSync(entry, [
      `@import ${JSON.stringify(path.join(__dirname, 'css/app.css'))};`,
      ...containers.map(dir => `@source ${JSON.stringify(dir)};`),
    ].join('\n') + '\n');
    execFileSync(process.execPath, [
      path.join(__dirname, 'node_modules/@tailwindcss/cli/dist/index.mjs'),
      '--input', entry,
      '--output', output,
      '--minify',
    ], { cwd: __dirname, stdio: 'inherit' });
    return fs.readFileSync(output);
  }
  const compiled = compile(mounted);
  if (check) {
    // A fresh checkout has no artifact to compare. Build twice instead: this
    // proves reproducibility and, with mounts, also checks Platform coverage.
    if (!compiled.equals(compile([]))) {
      console.error('UI CSS differs between builds. A utility used only by a mounted container belongs in a Platform component or assets/domain-candidates.txt. Commit source changes only; Domains own no CSS.');
      process.exitCode = 1;
    }
  } else {
    const artifact = explicitOutput ? path.resolve(args[1]) : path.join(__dirname, 'app.css');
    fs.mkdirSync(path.dirname(artifact), { recursive: true });
    // Leave identical bytes untouched, avoiding unnecessary Rust rebuilds.
    if (!fs.existsSync(artifact) || !compiled.equals(fs.readFileSync(artifact))) {
      fs.writeFileSync(artifact, compiled);
    }
  }
} finally {
  fs.rmSync(directory, { recursive: true, force: true });
}
