"""Cell and access workflows on the operator machine. Secrets never enter reports."""
import argparse
import copy
import fcntl
import ipaddress
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from github_common import Failure, atomic_json, private_read, repository
try:
    Operator
except NameError:
    from github_operator import Operator


class ManagementParser(argparse.ArgumentParser):
    def error(self, message):
        raise Failure(message)


def management_options(argv):
    parser = ManagementParser(prog='kuggis ' + ' '.join(argv[:2]), allow_abbrev=False)
    area, action = argv[:2]
    if (area, action) not in {('cell', a) for a in ('list', 'status', 'configure')} | {
            ('access', a) for a in ('list', 'grant', 'revoke', 'check', 'configure')} | {
            ('file', 'put'), ('file', 'get'), ('chat', 'archive'), ('admin', 'upgrade')}:
        raise Failure('unknown command; run kuggis ' + area + ' --help')
    if (area, action) != ('cell', 'list'):
        parser.add_argument('cell')
    parser.add_argument('--json', action='store_true')
    if area == 'access' and action != 'list':
        parser.add_argument('kind', choices=('repo', 'network') if action != 'configure' else ('repo',))
        parser.add_argument('target')
        if action == 'grant':
            parser.add_argument('--role', choices=('read', 'develop'))
        if action in ('grant', 'revoke'):
            parser.add_argument('--plan', action='store_true')
        if action == 'configure':
            for flag in ('--issues', '--pull-requests'):
                parser.add_argument(flag, required=True, choices=('none', 'read', 'write'))
            parser.add_argument('--merge', action='store_true')
    if (area, action) == ('cell', 'status'):
        parser.add_argument('--check', action='store_true')
    if (area, action) in (('cell', 'configure'), ('admin', 'upgrade')):
        if area == 'cell':
            parser.add_argument('--file', required=True)
        parser.add_argument('--plan', action='store_true')
        parser.add_argument('--restart', action='store_true')
        parser.add_argument('--bin', required=area == 'admin')
    if area == 'file':
        if action == 'put':
            parser.add_argument('source', nargs='?')
            parser.add_argument('destination', nargs='?')
            parser.add_argument('--paste', action='store_true')
            parser.add_argument('--chat')
        else:
            parser.add_argument('source')
            parser.add_argument('destination')
        parser.add_argument('--replace', action='store_true')
    if area == 'chat':
        parser.add_argument('chat')
    result = parser.parse_args(argv[2:])
    result.area, result.action = area, action
    if area == 'access' and action == 'grant':
        if result.kind == 'repo' and result.role is None:
            parser.error('repository grants require --role read|develop')
        if result.kind == 'network' and result.role is not None:
            parser.error('--role applies only to repositories')
    return result


def endpoint(text):
    host, separator, port = text.rpartition(':')
    if not separator or not port.isascii() or not port.isdecimal() or str(int(port)) != port or not 1 <= int(port) <= 65535:
        raise Failure('expected HOST:PORT with a port from 1 to 65535')
    if host.startswith('[') and host.endswith(']'):
        try:
            host = str(ipaddress.IPv6Address(host[1:-1]))
        except ValueError:
            raise Failure('invalid IPv6 address') from None
    elif ':' in host:
        raise Failure('IPv6 addresses require brackets')
    host = host.lower()
    if not host or len(host) > 253 or (':' not in host and not all(
            re.fullmatch(r'[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?', label) for label in host.split('.'))):
        raise Failure('expected a hostname or IP address, without a URL or path')
    return {'host': host, 'port': int(port), 'destination': {'kind': 'public'}}


def merge_patch(original, patch):
    if not isinstance(patch, dict):
        return copy.deepcopy(patch)
    result = copy.deepcopy(original) if isinstance(original, dict) else {}
    for key, value in patch.items():
        if value is None:
            result.pop(key, None)
        else:
            result[key] = merge_patch(result.get(key), value)
    return result


def non_network(workspace):
    result = copy.deepcopy(workspace)
    result['resources'] = [{k: v for k, v in r.items() if k != 'endpoints'}
                          for r in result.get('resources', []) if r.get('credentials')]
    return result


def read_json(path):
    with Path(path).open('rb') as stream:
        data = stream.read(1024 * 1024 + 1)
    if len(data) > 1024 * 1024:
        raise Failure('JSON input exceeds 1 MiB')
    return json.loads(data)


