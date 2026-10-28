"""Repository-bound GitHub collaboration operations. No caller-selected URLs or GraphQL."""
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
from urllib.parse import urlencode
from github_common import Failure, private_read, repository, request

API_LIMIT = 65536
LEVELS = ('none', 'read', 'write')


def api_permissions(issues, pulls, merge=False):
    if issues not in LEVELS or pulls not in LEVELS or type(merge) is not bool or (merge and pulls != 'write'):
        raise Failure('API permissions must be none/read/write; merge requires pull-request write')
    return {'issues': issues, 'pull_requests': pulls, 'merge': merge}


class GitHubAPI:
    def __init__(self, config_dir):
        self.config_dir = Path(config_dir)
        if not self.config_dir.is_absolute():
            raise Failure('gh_config_dir must be absolute')
        self.gh = shutil.which('gh', path='/usr/local/bin:/usr/bin:/bin')

    def token(self):
        try:
            info = self.config_dir.lstat()
            if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o077:
                raise Failure('broker gh configuration directory must be owned and mode 0700')
            private_read(self.config_dir / 'hosts.yml')
        except FileNotFoundError:
            raise Failure('GitHub API login is missing; run the broker GitHub login command first', 503) from None
        if self.gh is None:
            raise Failure('install GitHub CLI on the broker before API login', 503)
        result = subprocess.run([self.gh, 'auth', 'token', '--hostname', 'github.com'],
            env={'PATH': '/usr/bin:/bin', 'HOME': str(self.config_dir.parent),
                 'GH_CONFIG_DIR': str(self.config_dir), 'GH_PROMPT_DISABLED': '1', 'GH_HOST': 'github.com'},
            cwd=self.config_dir, capture_output=True, timeout=15)
        token = result.stdout.decode().strip()
        if result.returncode or not token or len(token) > 1024 or any(c.isspace() for c in token):
            raise Failure('GitHub API login is unavailable; log in again on the broker', 503)
        return token

    def session(self):
        token = self.token()
        def call(path, method='GET', body=None):
            # Only this module constructs paths; never follow redirects or caller URLs.
            try:
                return request('https://api.github.com' + path, token, body, method)
            except Failure as error:
                if error.status in (401, 403, 404, 422):
                    raise Failure('GitHub API returned HTTP %d; check login, repository permissions, SSO, and operation inputs'
                                  % error.status, error.status) from None
                raise
        return call

    def status(self):
        result = self.session()('/user')
        return {'authenticated': True, 'login': result['login']}


def fields(value, allowed, required=()):
    if not isinstance(value, dict) or set(value) - set(allowed) or not set(required) <= set(value):
        raise Failure('unknown or missing operation fields')


def string(value, limit=30000, empty=True):
    if not isinstance(value, str) or len(value) > limit or '\x00' in value or (not empty and not value.strip()):
        raise Failure('invalid text field')
    return value


def number(value):
    if type(value) is not int or not 1 <= value <= 2147483647:
        raise Failure('expected a positive issue/PR number or page')
    return value


def branch(value):
    if (not isinstance(value, str) or not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_./-]{0,199}', value)
            or '..' in value or '//' in value or value.endswith(('/', '.', '.lock'))):
        raise Failure('expected a branch name; specify a fork separately with --head-repo')
    return value


def plan(kind, action, params):
    """Validate complete input before obtaining upstream credentials or making requests."""
    if not isinstance(kind, str) or kind not in ('issue', 'pr') or not isinstance(action, str):
        raise Failure('only issue and pr operations are supported')
    read = {'list', 'view', 'comments'} | ({'diff', 'reviews'} if kind == 'pr' else set())
    write = {'create', 'edit', 'close', 'reopen', 'comment'} | ({'review', 'merge'} if kind == 'pr' else set())
    if action not in read | write:
        raise Failure('unsupported collaboration operation')
    allowed = {'number'}
    required = {'number'}
    if action == 'list':
        allowed, required = {'state', 'limit', 'page'}, set()
    elif action == 'create':
        allowed, required = {'title', 'body'}, {'title'}
        if kind == 'pr':
            allowed |= {'base', 'head', 'head_repo', 'draft'}
            required |= {'base', 'head'}
    elif action == 'edit':
        allowed |= {'title', 'body'}
    elif action in ('comments', 'reviews', 'diff'):
        allowed |= {'limit', 'page'}
    elif action == 'comment':
        allowed |= {'body'}
        required |= {'body'}
    elif action == 'review':
        allowed |= {'body', 'event'}
        required |= {'event'}
    elif action == 'merge':
        allowed |= {'method', 'sha'}
        required |= {'sha'}
    fields(params, allowed, required)
    if 'number' in params:
        number(params['number'])
    for name in ('title', 'body'):
        if name in params:
            string(params[name], 256 if name == 'title' else 30000, empty=name != 'title' and action != 'comment')
    if action == 'edit' and not (set(params) & {'title', 'body'}):
        raise Failure('edit requires --title or --body')
    if 'state' in params and params['state'] not in ('open', 'closed', 'all'):
        raise Failure('state must be open, closed, or all')
    for key in ('limit', 'page'):
        if key in params:
            number(params[key])
    if params.get('limit', 30) > 100:
        raise Failure('limit must be between 1 and 100; use --page for more results')
    for key in ('base', 'head'):
        if key in params:
            branch(params[key])
    if 'head_repo' in params:
        repository(params['head_repo'])
    if 'draft' in params and type(params['draft']) is not bool:
        raise Failure('draft must be boolean')
    if action == 'review' and (params['event'] not in ('APPROVE', 'REQUEST_CHANGES', 'COMMENT')
            or (params['event'] != 'APPROVE' and not params.get('body', '').strip())):
        raise Failure('review requires an event and a body for comments/change requests')
    if action == 'merge' and (params.get('method', 'merge') not in ('merge', 'squash', 'rebase')
            or not re.fullmatch('[a-f0-9]{40}', params['sha'] if isinstance(params['sha'], str) else '')):
        raise Failure('merge requires the expected 40-character head SHA and a valid method')
    return 'read' if action in read else 'write'


