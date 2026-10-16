"""Observe, plan, apply, verify. All service mutations live behind plan approval."""
import base64
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

from .policy import CHECKS, contract, digest, instructions, review_workflow, workflow

ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = '.github/workflows/kuggis-cell.yml'
REVIEW_WORKFLOW = '.github/workflows/kuggis-cell-review.yml'
RUNTIME = ['agents.sh', 'session.sh', 'run.sh', 'hook.sh', 'events.py', 'pr-gate.py', 'adopt-job.py',
           'review.py', 'configure-hooks.py', 'pi-extension.ts', 'needs-you', 'bootstrap.sh', 'CLAUDE.md']


class Failure(ValueError):
    pass


def run(argv, *, data=None, cwd=None, timeout=180):
    proc = subprocess.run([str(a) for a in argv], input=data, text=True, capture_output=True,
                          cwd=cwd, timeout=timeout)
    if proc.returncode:
        # Never include stdin: it may carry an approved private artifact.
        raise Failure(f'{argv[0]} failed: {proc.stderr.strip()[:800]}')
    return proc.stdout


class Services:
    def api(self, path, method='GET', body=None, missing=False):
        args = ['gh', 'api', '--hostname', 'github.com', '--method', method, path]
        if body is not None:
            args += ['--input', '-']
        try:
            out = run(args, data=json.dumps(body) if body is not None else None, timeout=30)
            return json.loads(out) if out.strip() else None
        except Failure as exc:
            if missing and '(HTTP 404)' in str(exc):
                return None
            raise

    def ssh(self, vm, program):
        return json.loads(run(['ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10',
                               vm + '.exe.xyz', 'python3 -'], data=program, timeout=30))

    def machines(self):
        return run(['ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', 'exe.dev', 'ls'], timeout=30)

    def local(self, *args):
        return run([ROOT / 'kuggis', *args])

    def open_setup_pr(self, project, body):
        # The owner must be able to approve the bootstrap PR under an existing
        # review requirement. GitHub forbids approving one's own PR, so create
        # it as the repository-scoped VM bot, without copying operator credentials.
        command = (f"env GH_HOST=github.int.exe.xyz gh api repos/{project['repo']}/pulls "
                   '--method POST --input -')
        return json.loads(run(['ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10',
                               project['vm'] + '.exe.xyz', command], data=json.dumps(body), timeout=30))


def atomic_json(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    # Audits can run concurrently with the dashboard or another terminal.
    with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, delete=False) as f:
        tmp = Path(f.name)
        try:
            f.write(json.dumps(data, indent=2) + '\n')
            f.flush()
            tmp.replace(path)
        finally:
            tmp.unlink(missing_ok=True)


@contextlib.contextmanager
def lock(config):
    config.mkdir(parents=True, exist_ok=True)
    with (config / 'cell-control.lock').open('a') as f:
        fcntl.flock(f, fcntl.LOCK_EX)
        yield


def mapping(config):
    result = {}
    path = config / 'vms'
    for line in path.read_text().splitlines() if path.exists() else []:
        fields = line.split()
        if not fields or fields[0].startswith('#'):
            continue
        if len(fields) != 4 or fields[0] in result:
            raise Failure('invalid or duplicate project map row')
        result[fields[0]] = dict(zip(('vm', 'repo', 'profile'), fields[1:]))
    return result


def check_ownership(config, policy):
    """No policy can steal an existing project/VM/repository or share .box configs."""
    rows = mapping(config)
    for p in policy['projects']:
        if p['profile'] != '-' and not (ROOT/'profiles'/(p['profile'] + '.sh')).is_file():
            raise Failure(f"{p['name']}: configured profile is not installed")
        desired = {k: p[k] for k in ('vm', 'repo', 'profile')}
        if p['name'] in rows and rows[p['name']] != desired:
            raise Failure(f"{p['name']}: existing project map conflicts with contract")
        for name, row in rows.items():
            if name != p['name'] and (row['vm'] == p['vm'] or row['repo'].lower() == p['repo'].lower()):
                raise Failure(f"{p['name']}: VM/repository already belongs to {name}")
    for path in (config / 'cells').glob('*.json'):
        if path.stem == policy['cell']:
            continue
        other = json.loads(path.read_text())
        projects = other.get('projects', [])
        other_names = {p['name'] if isinstance(p, dict) else p for p in projects}
        if other_names & {p['name'] for p in policy['projects']}:
            raise Failure('a project cannot belong to two cells')
        if other.get('version') == 2:
            for a in other['projects']:
                if any(a['vm'] == b['vm'] or a['repo'].lower() == b['repo'].lower() for b in policy['projects']):
                    raise Failure('a VM/repository cannot belong to two cells')