class Management:
    def __init__(self, payload, args):
        self.p, self.args = payload, args
        self.config = Path(payload['config']) if 'config' in payload else None
        self.original = self.config.read_bytes() if self.config else None
        self.local = json.loads(self.original) if self.config else None
        self.operator = Operator(payload)
        self.enrollment_restarts = []

    def cli(self, *words, timeout=120, allow_failure=False):
        result = subprocess.run([self.p['binary'], *map(str, words)], capture_output=True, timeout=timeout)
        if result.returncode and not allow_failure:
            message = result.stderr.decode(errors='replace').strip()[-2000:]
            raise Failure(message or 'command failed; inspect cell status before retrying')
        return result.stdout

    def remote(self, action, **fields):
        request = {'action': action, 'cell': self.local['name'], **fields}
        result = subprocess.run(self.p['host'], input=json.dumps(request).encode(), capture_output=True, timeout=120)
        try:
            report = json.loads(result.stdout)
        except (ValueError, UnicodeDecodeError):
            raise Failure('host management failed; check SSH connectivity and host installation') from None
        if result.returncode or not report.get('ok'):
            raise Failure(report.get('error', 'host management failed'))
        return report

    def validate(self, workspace):
        with tempfile.TemporaryDirectory(prefix='.validate-', dir=self.config.parent) as directory:
            path = Path(directory) / 'workspace.json'
            atomic_json(path, workspace)
            self.cli('validate', path)

    def publish(self, workspace):
        if self.config.read_bytes() != self.original:
            raise Failure('host change applied, but local configuration changed concurrently; reconcile before retrying')
        # SSH routing is operator-local metadata, not permission granted to a guest.
        workspace = copy.deepcopy(workspace)
        for field in ('ssh', 'ssh_config'):
            if field in self.local:
                workspace[field] = self.local[field]
        atomic_json(self.config.with_name(self.config.name + '.previous.json'), self.local)
        atomic_json(self.config, workspace)

    def snapshot(self):
        return self.remote('inspect')

    def binding(self, attach=False):
        receipt = self.config.with_name(self.config.name + '.github.json')
        if not receipt.exists():
            if not attach:
                return None
            self.enrollment_restarts = self.operator.attach().get('restarted_services', [])
            # Enrollment publishes the operator configuration too.
            self.original = self.config.read_bytes()
            self.local = json.loads(self.original)
        binding = json.loads(private_read(receipt))
        if binding['url'] != self.operator.configured()[0]:
            raise Failure('cell belongs to a different GitHub broker')
        return binding

    def repositories(self):
        binding = self.binding()
        return self.operator.api('/admin/repos/list', {'cell': binding['cell']}) if binding else {
            'repositories': [], 'connected': False}

    def repo(self):
        args = self.args
        name = repository(args.target)
        if args.action == 'grant':
            # Verify backend support before enrollment or any grant mutation.
            if self.operator.api('/admin/status', {}).get('transport') != 'ssh-gateway':
                raise Failure('roles require the SSH gateway; use legacy repo add --access for the App backend')
        binding = self.binding()
        if getattr(args, 'plan', False):
            return {'ok': True, 'status': 'plan', 'cell': self.local['name'], 'kind': 'repo',
                    'target': name, 'action': args.action, 'role': getattr(args, 'role', None),
                    'enrollment_required': binding is None,
                    'restart': 'proxy' if binding is None and args.action == 'grant' else 'none'}
        if args.action == 'grant':
            level = 'write' if args.role == 'develop' else 'read'
            self.operator.api('/admin/resolve', {'repo': name, 'access': level, 'workflows': False})
            binding = self.binding(attach=True)
        if binding is None:
            if args.action == 'check':
                return {'ok': False, 'reason': 'not_granted', 'kind': 'repo', 'target': name}
            raise Failure('cell has no GitHub enrollment; grant a repository first')
        body = {'cell': binding['cell'], 'repo': name}
        if args.action == 'grant':
            report = self.operator.api('/admin/repos/add', {**body, 'role': args.role})
        elif args.action == 'revoke':
            report = self.operator.api('/admin/repos/remove', body)
        elif args.action == 'configure':
            report = self.operator.api('/admin/repos/api', {**body, 'issues': args.issues,
                'pull_requests': args.pull_requests, 'merge': args.merge})
        else:
            grants = self.repositories()['repositories']
            grant = next((r for r in grants if r['repo'] == name), None)
            if not grant:
                return {'ok': False, 'reason': 'not_granted', 'kind': 'repo', 'target': name}
            self.operator.api('/admin/resolve', {'repo': name, 'access': 'read', 'workflows': False})
            api = {'status': 'not_granted'}
            if any(grant.get('api_permissions', {}).get(k, 'none') != 'none' for k in ('issues', 'pull_requests')):
                try:
                    status = self.operator.api('/admin/api/status', {})
                    api = {'status': 'authenticated' if status.get('authenticated') else 'needs_login'}
                except Failure as error:
                    api = {'status': 'needs_login' if 'login' in str(error).lower() or error.status == 401 else 'unavailable',
                           'detail': str(error)}
            return {'ok': api['status'] in ('authenticated', 'not_granted'), 'kind': 'repo', 'target': name,
                    'grant': grant, 'git_read': 'verified', 'git_write': 'not_tested',
                    'api_login': api, 'api_repository_access': 'not_tested'}
        return {'ok': True, 'cell': self.local['name'], 'kind': 'repo', 'target': name,
                'action': args.action, 'restart': 'proxy' if self.enrollment_restarts else 'none',
                'restarted_services': self.enrollment_restarts, **report}

    def apply_network(self, snapshot, candidate):
        self.validate(candidate)
        report = self.remote('network', revision=snapshot['revision'], workspace=candidate)
        self.publish(report['workspace'])
        return report

    def network(self):
        args = self.args
        target = endpoint(args.target)
        snapshot = self.snapshot()
        candidate = copy.deepcopy(snapshot['workspace'])
        matches = [(r, e) for r in candidate.get('resources', []) for e in r.get('endpoints', [])
                   if (e['host'], e['port']) == (target['host'], target['port'])]
        internet = candidate.get('egress', 'restricted') == 'internet'
        if args.action == 'check':
            if not matches and not internet:
                return {'ok': False, 'reason': 'not_granted', 'kind': 'network', 'target': args.target}
            probe = self.remote('probe', host=target['host'], port=target['port'])
            return {**probe, 'ok': probe['reachable'], 'kind': 'network', 'target': args.target,
                    'granted': True, 'via': 'explicit' if matches else 'internet'}
        if args.action == 'grant' and not matches:
            resource = next((r for r in candidate.get('resources', []) if r['name'] == 'network-grants'), None)
            if resource is None:
                resource = {'name': 'network-grants', 'endpoints': [], 'credentials': []}
                candidate.setdefault('resources', []).append(resource)
            resource.setdefault('endpoints', []).append(target)
        elif args.action == 'revoke':
            if internet:
                raise Failure('internet egress still permits this target; configure restricted egress before revoking a destination')
            for resource, match in matches:
                resource['endpoints'].remove(match)
            candidate['resources'] = [r for r in candidate.get('resources', [])
                                      if r['name'] != 'network-grants' or r.get('endpoints') or r.get('credentials')]
        changed = candidate != snapshot['workspace']
        self.validate(candidate)
        if args.plan:
            return {'ok': True, 'status': 'plan', 'kind': 'network', 'target': args.target,
                    'action': args.action, 'changed': changed, 'restart': 'proxy' if changed else 'none'}
        report = self.apply_network(snapshot, candidate)
        verification = self.remote('probe', host=target['host'], port=target['port'])
        verified = verification['reachable'] if args.action == 'grant' else verification.get('proxy_status') == 403
        return {'ok': verified, 'status': 'applied' if verified else 'applied_verification_failed',
                'kind': 'network', 'target': args.target, 'action': args.action,
                'changed': report['changed'], 'restart': report['restart'], 'verification': verification}

    def configure(self):
        snapshot = self.snapshot()
        current = snapshot['workspace']
        upgrade = self.args.area == 'admin'
        patch = {} if upgrade else read_json(self.args.file)
        if not isinstance(patch, dict):
            raise Failure('configuration patch must be a JSON object')
        candidate = merge_patch(current, patch)
        for key in ('version', 'name', 'ssh', 'ssh_config'):
            if candidate.get(key) != current.get(key):
                raise Failure('configure cannot change cell identity or SSH routing')
        self.validate(candidate)
        changed = candidate != current
        restart = 'cell' if upgrade or non_network(candidate) != non_network(current) else 'proxy' if changed else 'none'
        plan = {'ok': True, 'cell': current['name'], 'status': 'plan', 'changed': changed or upgrade,
                'restart': restart, 'changed_fields': sorted(k for k in set(current) | set(candidate)
                                                            if current.get(k) != candidate.get(k))}
        if self.args.plan:
            return plan
        if restart == 'cell':
            if not self.args.restart:
                raise Failure('this change restarts the cell; review --plan, then pass --restart and --bin LINUX_BINARY')
            if not self.args.bin:
                raise Failure('a cell restart requires --bin LINUX_BINARY')
            with tempfile.TemporaryDirectory(prefix='.configure-', dir=self.config.parent) as directory:
                staged = Path(directory) / 'workspace.json'
                atomic_json(staged, candidate)
                self.cli('setup', staged, '--bin', self.args.bin, '--restart',
                         '--expect-revision', snapshot['revision'], timeout=1800)
            observed = self.snapshot()
            self.publish(observed['workspace'])
        else:
            self.apply_network(snapshot, candidate)
        return {**plan, 'status': 'applied'}

    def run(self):
        args = self.args
        if (args.area, args.action) == ('cell', 'list'):
            cells = []
            for path in sorted(Path(self.p['state_root']).glob('*/workspace.json')):
                workspace = read_json(path)
                cells.append({'name': path.parent.name, 'cell_name': workspace['name'], 'ssh': workspace['ssh'], 'config': str(path),
                              'status': 'not_checked'})
            return {'ok': True, 'cells': cells}
        if args.area == 'access' and args.action != 'list':
            return self.repo() if args.kind == 'repo' else self.network()
        if (args.area, args.action) == ('cell', 'configure') or args.area == 'admin':
            return self.configure()
        if args.area == 'access':
            snapshot = self.snapshot()
            try:
                repos = self.repositories()
            except Failure as error:
                repos = {'error': str(error)}
            return {'ok': 'error' not in repos, 'cell': self.local['name'],
                    'egress': snapshot['workspace'].get('egress', 'restricted'),
                    'network': [{'resource': r['name'], **e} for r in snapshot['workspace'].get('resources', [])
                                for e in r.get('endpoints', [])], 'github': repos}
        if (args.area, args.action) == ('cell', 'status'):
            snapshot = self.snapshot()
            active = all(snapshot['services'][name].get('ActiveState') == 'active'
                         for name in ('kuggis-v2.service', 'kuggis-v2-proxy.service'))
            report = {'ok': not snapshot['updating'] and active, 'cell': self.local['name'],
                      'services': snapshot['services'], 'setup_incomplete': snapshot['updating']}
            if args.check:
                report['checks'] = json.loads(self.cli('check', self.config, allow_failure=True))
                report['ok'] = report['ok'] and report['checks']['ok']
            return report
        if args.area == 'chat':
            self.cli('archive', self.config, '--chat', args.chat)
            return {'ok': True, 'cell': self.local['name'], 'archived': args.chat}
        words = [args.action, str(self.config)]
        if args.area == 'file':
            if getattr(args, 'paste', False):
                if args.source or args.destination:
                    raise Failure('--paste cannot be combined with file paths')
                words.append('--paste')
            else:
                if not args.source:
                    raise Failure('expected a local file or --paste')
                words.append(args.source)
                if args.destination:
                    words.append(args.destination)
            if getattr(args, 'chat', None):
                words.extend(['--chat', args.chat])
            if args.replace:
                words.append('--replace')
        self.cli(*words)
        return {'ok': True, 'cell': self.local['name'], 'operation': args.action,
                'source': args.source, 'destination': args.destination}


