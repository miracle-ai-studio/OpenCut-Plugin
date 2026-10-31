#!/usr/bin/env python3
"""Build an explicit local development delivery: host, npm SDK and portable Rust SDK source."""
import argparse
import gzip
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
_runtime_spec = importlib.util.spec_from_file_location('opencut_runtime_bundle', ROOT / 'scripts/distribution/runtime_bundle.py')
runtime_bundle = importlib.util.module_from_spec(_runtime_spec)
_runtime_spec.loader.exec_module(runtime_bundle)
_native_spec = importlib.util.spec_from_file_location('opencut_native_runtime', ROOT / 'scripts/distribution/native_runtime.py')
native_runtime = importlib.util.module_from_spec(_native_spec)
_native_spec.loader.exec_module(native_runtime)


def run(args, cwd=ROOT):
    # Cargo/npm progress belongs to stderr, including lock-wait messages on a
    # successful JSON command. Never merge it into machine-readable stdout.
    result = subprocess.run([str(a) for a in args], cwd=cwd, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(f'Command failed ({result.returncode}): {args}\n{result.stdout}\n{result.stderr}')
    return result.stdout


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def archive(source, target, allowed_links=None):
    allowed_links = allowed_links or {}
    with target.open('wb') as raw, gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0) as gz, tarfile.open(fileobj=gz, mode='w') as tar:
        for path in sorted(source.rglob('*')):
            if path.is_symlink():
                relative = path.relative_to(source).as_posix()
                if (relative not in allowed_links or path.readlink().as_posix() != allowed_links[relative]
                        or not path.resolve(strict=True).is_relative_to(source.resolve(strict=True))):
                    raise ValueError(f'Unexpected symlink in delivery source: {path}')
                info = tar.gettarinfo(str(path), arcname=f'{source.name}/{relative}')
                info.uid = info.gid = info.mtime = 0
                info.uname = info.gname = ''
                info.mode = 0o777
                tar.addfile(info)
                continue
            if not path.is_file():
                continue
            info = tar.gettarinfo(str(path), arcname=f'{source.name}/{path.relative_to(source)}')
            info.uid = info.gid = 0
            info.uname = info.gname = ''
            info.mtime = 0
            info.mode = 0o755 if path.stat().st_mode & 0o111 else 0o644
            with path.open('rb') as f:
                tar.addfile(info, f)



def copy_sdk_sources(source, destination):
    """Ship Rust source, excluding local caches and editor residue."""
    destination.mkdir(parents=True)
    for path in sorted(source.iterdir()):
        if path.name in {'.DS_Store', '__pycache__', 'target', 'node_modules'} or path.suffix in {'.pyc', '.bak', '.tmp', '.orig', '.rej'}:
            continue
        if path.is_symlink():
            raise ValueError(f'Symlink in SDK source: {path}')
        target = destination / path.name
        if path.is_dir():
            copy_sdk_sources(path, target)
        elif path.suffix == '.rs':
            shutil.copy2(path, target)
        else:
            raise ValueError(f'Unreviewed file in SDK source delivery: {path}')


def validate_npm_files(files):
    """Only public runtime modules, declarations and package metadata are delivered."""
    expected = {'package.json', 'README.md', 'LICENSE'}
    for item in files:
        name = item['path']
        if name in expected:
            continue
        path = Path(name)
        if (len(path.parts) == 2 and path.parts[0] == 'dist'
                and (path.name.endswith('.js') or path.name.endswith('.d.ts'))
                and not any(marker in path.name for marker in ('.test.', '.spec.'))):
            continue
        raise ValueError(f'Unreviewed file in npm delivery: {name}')

