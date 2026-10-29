"""Laptop browser orchestration; tokens and native refresh stay in the guest."""
import argparse
import base64
import copy
import fcntl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import selectors
import subprocess
import sys
import threading
import time
from urllib.parse import parse_qs, urlsplit
import webbrowser

try:
    Management
except NameError:
    from manage import Management, ManagementParser, endpoint
    from github_common import Failure

CODEX_LOGIN_HOSTS = ('auth.openai.com', 'chatgpt.com')


def login_options(argv):
    parser = ManagementParser(prog='kuggis login', allow_abbrev=False)
    parser.add_argument('cell')
    parser.add_argument('--agent', required=True, choices=('codex',))
    parser.add_argument('--no-open', action='store_true')
    parser.add_argument('--force', action='store_true')
    parser.add_argument('--status', action='store_true')
    parser.add_argument('--plan', action='store_true')
    parser.add_argument('--json', action='store_true')
    args = parser.parse_args(argv)
    if args.status and (args.force or args.plan):
        raise Failure('--status cannot be combined with --force or --plan')
    return args


def guest_operation(payload, mode, request):
    result = subprocess.run(payload['guest'][mode], input=json.dumps(request).encode() + b'\n',
                            capture_output=True, timeout=120)
    try:
        report = json.loads(result.stdout)
    except (ValueError, UnicodeDecodeError):
        raise Failure('could not reach the cell login adapter; check SSH connectivity') from None
    if result.returncode or report.get('event') == 'error':
        raise Failure(report.get('message', 'cell login adapter failed'))
    return report


def login_network(manager, plan=False, hosts=CODEX_LOGIN_HOSTS):
    snapshot = manager.snapshot()
    workspace = snapshot['workspace']
    agents = workspace.get('agents', {})
    if 'codex' not in agents and not any(word.endswith('/kuggis-codex') for word in workspace.get('launch', [])):
        raise Failure('Codex is not configured in this cell; install the Codex recipe first')
    candidate = copy.deepcopy(workspace)
    allowed = {(e['host'], e['port']) for r in candidate.get('resources', []) for e in r.get('endpoints', [])}
    missing = [] if candidate.get('egress') == 'internet' else [h for h in hosts if (h, 443) not in allowed]
    if missing:
        resource = next((r for r in candidate.get('resources', []) if r['name'] == 'codex-login'), None)
        if resource is None:
            resource = {'name': 'codex-login', 'endpoints': [], 'credentials': []}
            candidate.setdefault('resources', []).append(resource)
        resource.setdefault('endpoints', []).extend(endpoint(host + ':443') for host in missing)
    if plan:
        return {'event': 'plan', 'ok': True, 'cell': workspace['name'], 'agent': 'codex',
                'network_grants': [h + ':443' for h in missing],
                'restart': 'proxy' if missing else 'none', 'cell_restart': False,
                'authentication': 'shared with every Codex chat and shell', 'history': 'retained in place'}
    if missing:
        manager.apply_network(snapshot, candidate)
    for host in hosts:
        if not manager.remote('probe', host=host, port=443).get('reachable'):
            raise Failure('cannot reach ' + host + ':443 through the cell proxy; login was not started')
    return missing


class CallbackServer(ThreadingHTTPServer):
    daemon_threads = True
    # Permit a new login after the previous socket enters TIME_WAIT. This does
    # not permit binding over an active listener (SO_REUSEPORT is never used).
    allow_reuse_address = True


def callback_server(payload, authorization_url):
    parsed = urlsplit(authorization_url)
    if parsed.scheme != 'https' or parsed.hostname != 'auth.openai.com':
        raise Failure('unexpected native authorization origin')
    query = parse_qs(parsed.query)
    redirect = urlsplit(query.get('redirect_uri', [''])[0])
    if (redirect.scheme != 'http' or redirect.hostname != '127.0.0.1'
            or redirect.port not in (1455, 1457) or redirect.path != '/auth/callback'):
        raise Failure('unexpected native callback address')
    port = redirect.port

    class CallbackHandler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass  # Callback query strings carry authorization codes.

        def do_GET(self):
            path = urlsplit(self.path)
            if (path.scheme or path.netloc or path.path not in ('/auth/callback', '/success')
                    or self.headers.get('Host') not in ('127.0.0.1:' + str(port), 'localhost:' + str(port))):
                self.send_error(404)
                return
            try:
                report = guest_operation(payload, 'callback', {'port': port, 'path': self.path})
                body = base64.b64decode(report['body'], validate=True)
                self.send_response(report['status'])
                for name, value in report['headers']:
                    if name.lower() in ('content-type', 'location') and '\r' not in value and '\n' not in value:
                        self.send_header(name, value)
                self.send_header('Cache-Control', 'no-store')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            except Exception:
                self.send_error(502, 'Could not deliver the callback to the cell. Check the login terminal.')

    try:
        server = CallbackServer(('127.0.0.1', port), CallbackHandler)
    except OSError:
        raise Failure('local port ' + str(port) + ' is occupied; finish or close the other Codex browser login, then retry') from None
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server, thread


