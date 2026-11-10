#!/usr/bin/env python3
"""Compare each recipe's pinned agent release with the newest upstream release.

Exit status 0 when every pin is current, 1 when any is stale, 2 on an error.
`--update` rewrites stale pins with the upstream version and SHA-256. A bumped
pin is untested: run the recipe's smoke check on a cell before merging it.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
CLAUDE = 'https://downloads.claude.ai/claude-code-releases/'
CLAUDE_PLATFORMS = {'x86_64': 'linux-x64', 'aarch64': 'linux-arm64'}
GITHUB = re.compile(r'https://github\.com/([^/]+/[^/]+)/releases/download/([^/]+)/([^/]+)')


def fetch(url):
    """curl is already a Kuggis prerequisite and honours the operator's proxy settings."""
    argv = ['curl', '--fail', '--location', '--silent', '--show-error', '--retry', '2', '--max-time', '60',
            '--user-agent', 'kuggis-release-check']
    if url.startswith('https://api.github.com/'):
        argv += ['--header', 'Accept: application/vnd.github+json']
        if os.environ.get('GITHUB_TOKEN'):  # Only a higher rate limit; the data is public.
            argv += ['--header', 'Authorization: Bearer ' + os.environ['GITHUB_TOKEN']]
    result = subprocess.run([*argv, url], capture_output=True, timeout=200)
    if result.returncode:
        raise RuntimeError(result.stderr.decode(errors='replace').strip()[-300:] or 'curl failed')
    return result.stdout.decode()


def latest(release, fetch=fetch):
    """The newest upstream release in the same shape as a recipe's release.json."""
    archives = release['archives']
    url = next(iter(archives.values()))['url']
    if url.startswith(CLAUDE):
        version = fetch(CLAUDE + 'latest').strip()
        platforms = json.loads(fetch(CLAUDE + version + '/manifest.json'))['platforms']
        return {'version': version, 'archives': {arch: {
            'url': CLAUDE + version + '/' + CLAUDE_PLATFORMS[arch] + '/claude',
            'sha256': platforms[CLAUDE_PLATFORMS[arch]]['checksum']} for arch in archives}}
    match = GITHUB.fullmatch(url)
    if not match:
        raise ValueError('unrecognized release source: ' + url)
    repository, tag, _ = match.groups()
    if release['version'] not in tag:
        raise ValueError('pinned tag ' + tag + ' does not contain version ' + release['version'])
    prefix = tag.replace(release['version'], '')
    newest = json.loads(fetch('https://api.github.com/repos/' + repository + '/releases/latest'))
    if not newest['tag_name'].startswith(prefix):
        raise ValueError('unexpected upstream tag ' + newest['tag_name'])
    assets = {asset['name']: asset for asset in newest['assets']}
    result = {'version': newest['tag_name'][len(prefix):], 'archives': {}}
    for arch, spec in archives.items():
        name = GITHUB.fullmatch(spec['url']).group(3)
        digest = (assets.get(name) or {}).get('digest') or ''
        if not digest.startswith('sha256:'):
            raise ValueError('upstream release has no SHA-256 for ' + name)
        result['archives'][arch] = {'url': assets[name]['browser_download_url'], 'sha256': digest[7:]}
    return result


def check(root=ROOT, fetch=fetch, update=False, only=()):
    report = {}
    for path in sorted((root / 'recipes').glob('*/release.json')):
        agent = path.parent.name
        if only and agent not in only:
            continue
        pinned = json.loads(path.read_text())
        try:
            newest = latest(pinned, fetch)
        except Exception as error:  # One unreachable upstream must not hide the others.
            report[agent] = {'pinned': pinned['version'], 'error': str(error) or type(error).__name__}
            continue
        stale = newest['version'] != pinned['version']
        report[agent] = {'pinned': pinned['version'], 'latest': newest['version'], 'stale': stale}
        if stale and update:
            path.write_text(json.dumps(newest, indent=2) + '\n')
            report[agent]['updated'] = True
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('agents', nargs='*', help='recipe names to check (default: all)')
    parser.add_argument('--update', action='store_true', help='rewrite stale pins')
    parser.add_argument('--json', action='store_true')
    args = parser.parse_args(argv)
    report = check(update=args.update, only=args.agents)
    if args.json:
        print(json.dumps(report, indent=2))
    else:
        for agent, entry in report.items():
            if 'error' in entry:
                print(f"{agent}: pinned {entry['pinned']}; could not check upstream: {entry['error']}")
            elif entry['stale']:
                print(f"{agent}: pinned {entry['pinned']}, latest {entry['latest']}"
                      + (' (pin updated; test before merging)' if entry.get('updated') else ' (stale)'))
            else:
                print(f"{agent}: {entry['pinned']} is current")
    if any('error' in entry for entry in report.values()):
        return 2
    return 1 if any(entry['stale'] and not entry.get('updated') for entry in report.values()) else 0


if __name__ == '__main__':
    sys.exit(main())