def copy_web_build(source, destination):
    """Ship only public build assets; reject workspace data, maps and symlinks."""
    if not (source / 'index.html').is_file():
        raise ValueError('Web build is missing index.html')
    allowed_root = {'index.html', 'favicon.ico', 'manifest.json', 'robots.txt'}
    allowed_assets = {'.js', '.css', '.svg', '.png', '.jpg', '.jpeg', '.webp', '.woff2'}
    atlas = Path('assets/classic-font-atlas')
    atlas_files = {'README.md', 'font-atlas.json'} | {f'font-chunk-{i}.avif' for i in range(15)}
    if (source / atlas).exists():
        actual = {p.name for p in (source / atlas).iterdir()}
        if actual != atlas_files:
            raise ValueError('Public font atlas differs from the reviewed complete file set')
    for file in sorted(source.rglob('*')):
        relative = file.relative_to(source)
        if file.is_symlink():
            raise ValueError(f'Symlink in Web build: {relative}')
        if file.is_dir():
            continue
        if relative.parent == atlas or relative == Path('assets/licenses/preview-digest.txt'):
            original = ROOT / 'apps/web/public' / relative
            if ((relative.parent == atlas and relative.name not in atlas_files) or not original.is_file()
                    or not original.resolve().is_relative_to(ROOT.resolve())
                    or any(p.is_symlink() for p in [original, *original.parents] if p != ROOT and ROOT in p.parents)
                    or sha(file) != sha(original)):
                raise ValueError(f'Public asset bytes differ from reviewed source: {relative}')
        elif str(relative) not in allowed_root and not (relative.parts[0] == 'assets' and file.suffix in allowed_assets):
            raise ValueError(f'Unreviewed Web build file: {relative}')
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(file, target)


def copy_packet_probe_provenance(source, destination):
    """Keep the native metadata parser's reviewed source, patch and license identity."""
    source = Path(source)
    if source.is_symlink():
        raise ValueError('Symlink in native packet provenance')
    manifest = json.loads((source / 'source.json').read_text())
    if (manifest.get('name'), manifest.get('version'), manifest.get('license')) != ('matroska-demuxer', '0.8.1', 'MIT'):
        raise ValueError('Unpinned native packet provenance')
    for relative, digest in manifest['files'].items():
        path = source / relative
        if not path.resolve(strict=True).is_relative_to(source.resolve(strict=True)) or any(p.is_symlink() for p in [path, *path.parents] if p == source or source in p.parents):
            raise ValueError('Symlink or escape in native packet provenance')
        if sha(path) != digest:
            raise ValueError('Native packet source differs from provenance')
    if sha(source / 'opencut.patch') != manifest['patch_sha256']:
        raise ValueError('Native packet patch differs')
    destination = Path(destination)
    destination.mkdir(parents=True, exist_ok=True)
    for name in ['LICENSE-MIT', 'LICENSE-APACHE', 'LICENSE-ZLIB', 'source.json', 'opencut.patch']:
        shutil.copy2(source / name, destination / name)
    return {'implementation': 'native-worker', 'provenance': 'THIRD_PARTY/matroska-demuxer/source.json',
            'provenance_sha256': sha(destination / 'source.json'), 'license': 'MIT',
            'scope': 'isolated native packet statistics; no Node or browser packet probe'}


def copy_catalog_runtime(packages_root, destination):
    """Deliver the fixed Node/zlib adapter only, without a browser runtime or audio."""
    packages_root = Path(packages_root)
    resolved_root = packages_root.resolve(strict=True)
    selected = ['classic-visual-renderer/package.json',
                'classic-visual-renderer/host/catalog-archive.mjs']
    for relative in selected:
        file = packages_root / relative
        for ancestor in [file, *file.parents]:
            if ancestor.is_symlink():
                raise ValueError(f'Symlink in catalog runtime: {relative}')
            if ancestor == packages_root:
                break
        if not file.resolve(strict=True).is_relative_to(resolved_root) or not file.is_file():
            raise ValueError(f'Invalid catalog runtime file: {relative}')
        if not 0 < file.stat().st_size <= (65536 if file.name == 'package.json' else 262144):
            raise ValueError(f'Catalog runtime file exceeds budget: {relative}')
    metadata = json.loads((packages_root / selected[0]).read_text())
    if (metadata.get('name'), metadata.get('version'), metadata.get('type')) != ('@opencut/classic-visual-renderer', '0.1.0', 'module'):
        raise ValueError('Unpinned catalog runtime package')
    files = {}
    for relative in selected:
        source, target = packages_root / relative, Path(destination) / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        files[f'bin/libexec/{relative}'] = {'sha256': sha(target), 'size': target.stat().st_size}
    return {
        'profile': 'catalog-archive-v1-node-zlib', 'external_runtime': 'node',
        'files': files, 'source_archive_included': False,
        'source_archive_sha256': '946fc23a63d535d693eb31b2eabb80c8c28d6351e2186b344ceb71b2cb1d5eb6',
        'download_switch': 'OPENCUT_ALLOW_PUBLIC_RESOURCE_DOWNLOAD',
        'scope': 'fixed Kenney archive extraction; original audio acquisition also requires ffprobe and host download permission or the exact local cache',
        'excluded': 'No source audio is bundled; rendering uses the adjacent native worker',
    }