def vm_program(project):
    # This only returns enums, public repo URL and hashes, never VM file contents.
    return '''import hashlib,json,pathlib,shutil,subprocess
h=pathlib.Path.home(); b=h/'.box'
files=FILES
hashes={f:hashlib.sha256((b/f).read_bytes()).hexdigest() if (b/f).is_file() else None for f in files}
try: contract=json.loads((b/'cell-contract.json').read_text())
except (OSError,ValueError): contract=None
try: origin=subprocess.check_output(['git','-C',str(h/REPO),'remote','get-url','origin'],text=True,stderr=subprocess.DEVNULL).strip()
except subprocess.CalledProcessError: origin=None
notes=(b/'cell-instructions.md').read_text() if (b/'cell-instructions.md').exists() else ''
instructions={p:bool(notes) and notes in (h/p).read_text() if (h/p).exists() else False for p in ['.claude/CLAUDE.md','.codex/AGENTS.md','.pi/agent/AGENTS.md']}
hooks={}
for agent,path in [('claude','.claude/settings.json'),('codex','.codex/hooks.json')]:
 try:
  data=json.loads((h/path).read_text())['hooks']
  hooks[agent]=all(any('.box/hook.sh ' in hook.get('command','') for group in data.get(event,[]) for hook in group.get('hooks',[])) for event in ['SessionStart','UserPromptSubmit','PostToolUse','Stop'])
 except (OSError,ValueError,KeyError,TypeError): hooks[agent]=False
print(json.dumps(dict(hashes=hashes,contract=contract,origin=origin,instructions=instructions,notes_hash=hashlib.sha256(notes.encode()).hexdigest(),hooks=hooks,agents={a:bool(shutil.which(a)) for a in ['claude','codex','pi']})))
'''.replace('FILES', repr(RUNTIME)).replace('REPO', repr(project['repo'].split('/')[1]))


def expected_hashes():
    return {f: hashlib.sha256((ROOT/'vm'/f).read_bytes()).hexdigest() for f in RUNTIME}