def management_text(report):
    if 'cells' in report:
        return '\n'.join(['Saved cells (live status not checked):', *[
            f"  {cell['name']:<24} {cell['ssh']}" for cell in report['cells']]])
    if 'network' in report:
        lines = [f"Access for {report['cell']} (egress: {report['egress']}):"]
        for grant in report['github'].get('repositories', []):
            git = grant['permissions'].get('git', grant['permissions'].get('contents', 'none'))
            api = grant.get('api_permissions', grant['permissions'])
            lines.append(f"  repo {grant['repo']}: Git {git}, issues {api.get('issues', 'none')}, "
                         f"PRs {api.get('pull_requests', 'none')}, API merge {api.get('merge', 'not separately controlled' if 'contents' in grant['permissions'] else False)}")
        lines.extend(f"  network {e['host']}:{e['port']} ({e['resource']}; {e.get('destination', {}).get('kind', 'public')})"
                     for e in report['network'])
        if 'error' in report['github']:
            lines.append('  GitHub unavailable: ' + report['github']['error'])
        return '\n'.join(lines)
    if 'services' in report:
        lines = [f"{report['cell']}: {'ready' if report['ok'] else 'needs attention'}"]
        lines.extend(f"  {name}: {state.get('ActiveState', 'unknown')}" for name, state in report['services'].items())
        lines.extend(f"  {entry['name']}: {'passed' if entry['passed'] else entry.get('detail', 'failed')}"
                     for entry in report.get('checks', {}).get('checks', []))
        return '\n'.join(lines)
    if report.get('status') == 'plan':
        lines = [f"Plan: {report.get('action', 'configure')} {report.get('kind', 'cell')} {report.get('target', report.get('cell', ''))}",
                 f"  Restart: {report['restart']}"]
        if report.get('role'):
            lines.append('  Role: ' + report['role'] + ' (API merging disabled)')
        if report.get('enrollment_required'):
            lines.append('  Enroll this cell with the configured GitHub broker.')
        if report.get('changed_fields'):
            lines.append('  Changed fields: ' + ', '.join(report['changed_fields']))
        if report['restart'] == 'cell':
            lines.append('  Applying requires --restart and --bin LINUX_BINARY.')
        elif report['restart'] == 'proxy':
            lines.append('  Outbound connections may briefly disconnect; the cell stays running.')
        return '\n'.join(lines)
    if 'git_read' in report:
        return (f"{report['target']}: Git read {report['git_read']}; Git write not tested.\n"
                f"  API login: {report['api_login']['status']}; API repository access not tested.")
    if 'reason' in report:
        return f"{report.get('target', 'Operation')}: {report['reason']}"
    if 'reachable' in report:
        return f"{report['target']}: {'reachable through the proxy' if report['reachable'] else 'proxy connection failed'}; application authentication not checked."
    if report.get('kind') == 'repo':
        change = 'Proxy and GitHub relay restarted for enrollment; cell stayed running.' if report.get('restarted_services') else 'No restart.'
        return f"Repository access {report['action']}: {report['target']} on {report['cell']}. {change}"
    if report.get('kind') == 'network':
        return (f"Network access {report['action']}: {report['target']}. "
                f"{'Verified' if report['ok'] else 'Saved, but connectivity verification failed'}. Restart: {report['restart']}.")
    if report.get('status') == 'applied':
        return f"{report['cell']}: configuration applied. Restart: {report['restart']}."
    if 'archived' in report:
        return f"Archived {report['archived']} in {report['cell']}."
    return f"{report.get('operation', 'Operation')} completed for {report.get('cell', 'cell')}."


def management_main(payload):
    json_output = '--json' in payload['args']
    try:
        args = management_options(payload['args'])
        manager = Management(payload, args)
        if manager.config and (args.area == 'access' or args.action == 'configure' or args.area == 'admin'):
            with manager.config.with_name(manager.config.name + '.management.lock').open('a') as lock:
                fcntl.flock(lock, (fcntl.LOCK_SH if args.action in ('list', 'check') else fcntl.LOCK_EX) | fcntl.LOCK_NB)
                report = manager.run()
        else:
            report = manager.run()
        if json_output:
            print(json.dumps(report))
        else:
            print(management_text(report))
        return 0 if report.get('ok', True) else 2
    except Exception as error:
        message = str(error) if isinstance(error, (Failure, ValueError, FileNotFoundError)) else (
            'another operation is in progress' if isinstance(error, BlockingIOError) else 'management failed; check host connectivity and cell status')
        if json_output:
            print(json.dumps({'ok': False, 'error': {'code': 'management_failed', 'message': message}}))
        else:
            print('kuggis: ' + message, file=sys.stderr)
        return 2