def package(output, skip_build, supplied_binary=None, runtime_spec=None, supplied_web_root=None, reuse_sdk_dist=False, supplied_worker=None, native_media_spec=None):
    if supplied_web_root is not None:
        if supplied_binary is None:
            raise ValueError('Reusing a Web build requires an explicit already-built binary')
        return package_with_web_root(output, True, supplied_binary, supplied_web_root, runtime_spec, prebuilt_web=True, reuse_sdk_dist=reuse_sdk_dist, supplied_worker=supplied_worker, native_media_spec=native_media_spec)
    # Build assets away from the directory a running service may be serving.
    with tempfile.TemporaryDirectory(prefix='opencut-package-web-') as web_directory:
        return package_with_web_root(output, skip_build, supplied_binary, Path(web_directory), runtime_spec, reuse_sdk_dist=reuse_sdk_dist, supplied_worker=supplied_worker, native_media_spec=native_media_spec)


def package_with_web_root(output, skip_build, supplied_binary, web_root, runtime_spec=None, *, prebuilt_web=False, reuse_sdk_dist=False, supplied_worker=None, native_media_spec=None):
    if native_media_spec is None:
        raise ValueError('Native renderer packaging requires --native-media-spec with built libraries and corresponding source/licenses')
    if not skip_build and supplied_binary is None:
        run(['cargo', 'build', '--locked', '--release', '-p', 'opencut-cli', '-p', 'opencut-render-worker'])
    if not prebuilt_web:
        run(['npm', 'ci', '--ignore-scripts', '--no-audit', '--no-fund'], ROOT / 'apps/web')
        run(['npm', 'run', 'build', '--', '--outDir', web_root], ROOT / 'apps/web')
    binary = supplied_binary or ROOT / 'target/release/opencut'
    worker = supplied_worker or binary.with_name('opencut-render-worker')
    assert binary.is_file() and worker.is_file(), 'Build the integrated release CLI and native worker first'
    tools = json.loads(run([binary, '--json', 'system', 'schema']))['data']
    target = next(line.split(': ', 1)[1] for line in run(['rustc', '-vV']).splitlines() if line.startswith('host: '))
    version = tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']
    with tempfile.TemporaryDirectory(prefix='opencut-package-schema-') as schema_temp:
        schema_db = Path(schema_temp) / 'schema.sqlite'
        run([binary, '--database', schema_db, '--json', 'project', 'list'])
        with sqlite3.connect(schema_db) as connection:
            database_schema = int(connection.execute("SELECT value FROM metadata WHERE key='schema_version'").fetchone()[0])
    domains = json.loads(run([binary, '--json', 'system', 'domains']))['data']
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='opencut-package-') as temporary:
        stage = Path(temporary)
        sdk_ts = ROOT / 'packages/sdk-ts'
        if reuse_sdk_dist:
            for filename in ('index.js', 'index.d.ts'):
                file = sdk_ts / 'dist' / filename
                if file.is_symlink() or not file.is_file() or file.stat().st_size == 0:
                    raise ValueError('Reusing the SDK requires an existing complete public dist')
        else:
            run(['npm', 'run', 'build'], sdk_ts)
        npm_package = json.loads(run(['npm', 'pack', '--ignore-scripts', '--json', '--pack-destination', stage], sdk_ts))[0]
        validate_npm_files(npm_package['files'])
        packed = npm_package['filename']
        rust = stage / 'opencut-rust-sdk'
        rust.mkdir()
        pure = ['contracts', 'media-contracts', 'job-contracts', 'api-contracts', 'sdk-rust']
        for name in pure:
            source, destination = ROOT / 'crates' / name, rust / 'crates' / name
            destination.mkdir(parents=True)
            for filename in ['Cargo.toml', 'README.md']:
                if (source / filename).is_file():
                    shutil.copy2(source / filename, destination / filename)
            for folder in ['src', 'examples']:
                if (source / folder).is_dir():
                    copy_sdk_sources(source / folder, destination / folder)
        (rust / 'Cargo.toml').write_text('[workspace]\nresolver="3"\nmembers=["crates/*"]\n[workspace.package]\n'+f'version="{version}"\nedition="2024"\nlicense="MIT"\n')
        shutil.copy2(ROOT / 'Cargo.lock', rust / 'Cargo.lock')
        shutil.copy2(ROOT / 'LICENSE', rust / 'LICENSE')
        (rust / 'README.md').write_text('OpenCut portable Rust SDK source delivery\n\nUse a path dependency on crates/sdk-rust after extracting this entire archive.\nThe sibling contract crates and workspace Cargo.toml are required.\nRun cargo test --locked -p opencut-sdk from this directory.\nThird-party dependencies are pinned by Cargo.lock and fetched from Cargo registries; this is not an offline vendor bundle.\nNo native runtime or project database is part of this SDK.\n')
        metadata = json.loads(run(['cargo', 'metadata', '--offline', '--format-version', '1'], rust))
        assert all(not p['name'].startswith('opencut-') or p['name'] in ['opencut-contracts', 'opencut-media-contracts', 'opencut-job-contracts', 'opencut-api-contracts', 'opencut-sdk'] for p in metadata['packages'])
        rust_archive = stage / f'opencut-rust-sdk-{version}.tar.gz'
        archive(rust, rust_archive)
        host = stage / f'opencut-{version}-{target}'
        (host / 'bin').mkdir(parents=True)
        native_renderer = native_runtime.bundle(host, binary, worker, target, native_media_spec)
        packaged_binary = host / 'bin/opencut'
        # Relocation changes executable identities. Check the exact delivered CLI.
        run(['node', ROOT / 'packages/sdk-ts/scripts/generate.mjs', '--check', '--skip-build', '--binary', packaged_binary])
        assert json.loads(run([packaged_binary, '--json', 'system', 'schema']))['data'] == json.loads((ROOT / 'packages/contract-fixtures/tools.generated.json').read_text())
        assert json.loads(run([packaged_binary, '--json', 'system', 'events-schema']))['data'] == json.loads((ROOT / 'packages/contract-fixtures/event-stream.generated.json').read_text())
        copy_web_build(web_root, host / 'web')
        packet_runtime = copy_packet_probe_provenance(ROOT / 'crates/media/vendor/matroska-demuxer', host / 'THIRD_PARTY/matroska-demuxer')
        catalog_runtime = copy_catalog_runtime(ROOT / 'packages', host / 'bin/libexec')
        runtime = runtime_bundle.bundle(ROOT, host / 'bin/libexec', target, runtime_spec)
        runtime_bundle.verify(host / 'bin/libexec')
        catalog_runtime['excluded'] = 'No resource audio/source archive; executable installations are declared by runtime_bundle'
        shutil.copy2(ROOT / 'LICENSE', host / 'LICENSE')
        shutil.copy2(ROOT / 'scripts/distribution/doctor.py', host / 'doctor.py')
        shutil.copy2(ROOT / 'scripts/distribution/runtime_bundle.py', host / 'runtime_bundle.py')
        shutil.copy2(ROOT / 'scripts/distribution/native_runtime.py', host / 'native_runtime.py')
        shutil.copy2(ROOT / 'scripts/distribution/agent-config.py', host / 'agent-config.py')
        shutil.copy2(ROOT / 'docs/package.md', host / 'README.md')
        with (host / 'README.md').open('a') as readme:
            readme.write('\n\nExplicit local runtime delivery\n\n'
                         'bin contains the native CLI, renderer worker and the five identified media libraries. '
                         'THIRD_PARTY/native-media preserves corresponding sources, licenses and the build recipe. '
                         'bin/libexec contains the fixed catalog adapter and three font derivation implementations and locks. '
                         'runtime-bundle.json records every auxiliary file, executable bit and permitted internal link. '
                         'Runtime state: ' + runtime['delivery'] + '. '
                         'Optional --runtime-spec supplies explicit FFmpeg/ffprobe, Node and standalone Python installations with redistribution licenses and corresponding source where required. '
                         'Node catalog extraction, Python/fontTools and FFmpeg/ffprobe export tools remain external when not explicitly bundled. '
                         'Run doctor.py after extracting elsewhere, then run actual project acceptance. No runtime is downloaded automatically. '
                         'Catalog downloads remain disabled unless OPENCUT_ALLOW_PUBLIC_RESOURCE_DOWNLOAD=1; no source archive, user media or font cache is bundled.\n')
        shutil.copy2(ROOT / 'docs/status.json', host / 'status.json')
        (host / 'sdk').mkdir()
        shutil.copy2(stage / packed, host / 'sdk' / packed)
        shutil.copy2(rust_archive, host / 'sdk' / rust_archive.name)
        (host / 'contracts').mkdir()
        for name in ['tools.generated.json', 'openapi.generated.json', 'event-stream.generated.json', 'edit-journey.json']:
            shutil.copy2(ROOT / 'packages/contract-fixtures' / name, host / 'contracts' / name)
        host_metadata = json.loads(run(['cargo', 'metadata', '--locked', '--format-version', '1']))
        nodes = {n['id']: n for n in host_metadata['resolve']['nodes']}
        packages = {p['id']: p for p in host_metadata['packages']}
        pending = [p['id'] for p in packages.values() if p['name'] in ('opencut-cli', 'opencut-render-worker')]
        reached = set()
        while pending:
            current = pending.pop()
            if current in reached:
                continue
            reached.add(current)
            pending.extend(nodes[current]['dependencies'])
        inventory = sorted([{'name': packages[i]['name'], 'version': packages[i]['version'], 'license': packages[i]['license']} for i in reached], key=lambda p: (p['name'], p['version']))
        (host / 'dependency-inventory.json').write_text(json.dumps(inventory, indent=2)+'\n')
        files = {str(p.relative_to(host)): {'sha256': sha(p), 'size': p.stat().st_size} for p in sorted(host.rglob('*')) if p.is_file() and not p.is_symlink()}
        links = {'bin/libexec/' + name: entry['target'] for name, entry in runtime['files'].items() if entry['kind'] == 'symlink'}
        manifest = {'format': 'opencut.local-bundle.v1', 'version': version, 'maturity': 'development', 'target': target,
                    'database_schema': database_schema, 'operation_count': len(tools),
                    'domain_count': len(domains), 'binary_sha256': sha(packaged_binary), 'files': files,
                    'external_dependencies': runtime['external_dependencies'], 'links': links,
                    'runtime_bundle': {'manifest': 'bin/libexec/runtime-bundle.json', 'sha256': sha(host / 'bin/libexec/runtime-bundle.json'),
                                       'delivery': runtime['delivery']},
                    'native_renderer': native_renderer,
                    'native_packet_probe': packet_runtime,
                    'catalog_archive_runtime': catalog_runtime,
                    'scope': 'local Web, CLI, HTTP, MCP and SDKs; native worker/media libraries and font scripts; explicitly declared auxiliary installations require relocated doctor and project acceptance; full R0-R7 acceptance not complete'}
        (host / 'manifest.json').write_text(json.dumps(manifest, indent=2)+'\n')
        bundle_id = sha(host / 'manifest.json')[:12]
        destination = output / f'opencut-{version}-dev-{bundle_id}-{target}.tar.gz'
        assembled = stage / destination.name
        archive(host, assembled, links)
        if destination.exists():
            assert sha(destination) == sha(assembled), 'Existing delivery differs; refusing to overwrite'
        else:
            shutil.copy2(assembled, destination)
        digest = sha(destination)
        Path(str(destination)+'.sha256').write_text(f'{digest}  {destination.name}\n')
        receipt = {'status': 'packaged_not_yet_accepted', 'archive': str(destination), 'sha256': digest, 'binary_sha256': sha(packaged_binary), 'target': target, 'manifest': manifest}
        Path(str(destination)+'.json').write_text(json.dumps(receipt, indent=2)+'\n')
        print(json.dumps({k: v for k, v in receipt.items() if k != 'manifest'}, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', type=Path, default=ROOT / 'dist/local')
    parser.add_argument('--skip-build', action='store_true')
    parser.add_argument('--binary', type=Path, help='Package an already-built local CLI instead of building release')
    parser.add_argument('--runtime-spec', type=Path, help='Trusted FFmpeg/ffprobe/Node/standalone-Python installation, license and source spec; never downloads runtimes')
    parser.add_argument('--web-root', type=Path, help='Reuse an already-built public Web tree with --binary; skip Web install/build, retain contract and delivery-file checks')
    parser.add_argument('--reuse-sdk-dist', action='store_true', help='Reuse the already-built local TypeScript SDK dist; public contents and generated contracts are still checked')
    parser.add_argument('--render-worker', type=Path, help='Explicit native worker built with the CLI; defaults to its adjacent worker')
    parser.add_argument('--native-media-spec', type=Path, required=True, help='Trusted native shared libraries, source, build and license input')
    args = parser.parse_args()
    package(args.output.resolve(), args.skip_build, args.binary.resolve() if args.binary else None,
            args.runtime_spec.resolve() if args.runtime_spec else None,
            args.web_root.resolve() if args.web_root else None, args.reuse_sdk_dist,
            args.render_worker.resolve() if args.render_worker else None, args.native_media_spec.resolve())
