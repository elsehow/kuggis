"""Cell and access workflows on the operator machine. Secrets never enter reports."""
import argparse
import copy
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import shlex
import shutil
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
    if (area, action) not in {('cell', a) for a in ('list', 'status', 'configure', 'delete')} | {
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
    if (area, action) == ('cell', 'delete'):
        parser.add_argument('--plan', action='store_true')
        parser.add_argument('--yes', action='store_true')
    if (area, action) in (('cell', 'configure'), ('admin', 'upgrade')):
        if area == 'cell':
            parser.add_argument('--file', required=True)
        parser.add_argument('--plan', action='store_true')
        parser.add_argument('--restart', action='store_true')
        parser.add_argument('--bin', required=area == 'admin')
    # CELL --put/--get: every positional is an item; --save-as names destinations.
    many = area == 'file' and '--items' in argv[2:]
    if many:
        parser.add_argument('--items', action='store_true')
        parser.add_argument('items_list', nargs='*', metavar='ITEM')
        parser.add_argument('--save-as', nargs='+', metavar='DESTINATION')
        if action == 'put':
            parser.add_argument('--paste', action='store_true')
            parser.add_argument('--chat')
        parser.add_argument('--replace', action='store_true')
    elif area == 'file':
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
    # Items and flags may interleave; before Python 3.13 plain parse_args
    # rejects positionals that follow an option.
    result = (parser.parse_intermixed_args if many else parser.parse_args)(argv[2:])
    result.area, result.action = area, action
    if many:
        items, names = result.items_list, result.save_as
        result.source = result.destination = None
        if getattr(result, 'paste', False):
            if items or names:
                parser.error('--paste cannot be combined with file paths')
        elif not items:
            parser.error('expected at least one ' + ('local file or --paste' if action == 'put' else 'guest path'))
        elif names and len(names) != len(items):
            parser.error(f'--save-as needs one destination per item: got {len(names)} for {len(items)} items')
        # Downloads without --save-as land in the current directory by name.
        result.transfers = list(zip(items, names or [
            None if action == 'put' else os.path.basename(item.rstrip('/')) for item in items]))
        if any(not destination for _, destination in result.transfers) and action == 'get':
            parser.error('cannot derive a local name; name it with --save-as')
        local = [destination for _, destination in result.transfers if destination]
        if len(set(local)) != len(local):
            parser.error('two items would be saved to the same destination; name them with --save-as')
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


GATEWAY_HOST = 'llm.int.exe.xyz'


def without_gateway(workspace):
    """Drop the retired exe model gateway grant; agents use their native login."""
    candidate = copy.deepcopy(workspace)
    removed = [r['name'] for r in candidate.get('resources', [])
               if any(e.get('host') == GATEWAY_HOST for e in r.get('endpoints', []))]
    if removed:
        candidate['resources'] = [r for r in candidate['resources'] if r['name'] not in removed]
    return candidate, removed


AGENT_STAGE = '/home/agent/.local/share/kuggis/setup/agents'
# Installed release per agent: the version directory its executable links into,
# and a digest of the Kuggis launcher beside it.
AGENT_PROBE = '''import hashlib,json,os,sys
from pathlib import Path
home=Path.home();out={}
for agent in sys.argv[1:]:
    lib=Path(os.path.realpath(home/'.local/lib'/agent));target=Path(os.path.realpath(home/'.local/bin'/agent))
    launcher=home/'.local/bin'/('kuggis-'+agent)
    out[agent]={'version':target.relative_to(lib).parts[0] if target.is_file() and lib in target.parents else None,
                'launcher':hashlib.sha256(launcher.read_bytes()).hexdigest() if launcher.is_file() else None}
print(json.dumps(out))
'''
# Stage through the workspace (uploads are workspace-relative) without leaving
# installer files in the project: a link to private guest state, removed after.
AGENT_PREPARE = '''import sys
from pathlib import Path
stage=Path(sys.argv[1]);(stage/sys.argv[2]).mkdir(parents=True,exist_ok=True)
link=Path('/home/agent/workspace/.kuggis-setup')
if link.is_symlink():
    assert link.readlink()==stage, 'setup staging link belongs to another target'
elif link.exists():
    raise RuntimeError('project uses reserved setup staging path .kuggis-setup')
else:link.symlink_to(stage,target_is_directory=True)
'''
AGENT_CLEAN = '''import sys
from pathlib import Path
link=Path('/home/agent/workspace/.kuggis-setup')
if link.is_symlink() and link.readlink()==Path(sys.argv[1]):link.unlink()
'''


def with_builtin_agents(workspace, builtin):
    """Register built-in agents on a cell that already selects agents per chat.

    Existing entries, the default agent and legacy launch-only cells are left
    alone: binding a legacy default chat to a recipe is an explicit migration."""
    candidate = copy.deepcopy(workspace)
    if not candidate.get('agents'):
        return candidate, []
    added = [name for name in builtin if name not in candidate['agents']]
    for name in added:
        candidate['agents'][name] = copy.deepcopy(builtin[name])
    return candidate, added


def agent_plan(recipes, installed):
    """What upgrade does to each built-in agent, from the guest's installed state."""
    plan = {}
    for agent, files in recipes.items():
        version = json.loads(files['release.json'])['version']
        found = installed.get(agent) or {}
        launcher = hashlib.sha256(files['launch.py'].encode()).hexdigest() if 'launch.py' in files else None
        current = found.get('version') == version and found.get('launcher') == launcher
        plan[agent] = {'installed': found.get('version'), 'pinned': version,
                       'action': 'none' if current else 'install'}
    return plan


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


STEP_NAMES = {'host': 'host', 'github_enrollment': 'GitHub enrollment',
              'repository_grant': 'repository grant', 'local_record': 'local record'}


def host_name(receipt, workspace):
    return receipt.get('vm') or workspace['ssh'].removesuffix('.exe.xyz')


def setup_record(config, workspace):
    """What creation recorded locally, in the order it made it. Nothing is probed,
    and grants changed after creation are not reflected; removal undoes it in reverse."""
    receipt = config.with_name('receipt.json')
    if not receipt.exists():
        return {'state': 'not_recorded', 'created': []}
    record = read_json(receipt)
    created = []
    if record.get('created_at') or record.get('creation_started'):
        # A started creation without an observed host may or may not have allocated one.
        created.append({'kind': 'host', 'name': host_name(record, workspace),
                        'state': 'created' if record.get('created_at') else 'unknown'})
    if config.with_name(config.name + '.github.json').exists():
        created.append({'kind': 'github_enrollment', 'state': 'created'})
    elif config.with_name(config.name + '.github-enrollment.json').exists():
        created.append({'kind': 'github_enrollment', 'state': 'unknown'})
    if record.get('github_initialized'):
        request = config.with_name('setup.json')
        created.append({'kind': 'repository_grant', 'state': 'created',
                        'name': read_json(request).get('repo') if request.exists() else None})
    return {'state': 'ready' if record.get('status') == 'ready' else 'incomplete', 'created': created}


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

    def provider(self, receipt, *words, allow_failure=False):
        argv = ['ssh', '-T', '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes', '-o', 'ForwardAgent=no',
                '-o', 'ForwardX11=no', '-o', 'ClearAllForwardings=yes', '-o', 'ControlMaster=no', '-o', 'ControlPath=none',
                '-o', 'PermitLocalCommand=no', '-o', 'ConnectTimeout=15']
        if receipt.get('ssh_config'):
            argv += ['-F', str(receipt['ssh_config'])]
        result = subprocess.run([*argv, 'exe.dev', shlex.join(words)], stdin=subprocess.DEVNULL,
                                capture_output=True, timeout=120)
        if result.returncode and not allow_failure:
            raise Failure('exe.dev ' + words[0] + ' failed; check provider access and retry')
        return result.stdout

    def owned_host(self, receipt, name):
        """The provider host this cell's creation made, or None when it is gone."""
        try:
            vms = json.loads(self.provider(receipt, 'ls', '--json'))['vms']
            matches = [vm for vm in vms if vm.get('vm_name') == name]
        except (ValueError, KeyError, TypeError, AttributeError):
            raise Failure('exe.dev returned an invalid host inventory') from None
        if not matches:
            return None
        if (len(matches) != 1 or not receipt.get('marker') or matches[0].get('comment') != receipt['marker']
                or receipt.get('created_at') not in (None, matches[0].get('created_at'))):
            raise Failure(f'a host named {name} exists but is not the one this cell created; it was not deleted')
        return matches[0]

    def delete(self):
        """Undo creation in reverse: repository grants, then the host, then the local record."""
        directory = self.config.parent
        if self.config.name != 'workspace.json' or directory.parent.resolve() != Path(self.p['state_root']).resolve():
            raise Failure('delete takes a saved cell name; this workspace file is outside the Kuggis state directory')
        receipt = directory/'receipt.json'
        receipt = read_json(receipt) if receipt.exists() else {}
        apply = self.args.yes and not self.args.plan
        steps = []
        binding = self.binding()
        repos = [grant['repo'] for grant in self.repositories().get('repositories', [])]
        for repo in repos:
            if apply:
                self.operator.api('/admin/repos/remove', {'cell': binding['cell'], 'repo': repo})
            steps.append({'kind': 'repository_grant', 'name': repo, 'result': 'revoked' if apply else 'revoke'})
        if binding or self.config.with_name(self.config.name + '.github-enrollment.json').exists():
            # The broker has no cell removal; without grants the enrollment authorizes nothing.
            steps.append({'kind': 'github_enrollment', 'result': 'retained'})
        if receipt.get('created_at') or receipt.get('creation_started'):
            name = host_name(receipt, self.local)
            if self.owned_host(receipt, name) is None:
                steps.append({'kind': 'host', 'name': name, 'result': 'absent'})
            elif apply:
                # The provider's output format is not documented; trust only the inventory.
                self.provider(receipt, 'rm', name, allow_failure=True)
                if self.owned_host(receipt, name) is not None:
                    raise Failure(f'exe.dev did not delete {name}; run ssh exe.dev rm {name}, then retry')
                steps.append({'kind': 'host', 'name': name, 'result': 'deleted'})
            else:
                steps.append({'kind': 'host', 'name': name, 'result': 'delete'})
        elif not receipt:
            steps.append({'kind': 'host', 'name': self.local['ssh'], 'result': 'not_managed'})
        if apply:
            shutil.rmtree(directory)
        steps.append({'kind': 'local_record', 'name': str(directory), 'result': 'removed' if apply else 'remove'})
        return {'ok': True, 'cell': directory.name, 'deleted': apply, 'steps': steps}

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

    def installed_agents(self, agents):
        try:
            return json.loads(self.cli('exec', self.config, '--', '/usr/bin/python3', '-c', AGENT_PROBE, *agents))
        except (Failure, ValueError, subprocess.TimeoutExpired):
            return {}  # Unreachable or unreadable: install rather than assume current.

    def download_agent(self, agent, files):
        spec = json.loads(files['release.json'])['archives']['x86_64']
        package = self.config.with_name(agent + '.pkg')
        def digest(path):
            with path.open('rb') as stream:
                return hashlib.file_digest(stream, 'sha256').hexdigest()
        if not package.exists() or digest(package) != spec['sha256']:
            partial = package.with_suffix('.pkg.partial')
            result = subprocess.run(['curl', '--fail', '--location', '--silent', '--show-error', '--retry', '2',
                                     '--connect-timeout', '20', '--max-time', '900', spec['url'], '--output', str(partial)],
                                    capture_output=True, timeout=1860)
            if result.returncode:
                raise Failure('could not download the pinned ' + agent + ' release: '
                              + result.stderr.decode(errors='replace').strip()[-500:])
            if digest(partial) != spec['sha256']:
                partial.unlink()
                raise Failure(agent + ' release checksum does not match the pinned release')
            os.replace(partial, package)
        return package

    def install_agents(self, packages):
        """Upload and run each pinned recipe as the guest agent user."""
        guest = lambda *argv, timeout=120: self.cli('exec', self.config, '--', '/usr/bin/python3', *argv, timeout=timeout)
        try:
            for agent, package in packages.items():
                guest('-c', AGENT_PREPARE, AGENT_STAGE, agent)
                local = self.config.with_name('recipes') / agent
                local.mkdir(parents=True, exist_ok=True)
                for name, content in self.p['recipes'][agent].items():
                    (local / name).write_text(content)
                    self.cli('put', self.config, local / name, '.kuggis-setup/' + agent + '/' + name, '--replace')
                self.cli('put', self.config, package, '.kuggis-setup/' + agent + '/agent.pkg', '--replace', timeout=1800)
                guest(AGENT_STAGE + '/' + agent + '/install.py', AGENT_STAGE + '/' + agent + '/agent.pkg', timeout=300)
        finally:
            self.cli('exec', self.config, '--', '/usr/bin/python3', '-c', AGENT_CLEAN, AGENT_STAGE, allow_failure=True)

    def configure(self):
        snapshot = self.snapshot()
        current = snapshot['workspace']
        upgrade = self.args.area == 'admin'
        patch = {} if upgrade else read_json(self.args.file)
        if not isinstance(patch, dict):
            raise Failure('configuration patch must be a JSON object')
        candidate = merge_patch(current, patch)
        retired, registered, agents = [], [], {}
        if upgrade:
            candidate, retired = without_gateway(candidate)
            candidate, registered = with_builtin_agents(candidate, self.p.get('agents', {}))
            # Only cells that select agents per chat have their agents managed.
            if candidate.get('agents') and self.p.get('recipes'):
                agents = agent_plan(self.p['recipes'], self.installed_agents(list(self.p['recipes'])))
        for key in ('version', 'name', 'ssh', 'ssh_config'):
            if candidate.get(key) != current.get(key):
                raise Failure('configure cannot change cell identity or SSH routing')
        self.validate(candidate)
        changed = candidate != current
        restart = 'cell' if upgrade or non_network(candidate) != non_network(current) else 'proxy' if changed else 'none'
        plan = {'ok': True, 'cell': current['name'], 'status': 'plan', 'changed': changed or upgrade,
                'restart': restart, 'changed_fields': sorted(k for k in set(current) | set(candidate)
                                                            if current.get(k) != candidate.get(k))}
        if retired:
            plan['removed_model_gateway'] = retired
        if registered:
            plan['registered_agents'] = registered
        if agents:
            plan['agents'] = agents
        elif upgrade:
            plan['agents_unmanaged'] = 'cell has no agents section; agent recipes were not installed'
        if self.args.plan:
            return plan
        if restart == 'cell':
            if not self.args.restart:
                raise Failure('this change restarts the cell; review --plan, then pass --restart and --bin LINUX_BINARY')
            if not self.args.bin:
                raise Failure('a cell restart requires --bin LINUX_BINARY')
            # Fetch before stopping anything: a failed download changes nothing.
            packages = {agent: self.download_agent(agent, self.p['recipes'][agent])
                        for agent, entry in agents.items() if entry['action'] == 'install'}
            with tempfile.TemporaryDirectory(prefix='.configure-', dir=self.config.parent) as directory:
                staged = Path(directory) / 'workspace.json'
                atomic_json(staged, candidate)
                self.cli('setup', staged, '--bin', self.args.bin, '--restart',
                         '--expect-revision', snapshot['revision'], timeout=1800)
            observed = self.snapshot()
            self.publish(observed['workspace'])
            if packages:
                try:
                    self.install_agents(packages)
                except Failure as error:
                    raise Failure('the cell runtime was upgraded and restarted, but installing ' + ', '.join(packages)
                                  + ' failed; rerun the same upgrade to retry. ' + str(error)) from None
        else:
            self.apply_network(snapshot, candidate)
        return {**plan, 'status': 'applied'}

    def transfer_items(self, args):
        """One ordinary transfer per item, in order; stop at the first failure."""
        flags = [*(['--chat', args.chat] if getattr(args, 'chat', None) else []),
                 *(['--replace'] if args.replace else [])]
        done = []
        for source, destination in args.transfers:
            try:
                self.cli(args.action, self.config, source, *([destination] if destination else []), *flags)
            except Failure as error:
                if not done:
                    raise
                raise Failure(f'{error} ({len(done)} of {len(args.transfers)} items completed before '
                              f'{source} failed: ' + ', '.join(item['source'] for item in done) + ')') from None
            done.append({'source': source, 'destination': destination})
        report = {'ok': True, 'cell': self.local['name'], 'operation': args.action, 'items': done}
        if len(done) == 1:
            report.update(done[0])
        return report

    def run(self):
        args = self.args
        if (args.area, args.action) == ('cell', 'list'):
            cells = []
            for path in sorted(Path(self.p['state_root']).glob('*/workspace.json')):
                workspace = read_json(path)
                cells.append({'name': path.parent.name, 'cell_name': workspace['name'], 'ssh': workspace['ssh'], 'config': str(path),
                              'status': 'not_checked', 'setup': setup_record(path, workspace)})
            return {'ok': True, 'cells': cells}
        if args.area == 'access' and args.action != 'list':
            return self.repo() if args.kind == 'repo' else self.network()
        if (args.area, args.action) == ('cell', 'delete'):
            return self.delete()
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
        if getattr(args, 'transfers', None):
            return self.transfer_items(args)
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
        if args.area == 'file' and getattr(args, 'paste', False):
            uploaded = json.loads(self.cli(*words, '--json'))
            return {'ok': True, 'cell': self.local['name'], 'operation': args.action,
                    'source': None, 'destination': uploaded['destination'], 'chat': uploaded.get('chat')}
        self.cli(*words)
        return {'ok': True, 'cell': self.local['name'], 'operation': args.action,
                'source': args.source, 'destination': args.destination}


def management_text(report):
    if 'cells' in report:
        def setup(record):
            if record['state'] != 'incomplete':
                return {'ready': 'ready', 'not_recorded': 'no setup record'}[record['state']]
            made = [STEP_NAMES[item['kind']] + (' (outcome unknown)' if item['state'] == 'unknown' else '')
                    for item in record['created']]
            return 'setup incomplete; created: ' + (', '.join(made) or 'nothing')
        return '\n'.join(['Saved cells (from local setup records; live status not checked):', *[
            f"  {cell['name']:<24} {cell['ssh']:<32} {setup(cell['setup'])}" for cell in report['cells']]])
    if 'steps' in report:
        done = report['deleted']
        wording = {'revoke': 'revoke', 'revoked': 'revoked', 'delete': 'delete (permanent: files, chats and agent logins)',
                   'deleted': 'deleted', 'absent': 'already gone', 'remove': 'remove', 'removed': 'removed',
                   'retained': 'kept at the broker; it authorizes nothing without grants',
                   'not_managed': 'not created by Kuggis; left running'}
        lines = [f"{'Deleted' if done else 'Plan: delete'} {report['cell']}", *[
            f"  {STEP_NAMES[step['kind']]}{' ' + step['name'] if step.get('name') else ''}: {wording[step['result']]}"
            for step in report['steps']]]
        lines.append('  Agent sessions are not signed out at the provider.' if done else
                     f"Nothing was deleted. To delete: kuggis {report['cell']} --delete --yes")
        return '\n'.join(lines)
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
        for agent, entry in report.get('agents', {}).items():
            lines.append(f"  {agent}: " + (f"{entry['pinned']} already installed" if entry['action'] == 'none' else
                         f"install {entry['pinned']} (currently {entry['installed'] or 'not installed'})"))
        if report.get('registered_agents'):
            lines.append('  Registers agents: ' + ', '.join(report['registered_agents'])
                         + '. Sign each in with kuggis login CELL --agent AGENT.')
        if report.get('agents_unmanaged'):
            lines.append('  Agents: ' + report['agents_unmanaged'] + '.')
        if report.get('removed_model_gateway'):
            lines.append('  Removes the exe model gateway grant ('+', '.join(report['removed_model_gateway'])+'). Agents still using it need kuggis login CELL --agent AGENT; history is kept.')
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
        note = ('\n  Removed the exe model gateway grant; sign agents in with kuggis login CELL --agent AGENT.'
                if report.get('removed_model_gateway') else '')
        installed = [f"{agent} {entry['pinned']}" for agent, entry in report.get('agents', {}).items()
                     if entry['action'] == 'install']
        if installed:
            note += '\n  Installed ' + ', '.join(installed) + '.'
        if report.get('registered_agents'):
            note += ('\n  Registered ' + ', '.join(report['registered_agents'])
                     + '; sign in with kuggis login CELL --agent AGENT.')
        return f"{report['cell']}: configuration applied. Restart: {report['restart']}.{note}"
    if 'archived' in report:
        return f"Archived {report['archived']} in {report['cell']}."
    if report.get('operation') == 'put' and report.get('destination'):
        note = (f"\nReference inserted into {report['chat']} (not submitted)." if report.get('chat') else '')
        return f"Uploaded: {report['destination']}" + note
    if len(report.get('items', [])) > 1:
        return f"{report['operation']} completed for {report['cell']}: {len(report['items'])} items."
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
        elif args.action == 'delete':
            # Exclude a running create or another management change for this cell.
            with manager.config.with_name('create.lock').open('a') as creating:
                with manager.config.with_name(manager.config.name + '.management.lock').open('a') as managing:
                    for lock in (creating, managing):
                        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
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
