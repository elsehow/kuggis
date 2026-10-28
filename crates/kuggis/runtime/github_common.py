"""Shared GitHub protocol. No credentials in URLs or permissive redirects."""
import json
import os
from pathlib import Path
import re
import stat
import tempfile
import urllib.error
import urllib.parse
import urllib.request

LIMIT = 1024 * 1024


class Failure(Exception):
    def __init__(self, message, status=400):
        super().__init__(message)
        self.status = status


def repository(value):
    if (not isinstance(value, str) or len(value) > 201 or value.lower().endswith('.git')
            or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,99}/[A-Za-z0-9_][A-Za-z0-9_.-]{0,99}', value)):
        raise Failure('expected OWNER/REPO without a URL or .git suffix')
    return value.lower()


def permissions(access, workflows=False):
    if access not in ('read', 'write') or type(workflows) is not bool or (workflows and access != 'write'):
        raise Failure('expected read or write access; workflows requires write')
    result = {'contents': access, 'issues': access, 'pull_requests': access}
    if workflows:
        result['workflows'] = 'write'
    return result


def broker_url(value):
    parsed = urllib.parse.urlsplit(value)
    if (parsed.scheme != 'https' or not parsed.hostname or parsed.username or parsed.password
            or parsed.query or parsed.fragment or parsed.path not in ('', '/')
            or any(c.isspace() for c in value)):
        raise Failure('broker URL must be an HTTPS origin without credentials or a path')
    return value.rstrip('/')


def private_read(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as source:
        info = os.fstat(source.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid()
                or info.st_mode & 0o077 or info.st_size > LIMIT):
            raise Failure('secret must be an owned regular file with mode 0600 or 0400')
        return source.read(LIMIT + 1)


def atomic_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd, temporary = tempfile.mkstemp(prefix='.github-', dir=path.parent)
    try:
        with os.fdopen(fd, 'w') as out:
            json.dump(value, out)
            out.write('\n')
            out.flush()
            os.fsync(out.fileno())
        os.replace(temporary, path)
        parent = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(parent)
        finally:
            os.close(parent)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise Failure('redirect refused', 502)


def request(url, token, data=None, method='POST', *, trusted_errors=False, timeout=30):
    if not isinstance(token, str) or not token or any(c.isspace() for c in token):
        raise Failure('invalid credential')
    req = urllib.request.Request(url, method=method,
        data=json.dumps(data).encode() if data is not None else None,
        headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json',
                 'Accept': 'application/vnd.github+json', 'User-Agent': 'kuggis-github',
                 'X-GitHub-Api-Version': '2022-11-28'})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    try:
        with opener.open(req, timeout=timeout) as response:
            raw = response.read(LIMIT + 1)
            if len(raw) > LIMIT:
                raise Failure('upstream response too large', 502)
            return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as error:
        if trusted_errors:
            try:
                message = json.loads(error.read(4096)).get('error')
                if isinstance(message, str) and len(message) <= 1000:
                    raise Failure(message, error.code)
            except (ValueError, AttributeError):
                pass
        raise Failure('upstream request failed (HTTP %d)' % error.code, error.code) from None
    except (OSError, ValueError):
        raise Failure('upstream connection or response failed', 502) from None


def enable_network(workspace, url=None, transport='github'):
    """Transport reachability only; authenticated authority lives in the broker."""
    resources = workspace.setdefault('resources', [])
    if any(e.get('host') == 'github.int.exe.xyz' for r in resources for e in r.get('endpoints', [])):
        raise Failure('remove the legacy github.int.exe.xyz resource and its exe attachment before connecting')
    names = {r['name'] for r in resources}
    present = {(e['host'], e['port']) for r in resources for e in r.get('endpoints', [])}
    if transport == 'ssh-gateway':
        parsed = urllib.parse.urlsplit(broker_url(url))
        targets = [(parsed.hostname, parsed.port or 443)]
    elif transport == 'github':
        targets = [('github.com', 443), ('api.github.com', 443)]
    else:
        raise Failure('unknown GitHub transport')
    endpoints = [{'host': host, 'port': port} for host, port in targets if (host, port) not in present]
    if endpoints:
        name = 'kuggis-github'
        while name in names:
            name += '-transport'
        resources.append({'name': name, 'endpoints': endpoints, 'credentials': []})
    return workspace