def observe(policy, config, services):
    check_ownership(config, policy)
    rows, results = mapping(config), {}
    machines = services.machines()
    for p in policy['projects']:
        name, repo, branch = p['name'], p['repo'], p['branch']
        result = {'issues': [], 'actions': [], 'facts': {}}
        issues, actions, facts = result['issues'], result['actions'], result['facts']
        meta = services.api('repos/' + repo, missing=True)
        if meta is None:
            upstream = services.api('repos/' + p['upstream'])
            if upstream.get('visibility') != 'public':
                raise Failure('only public development repositories are supported')
            # Creation always uses the signed-in account; never guess an organization.
            user = services.api('user')['login']
            if repo.split('/')[0].lower() != user.lower() or repo.split('/')[1] != p['upstream'].split('/')[1]:
                raise Failure('new forks must use the signed-in owner and upstream repository name')
            issues.append('development fork missing')
            actions.append('fork')
        else:
            if meta.get('visibility') != 'public' or meta.get('archived'):
                raise Failure(f'{repo}: requires a public, active development repository')
            if meta.get('default_branch') != branch:
                raise Failure(f'{repo}: configured base is not its default branch')
            if not meta.get('fork') or (meta.get('parent') or {}).get('full_name', '').lower() != p['upstream'].lower():
                raise Failure(f'{repo}: does not match the explicitly declared upstream fork relationship')
            facts['repo_id'] = meta['id']
            head = services.api(f'repos/{repo}/git/ref/heads/{branch}')['object']['sha']
            facts['head'] = head
            protection = services.api(f'repos/{repo}/branches/{branch}/protection', missing=True)
            # Preserve unrelated restrictions by refusing to silently replace them.
            rules = services.api(f'repos/{repo}/rules/branches/{branch}')
            facts['rules'] = rules
            if rules:
                issues.append('additional rulesets require explicit operator reconciliation')
            if protection and (protection.get('restrictions') or protection.get('required_signatures', {}).get('enabled')
                               or protection.get('lock_branch', {}).get('enabled')):
                issues.append('additional branch restrictions require operator reconciliation')
            facts['protection'] = protection
            files_match = True
            facts['workflow_shas'] = {}
            for path, wanted in workflows(p).items():
                actual_file = services.api(f'repos/{repo}/contents/{path}?ref={head}', missing=True)
                content = base64.b64decode(actual_file['content']).decode() if actual_file else None
                facts['workflow_shas'][path] = actual_file['sha'] if actual_file else None
                files_match = files_match and content == wanted
            if not files_match:
                issues.append('managed test/review workflows need a reviewed PR')
                actions.append('workflow-pr')
            runs = services.api(f'repos/{repo}/actions/runs?head_sha={head}&per_page=100')['workflow_runs']
            candidates = [r for r in runs if r.get('path') == WORKFLOW and r.get('event') == 'push']
            latest = max(candidates, key=lambda r: r['id']) if candidates else None
            green = bool(latest and latest['status'] == 'completed' and latest['conclusion'] == 'success')
            facts['test_run'] = {k: latest[k] for k in ('id', 'status', 'conclusion')} if latest else None
            if not green:
                issues.append('managed tests have not passed on the default-branch head')
            if not protection_matches(protection):
                issues.append('merge permissions differ from cell contract')
                if files_match and green and not rules and not any('restrictions' in i for i in issues):
                    actions.append('protection')
            if not meta.get('allow_squash_merge'):
                issues.append('squash merging is disabled')
                actions.append('enable-squash')
        if p['name'] not in rows:
            if p['vm'] + '.exe.xyz' in machines:
                issues.append('existing unmanaged VM needs explicit enrollment; refusing to overwrite it')
            else:
                issues.append('VM/project registration missing')
                actions.append('new')
        else:
            try:
                remote = services.ssh(p['vm'], vm_program(p))
                facts['vm'] = remote
                good = (remote['contract'] == contract(policy, p)
                        and remote['hashes'] == expected_hashes()
                        and remote['origin'] == 'https://github.int.exe.xyz/' + repo + '.git'
                        and remote['notes_hash'] == contract(policy, p)['instructions_hash']
                        and remote['instructions'] == {f: True for f in ['.claude/CLAUDE.md', '.codex/AGENTS.md', '.pi/agent/AGENTS.md']}
                        and remote['hooks'] == {'claude': True, 'codex': True}
                        and remote['agents'] == {'claude': True, 'codex': True, 'pi': True})
                if not good:
                    issues.append('VM adapters, instructions or contract differ')
                    actions.append('sync')
            except (ValueError, KeyError, TypeError, OSError, subprocess.TimeoutExpired):
                issues.append('VM unavailable or probe invalid')
        result['ready'] = not issues
        results[name] = result
    return results


def protection_matches(p):
    if not p:
        return False
    checks = p.get('required_status_checks') or {}
    reviews = p.get('required_pull_request_reviews') or {}
    return (set(checks.get('contexts', [])) == set(CHECKS)
            and {(c.get('context'), c.get('app_id')) for c in checks.get('checks', [])}
                == {(name, 15368) for name in CHECKS}
            and p.get('enforce_admins', {}).get('enabled') is True
            and reviews.get('required_approving_review_count') == 0
            and not reviews.get('require_code_owner_reviews') and not reviews.get('require_last_push_approval')
            and not p.get('allow_force_pushes', {}).get('enabled') and not p.get('allow_deletions', {}).get('enabled'))


def protection_body(old):
    # Preserve unrelated boolean protections. Never remove access restrictions.
    body = {'required_status_checks': {'strict': False,
                'checks': [{'context': name, 'app_id': 15368} for name in CHECKS]}, 'enforce_admins': True,
            'required_pull_request_reviews': {'dismiss_stale_reviews': True, 'require_code_owner_reviews': False,
                                             'required_approving_review_count': 0, 'require_last_push_approval': False},
            'restrictions': None, 'allow_force_pushes': False, 'allow_deletions': False}
    for key in ('required_linear_history', 'required_conversation_resolution', 'block_creations', 'allow_fork_syncing'):
        if key in (old or {}):
            body[key] = old[key]['enabled']
    return body


def plan(policy, config, services):
    observation = observe(policy, config, services)
    result = {'version': 1, 'operation': 'reconcile', 'policy': policy,
              'runtime_hashes': expected_hashes(), 'projects': observation,
              'instructions': {p['name']: instructions(policy, p) for p in policy['projects']},
              'workflows': {p['name']: workflows(p) for p in policy['projects']}}
    result['approval'] = digest(result)
    return result