def projection(item):
    """Return collaboration data, never credentials, API URLs, or nested repository objects."""
    if isinstance(item, list):
        return [projection(row) for row in item]
    if not isinstance(item, dict):
        raise Failure('invalid GitHub API response', 502)
    keys = ('number', 'id', 'title', 'body', 'state', 'draft', 'html_url', 'created_at', 'updated_at',
            'merged', 'mergeable', 'sha', 'message', 'commit_id', 'submitted_at', 'filename',
            'status', 'additions', 'deletions', 'changes', 'patch')
    result = {key: item[key] for key in keys if key in item}
    if isinstance(item.get('user'), dict):
        result['author'] = item['user'].get('login')
    for key in ('head', 'base'):
        if isinstance(item.get(key), dict):
            result[key] = {k: item[key][k] for k in ('ref', 'sha', 'label') if k in item[key]}
    return result


def execute(call, repo, kind, action, params, resolve):
    """resolve binds numeric repository IDs and checks fork grants for the same cell."""
    base_repo = resolve(repo, call)
    prefix = '/repos/' + repo
    collection = '/issues' if kind == 'issue' else '/pulls'
    paging = urlencode({'per_page': params.get('limit', 30), 'page': params.get('page', 1)})
    if action == 'list':
        rows = call(prefix + collection + '?' + paging + '&state=' + params.get('state', 'open'))
        if not isinstance(rows, list):
            raise Failure('invalid GitHub list response', 502)
        if kind == 'issue':
            rows = [row for row in rows if 'pull_request' not in row]
        return projection(rows)
    if action == 'create':
        body = {key: params[key] for key in ('title', 'body', 'base', 'draft') if key in params}
        if kind == 'pr':
            head_repo = repository(params.get('head_repo', repo))
            if head_repo == repo:
                body['head'] = params['head']
            else:
                head = resolve(head_repo, call)
                base_network = base_repo.get('source', {}).get('id', base_repo['id'])
                head_network = head.get('source', {}).get('id', head['id'])
                if base_network != head_network:
                    raise Failure('PR head repository is not in the same fork network')
                body['head'] = head_repo.split('/')[0] + ':' + params['head']
                body['head_repo'] = head_repo.split('/')[1]
        return projection(call(prefix + collection, 'POST', body))

    target = prefix + collection + '/' + str(params['number'])
    item = call(target)
    if kind == 'issue' and 'pull_request' in item:
        raise Failure('this number is a pull request; use pr permissions and commands', 403)
    if kind == 'pr' and item.get('base', {}).get('repo', {}).get('id') != base_repo['id']:
        raise Failure('pull request repository identity mismatch', 502)
    if action == 'view':
        return projection(item)
    if action in ('comments', 'comment'):
        path = prefix + '/issues/' + str(params['number']) + '/comments'
        return projection(call(path + '?' + paging) if action == 'comments'
                          else call(path, 'POST', {'body': params['body']}))
    if action in ('reviews', 'diff'):
        return projection(call(target + ('/reviews?' if action == 'reviews' else '/files?') + paging))
    if action == 'review':
        body = {key: params[key] for key in ('body', 'event') if key in params}
        return projection(call(target + '/reviews', 'POST', body))
    if action == 'merge':
        return projection(call(target + '/merge', 'PUT', {'sha': params['sha'], 'merge_method': params.get('method', 'merge')}))
    body = ({'state': 'closed' if action == 'close' else 'open'} if action in ('close', 'reopen')
            else {key: params[key] for key in ('title', 'body') if key in params})
    return projection(call(target, 'PATCH', body))
