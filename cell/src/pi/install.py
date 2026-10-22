"""Build the host-owned Pi seed with a patched, private Node runtime.

The npm release lock fixes every dependency and its integrity hash. Lifecycle
scripts are disabled. This directory is copied into the guest by guest.py;
never install packages or follow guest paths from the host.
"""
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile

PI_VERSION = '1.3.0'
NODE_VERSION = '24.22.1'
RUNTIME = f'pi-{PI_VERSION}-node-{NODE_VERSION}'
ROOT = Path('/usr/local/lib/cell') / RUNTIME
NODE_HASHES = {
    'x86_64': ('x64', '6c172a9ab6718f7b801fb5a7bd486af2ee5e32758086a04b84fcce2aa77095e6'),
    'aarch64': ('arm64', 'dbabf7a1cbf947e6059ffad2c27b80041db97dea1e553b62afdee7f13fca336e'),
}
PACKAGE_HASHES = {
    'package.json': 'efa2ca2695c003c92febb3bca40c5de5f94d43a8aa78e1ba88420b30156d5e21',
    'package-lock.json': '65eacda789c30e1c3859a96eee2fd53cf4a18629d27dcf2aa6d7bdb3da02d719',
}


def run(*args, **kwargs):
    result = subprocess.run(args, text=True, capture_output=True, timeout=600, **kwargs)
    if result.returncode:
        raise RuntimeError(f'{args[0]} failed: {result.stderr[-3000:]}\n{result.stdout[-1000:]}')
    return result.stdout.strip()


def download(url, path, digest):
    run('curl', '--fail', '--silent', '--show-error', '--location',
        '--proto', '=https', '--proto-redir', '=https', '--max-time', '180',
        '--output', str(path), url)
    if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
        raise RuntimeError(f'checksum mismatch: {path.name}')


def verify(root):
    if run(str(root / 'node/bin/node'), '--version') != 'v' + NODE_VERSION:
        raise RuntimeError('unexpected Node runtime version')
    if run(str(root / 'pi'), '--version') != PI_VERSION:
        raise RuntimeError('unexpected Pi version')
    run(str(root / 'node/bin/node'), '-e', '''
const tls = require('node:tls'), { X509Certificate } = require('node:crypto');
const bad = tls.rootCertificates.map(pem => new X509Certificate(pem).subject)
  .filter(s => /CN=(emSign ECC Root CA - G3|AAA Certificate Services)(\\n|$)/.test(s));
if (bad.length) throw new Error('compromised CA roots still trusted: ' + bad);
''')


def install():
    if ROOT.exists():
        verify(ROOT)
        return
    arch, digest = NODE_HASHES[platform.machine()]
    ROOT.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.pi-node-build-', dir=ROOT.parent) as temp:
        stage = Path(temp) / RUNTIME
        stage.mkdir(mode=0o755)
        archive = Path(temp) / 'node.tar.xz'
        download(f'https://nodejs.org/dist/v{NODE_VERSION}/node-v{NODE_VERSION}-linux-{arch}.tar.xz',
                 archive, digest)
        node = stage / 'node'
        node.mkdir()
        run('tar', '-xJf', str(archive), '--strip-components=1', '--no-same-owner', '-C', str(node))
        for name, checksum in PACKAGE_HASHES.items():
            download(f'https://github.com/earendil-works/pi/releases/download/v{PI_VERSION}/pi-coding-agent-install-{name}',
                     stage / name, checksum)
        env = {**os.environ, 'PATH': str(node / 'bin') + ':' + os.environ['PATH'],
               'HOME': temp, 'NPM_CONFIG_USERCONFIG': temp + '/user.npmrc',
               'NPM_CONFIG_GLOBALCONFIG': temp + '/global.npmrc'}
        run(str(node / 'bin/node'), str(node / 'lib/node_modules/npm/bin/npm-cli.js'),
            'ci', '--omit=dev', '--ignore-scripts', '--no-audit', '--no-fund',
            '--cache', str(Path(temp) / 'npm-cache'), cwd=stage, env=env)
        wrapper = stage / 'pi'
        wrapper.write_text('''#!/bin/sh
set -eu
runtime=$(dirname "$(readlink -f "$0")")
export PATH="$runtime/node/bin:$PATH"
exec "$runtime/node/bin/node" "$runtime/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js" "$@"
''')
        wrapper.chmod(0o755)
        verify(stage)
        (stage / 'runtime.json').write_text(json.dumps({
            'pi': PI_VERSION, 'node': NODE_VERSION, 'node_sha256': digest,
            'manifests': PACKAGE_HASHES,
        }, indent=2) + '\n')
        run('chmod', '-R', 'go-w', str(stage))
        stage.rename(ROOT)


if __name__ == '__main__':
    install()
    print(ROOT)
