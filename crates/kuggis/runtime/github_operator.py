"""Operator effects. Admin authority stays on the operator, never on cell hosts."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import subprocess
import sys
from github_common import Failure, atomic_json, broker_url, enable_network, permissions, private_read, repository, request


def options(args):
    parser = argparse.ArgumentParser(prog='kuggis')
    groups = parser.add_subparsers(dest='group', required=True)
    github = groups.add_parser('github').add_subparsers(dest='action', required=True)
    github.add_parser('serve', help='run the trusted HTTPS broker').add_argument('--config', required=True)
    connect = github.add_parser('connect')
    connect.add_argument('--url', required=True)
    connect.add_argument('--admin-token-file', required=True)
    attach = github.add_parser('attach')
    attach.add_argument('workspace')
    check = github.add_parser('check')
    check.add_argument('--repo', required=True)
    check.add_argument('--access', choices=('read', 'write'), default='read')
    github.add_parser('api-status', help='check the broker GitHub API login')
    repos = groups.add_parser('repo').add_subparsers(dest='action', required=True)
    for action in ('list', 'add', 'remove', 'api'):
        command = repos.add_parser(action)
        command.add_argument('workspace')
        command.add_argument('--json', action='store_true')
        if action != 'list':
            command.add_argument('repo')
        if action == 'add':
            grant = command.add_mutually_exclusive_group(required=True)
            grant.add_argument('--role', choices=('read', 'develop'),
                               help='set Git and PR/issue access together; disables API merging')
            grant.add_argument('--access', choices=('read', 'write'),
                               help='set Git access only; preserves existing API permissions')
            command.add_argument('--workflows', action='store_true')
        if action == 'api':
            command.add_argument('--issues', choices=('none', 'read', 'write'), required=True)
            command.add_argument('--pull-requests', choices=('none', 'read', 'write'), required=True)
            command.add_argument('--merge', action='store_true', help='also permit PR merging; omitted means disabled')
    result = parser.parse_args(args)
    if getattr(result, 'role', None) and result.workflows:
        parser.error('--workflows cannot be combined with --role')
    return result


class Operator:
    def __init__(self, payload):
        self.payload = payload
        self.settings = Path(payload['settings'])

    def configured(self):
        try:
            config = json.loads(private_read(self.settings))
        except FileNotFoundError:
            raise Failure('configure GitHub first: kuggis github connect --url HTTPS_ORIGIN --admin-token-file FILE') from None
        url = broker_url(config['url'])
        token = private_read(config['admin_token_file']).decode().strip()
        return url, token

    def api(self, path, value):
        url, token = self.configured()
        return request(url + path, token, value, trusted_errors=True, timeout=150)

    def remote(self, verb, data=None):
        result = subprocess.run(self.payload['ssh'][verb], input=json.dumps(data).encode() if data else b'',
                                capture_output=True, timeout=120)
        if result.returncode:
            raise Failure('host GitHub operation failed; ensure the updated Kuggis runtime is installed and retry')
        return json.loads(result.stdout)

    def attach(self):
        config = Path(self.payload['config'])
        url, _ = self.configured()
        transport = self.api('/admin/status', {}).get('transport', 'github')
        workspace = enable_network(json.loads(config.read_text()), url, transport)
        pending = config.with_name(config.name + '.github-enrollment.json')
        receipt = config.with_name(config.name + '.github.json')
        state = self.remote('status')
        if state['connected']:
            if state['url'] != url:
                raise Failure('host is connected to a different broker')
            if state.get('transport', 'github') != transport:
                raise Failure('host is connected to a different transport; re-enroll using a separate broker identity')
            # Repeat host attach when a previous acknowledgement/configuration step was lost.
            enrollment = json.loads(private_read(pending)) if pending.exists() else None
        else:
            if pending.exists():
                enrollment = json.loads(private_read(pending))
            else:
                enrollment = {**self.api('/admin/cells', {'label': workspace['name']}), 'url': url}
                atomic_json(pending, enrollment)
        if enrollment:
            if enrollment['url'] != url:
                raise Failure('pending enrollment belongs to another broker')
            state = self.remote('attach', enrollment)
        if not state['connected']:
            raise Failure('host enrollment was not confirmed')
        self.api('/admin/repos/list', {'cell': state['cell']})
        atomic_json(config, workspace)
        atomic_json(receipt, {'url': url, 'cell': state['cell']})
        pending.unlink(missing_ok=True)
        return {'connected': True, 'cell': state['cell'], 'workspace': workspace['name']}

    def run(self, args):
        if args.group == 'github' and args.action == 'api-status':
            return self.api('/admin/api/status', {})
        if args.group == 'github' and args.action == 'connect':
            url = broker_url(args.url)
            path = str(Path(args.admin_token_file).absolute())
            request(url + '/admin/status', private_read(path).decode().strip(), {})
            atomic_json(self.settings, {'url': url, 'admin_token_file': path})
            return {'connected': True, 'url': url}
        if args.group == 'github' and args.action == 'check':
            return self.api('/admin/resolve', {'repo': repository(args.repo), 'access': args.access, 'workflows': False})
        if args.group == 'github' and args.action == 'attach':
            # Serialize enrollment and both local publications. Locks contain no secrets.
            lock_path = self.payload['config'] + '.github.lock'
            fd = os.open(lock_path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
            with os.fdopen(fd, 'w') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX)
                return self.attach()
        config = Path(self.payload['config'])
        try:
            cell = json.loads(private_read(config.with_name(config.name + '.github.json')))
        except FileNotFoundError:
            raise Failure('connect this cell first: kuggis github attach CONFIG_OR_NAME') from None
        if cell['url'] != self.configured()[0]:
            raise Failure('cell belongs to another broker; reconnect the operator to that broker')
        body = {'cell': cell['cell']}
        if args.action != 'list':
            body['repo'] = repository(args.repo)
        if args.action == 'add':
            if args.role:
                body.update(role=args.role)
            else:
                permissions(args.access, args.workflows)
                body.update(access=args.access, workflows=args.workflows)
        if args.action == 'api':
            body.update(issues=args.issues, pull_requests=args.pull_requests, merge=args.merge)
        try:
            return self.api('/admin/repos/' + args.action, body)
        except Failure as error:
            if (args.action == 'add' and args.role and error.status == 404
                    and str(error) == 'unknown operation or fields'):
                raise Failure('broker does not support repository roles; upgrade the broker and retry (no grant changed)') from None
            raise


def main(payload):
    try:
        args = options(payload['args'])
        print(json.dumps(Operator(payload).run(args), indent=2))
        return 0
    except Failure as error:
        print('kuggis github: ' + str(error), file=sys.stderr)
        if error.status == 503 and 'revocation' in str(error):
            print('A grant change may be saved with revocation pending. Run repo list and retry the change.', file=sys.stderr)
        return 2
    except Exception:
        print('kuggis github: operation failed; check configuration and host connectivity, then retry', file=sys.stderr)
        return 2