def login_browser(payload, args):
    child = subprocess.Popen(payload['guest']['login'], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL, bufsize=0)
    selector = selectors.DefaultSelector()
    selector.register(child.stdout, selectors.EVENT_READ)
    server, thread = None, None
    try:
        child.stdin.write(json.dumps({'force': args.force}).encode() + b'\n')
        child.stdin.flush()
        deadline = time.monotonic() + 660
        while time.monotonic() < deadline:
            if not selector.select(1):
                continue
            line = child.stdout.readline(2 * 1024 * 1024 + 1)
            if not line:
                raise Failure('cell login connection closed before completion; retry kuggis login')
            if len(line) > 2 * 1024 * 1024:
                raise Failure('login response exceeds the protocol limit')
            event = json.loads(line)
            if event.get('event') == 'error':
                raise Failure(event['message'])
            if event.get('event') == 'authorize':
                if server:
                    raise Failure('unexpected second authorization request')
                server, thread = callback_server(payload, event['url'])
                if args.json:
                    print(json.dumps(event), flush=True)
                else:
                    print('Sign in for ' + args.cell + ' in your browser:\n' + event['url'], flush=True)
                if not args.no_open:
                    try:
                        webbrowser.open(event['url'])
                    except (OSError, webbrowser.Error):
                        print('Open the URL above in your laptop browser.', file=sys.stderr)
            elif event.get('event') == 'complete':
                return event
        raise Failure('browser login timed out; rerun kuggis login')
    finally:
        selector.close()
        child.stdin.close()  # EOF tells the guest to cancel and close its listener.
        try:
            child.wait(timeout=10)
        except subprocess.TimeoutExpired:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        if server:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)


def login_main(payload):
    json_output = '--json' in payload['args']
    try:
        args = login_options(payload['args'])
        if args.status:
            report = guest_operation(payload, 'status', {})
            report.update(cell=args.cell, agent=args.agent, ok=True)
        else:
            manager = Management(payload, None)
            with manager.config.with_name(manager.config.name + '.management.lock').open('a') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                if args.plan:
                    report = login_network(manager, plan=True)
                else:
                    # Check/install the adapter before changing outbound policy.
                    guest_operation(payload, 'prepare', {'launcher': payload['launcher']})
                    missing = login_network(manager)
                    if missing and not args.json:
                        print('Enabled Codex HTTPS access; the cell and running chats stayed running.', flush=True)
                    report = login_browser(payload, args)
                    origin = report.get('backend_origin')
                    if origin:
                        route = urlsplit(origin)
                        if (route.scheme != 'https' or not route.hostname or route.username or route.password
                                or route.port not in (None, 443) or route.path not in ('', '/') or route.query or route.fragment):
                            raise Failure('Codex returned an unsupported workspace backend origin')
                        # Native account discovery determines the organization's
                        # backend. Grant that exact public destination too.
                        manager = Management(payload, None)
                        login_network(manager, hosts=(route.hostname,))
                    report.update(ok=True, cell=args.cell, agent=args.agent)
        if args.json:
            print(json.dumps(report), flush=True)
        elif report['event'] == 'plan':
            print('Login plan for ' + args.cell + ': shared Codex account; existing history retained.\n'
                  '  Network grants: ' + (', '.join(report['network_grants']) or 'already configured') + '\n'
                  '  Restart: ' + report['restart'] + '; cell and chat processes stay running.')
        elif report['event'] == 'status':
            print(args.cell + ': ' + ('signed in to ChatGPT' if report['authenticated'] else 'not signed in to ChatGPT')
                  + '; provider: ' + str(report['provider']))
        else:
            print(args.cell + ': ' + ('existing ChatGPT login verified' if report['reused'] else 'signed in to ChatGPT')
                  + '. All Codex chats and shells share this login.\n'
                  'Exit and reopen already-running Codex chats to use it; their history is retained.')
        return 0
    except (Exception, KeyboardInterrupt) as error:
        message = (str(error) if isinstance(error, (Failure, ValueError)) else
                   'login cancelled' if isinstance(error, KeyboardInterrupt) else
                   'another cell operation is in progress' if isinstance(error, BlockingIOError) else
                   'login failed; check cell connectivity and rerun kuggis login')
        if json_output:
            print(json.dumps({'ok': False, 'error': {'code': 'login_failed', 'message': message}}))
        else:
            print('kuggis: ' + message, file=sys.stderr)
        return 2
