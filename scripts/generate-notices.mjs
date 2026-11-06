import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync, readdirSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
const root = fileURLToPath(new URL('../', import.meta.url));
const read = path => readFileSync(path, 'utf8');
const cargo = args => execFileSync('cargo', args, { cwd: root, encoding: 'utf8', windowsHide: true, maxBuffer: 20 * 1024 * 1024 });
const metadata = JSON.parse(cargo(['metadata', '--format-version', '1', '--locked', '--offline', '--filter-platform', 'x86_64-pc-windows-msvc']));
const reachable = new Set([...cargo(['tree', '--locked', '--offline', '--target', 'x86_64-pc-windows-msvc', '--prefix', 'none', '--format', '{p}']).matchAll(/^([\w-]+) v([^\s]+)/gm)].map(m => `${m[1]}@${m[2]}`));
const texts = new Map();
const entries = [];
function add(name, version, license, source, files) {
  if (!files.length) throw new Error(`Missing license text for ${name}@${version}`);
  const ids = files.map(path => {
    const text = read(path).replace(/\r\n/g, '\n').replace(/[\t ]+$/gm, '').trim();
    if (!texts.has(text)) texts.set(text, texts.size + 1);
    return texts.get(text);
  });
  entries.push(`${name}@${version}\nLicense expression: ${license}\nSource: ${source}\nLicense texts: ${ids.join(', ')}`);
}
function licenses(directory) {
  return readdirSync(directory, { withFileTypes: true }).filter(f => f.isFile() && /^(licen[cs]e|copying|notice|copyright)/i.test(f.name)).map(f => join(directory, f.name));
}
for (const pkg of metadata.packages.filter(p => p.source && reachable.has(`${p.name}@${p.version}`)).sort((a,b) => `${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`))) {
  const directory = fileURLToPath(new URL('.', 'file:///' + pkg.manifest_path.replaceAll('\\', '/')));
  let files = licenses(directory);
  if (!files.length) {
    const fallback = join(root, 'docs', 'licenses', `${pkg.name}-${pkg.version}.txt`);
    if (existsSync(fallback)) files = [fallback];
  }
  add(pkg.name, pkg.version, pkg.license, `https://crates.io/api/v1/crates/${pkg.name}/${pkg.version}/download`, files);
}
const lock = JSON.parse(read(join(root, 'package-lock.json')));
for (const [path, pkg] of Object.entries(lock.packages).filter(([path,p]) => path && !p.dev)) {
  const directory = join(root, path);
  const manifest = JSON.parse(read(join(directory, 'package.json')));
  add(manifest.name, pkg.version, manifest.license, pkg.resolved || manifest.homepage || '', licenses(directory));
}
add('Tauri NSIS template', '2.12.4', 'MIT OR Apache-2.0', 'https://github.com/tauri-apps/tauri/blob/993038c49185307dfc895ab7657d77ca0d5ed7c7/crates/tauri-bundler/src/bundle/windows/nsis/installer.nsi', [join(root, 'docs/licenses/tauri-installer-2.12.4.txt')]);
add('Inter', '4.1', 'OFL-1.1', 'https://github.com/rsms/inter/tree/v4.1', [join(root, 'public/fonts/Inter-LICENSE.txt')]);
add('Source Han Sans', '2.005', 'OFL-1.1', 'https://github.com/adobe-fonts/source-han-sans/tree/2.005R', [join(root, 'public/fonts/SourceHanSans-LICENSE.txt')]);
const version = JSON.parse(read(join(root, 'package.json'))).version;
const output = `Fusion ${version} — third-party license materials\n\nGenerated from the locked Windows x64 dependency tree (including build dependencies conservatively) and npm runtime dependencies. This inventory does not claim every build dependency is embedded in the executable. Upstream source archives are linked per exact version. No dependency source files were modified. The Tauri installer template changes are described in THIRD_PARTY_NOTICES.md.\n\n${entries.join('\n\n')}\n\n` + [...texts].map(([text, id]) => `===== LICENSE TEXT ${id} =====\n\n${text}\n`).join('\n');
writeFileSync(join(root, 'docs/THIRD_PARTY_LICENSES.txt'), output);
console.log(`License materials: ${entries.length} entries, ${texts.size} texts, ${Buffer.byteLength(output)} bytes`);