def workflows(project):
    return {WORKFLOW: workflow(project), REVIEW_WORKFLOW: review_workflow(project)}


def workflow_pr(services, p, expected_files, expected_head, expected_shas):
    repo = p['repo']
    base = services.api(f"repos/{repo}/git/ref/heads/{p['branch']}")['object']['sha']
    if base != expected_head:
        raise Failure('default branch changed during apply; inspect a new plan')
    branch = 'kuggis-cell-setup-' + digest({'base': base, 'workflows': expected_files})[:12]
    ref = services.api(f'repos/{repo}/git/ref/heads/{branch}', missing=True)
    if ref is None:
        services.api(f'repos/{repo}/git/refs', 'POST', {'ref': 'refs/heads/' + branch, 'sha': base})
    # An existing setup branch is never overwritten. A changed proposal needs review.
    for path, expected_content in expected_files.items():
        current = services.api(f'repos/{repo}/contents/{path}?ref={branch}', missing=True)
        if current and base64.b64decode(current['content']).decode() != expected_content and current['sha'] != expected_shas[path]:
            raise Failure(f'{repo}: existing setup branch has unreviewed changes; resolve it before retrying')
        if not current or base64.b64decode(current['content']).decode() != expected_content:
            update = {'message': 'Configure development-cell test and review workflows',
                      'content': base64.b64encode(expected_content.encode()).decode(), 'branch': branch}
            if current:
                update['sha'] = current['sha']
            services.api(f'repos/{repo}/contents/{path}', 'PUT', update)
    prs = services.api(f"repos/{repo}/pulls?head={repo.split('/')[0]}:{branch}&state=open")
    if not prs:
        return services.open_setup_pr(p, {'title': 'Add cell development test and review workflows',
            'head': branch, 'base': p['branch'],
            'body': 'Run explicit offline tests on PRs and the default branch. Publish exact-head reviewer verdicts from the scoped VM bot as statuses, using trusted workflow code without checking out the PR. These are prerequisites for agent self-merging; merge permissions change only after both workflows are merged and default-branch tests are green.'})['html_url']
    return prs[0]['html_url']


def save_report(policy, config, observation):
    report = {'version': 1, 'policy_hash': digest(policy), 'checked_at': int(time.time()),
              'runtime_hashes': expected_hashes(), 'projects': observation}
    atomic_json(config / 'readiness' / (policy['cell'] + '.json'), report)
    return report


def audit(policy, config, services):
    try:
        observed = observe(policy, config, services)
    except (ValueError, OSError, KeyError, TypeError, subprocess.TimeoutExpired):
        # A failed fresh observation must not leave a previous green cache.
        failed = {p['name']: {'ready': False, 'issues': ['verification unavailable'], 'actions': []}
                  for p in policy['projects']}
        save_report(policy, config, failed)
        raise
    save_report(policy, config, observed)
    return observed


def apply(policy, config, services, approved):
    with lock(config):
        current = plan(policy, config, services)
        if current['approval'] != approved:
            raise Failure('plan changed; inspect a fresh plan before applying it')
        # Persist desired state before side effects. A partial failure remains incomplete.
        atomic_json(config / 'cells' / (policy['cell'] + '.json'), policy)
        save_report(policy, config, current['projects'])
        urls = []
        for p in policy['projects']:
            row = current['projects'][p['name']]
            actions = row['actions']
            if 'fork' in actions:
                services.api('repos/' + p['upstream'] + '/forks', 'POST', {})
                # Fork creation is asynchronous. Retrying reconcile resumes safely.
                raise Failure('fork requested; rerun reconcile after GitHub finishes creating it')
            if 'new' in actions:
                services.local('new', p['name'], p['repo'], '--vm', p['vm'], '--profile', p['profile'])
            elif 'sync' in actions:
                services.local('sync', p['name'])
            if 'workflow-pr' in actions:
                urls.append(workflow_pr(services, p, workflows(p), row['facts']['head'], row['facts']['workflow_shas']))
            if 'enable-squash' in actions:
                services.api('repos/' + p['repo'], 'PATCH', {'allow_squash_merge': True})
            if 'protection' in actions:
                services.api(f"repos/{p['repo']}/branches/{p['branch']}/protection", 'PUT',
                             protection_body(row['facts']['protection']))
        observed = audit(policy, config, services)
        return {'projects': observed, 'pull_requests': urls}
