"""Policy/readiness/approval tests. Service fakes never make external changes."""
import base64
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from cells import adoption, control, policy
from cells.display import project as dashboard

ROOT = Path(__file__).resolve().parent


def definition():
    p = json.loads((ROOT/'cells/forecasting.example.json').read_text())
    p['projects'] = [dict(p['projects'][0], name='demo', repo='example/demo', upstream='upstream/demo',
                          vm='demo-dev', profile='-', test='python3 -m unittest discover -s tests')]
    return p


class Services:
    def __init__(self, value):
        self.policy = value
        self.project = value['projects'][0]
        self.head = 'a' * 40
        self.protection = control.protection_body(None)
        # API response shape differs from PUT input for the boolean flags.
        self.protection = self.response(self.protection)
        self.workflow = policy.workflow(self.project)
        self.review_workflow = policy.review_workflow(self.project)
        self.status = 'success'
        self.rules = []
        self.mutations = []
        self.vm_bad = False
        self.file_calls = 0

    def response(self, data):
        return {k: {'enabled': v} if isinstance(v, bool) else v for k, v in data.items()}

    def machines(self):
        return 'demo-dev.exe.xyz'

    def api(self, path, method='GET', body=None, missing=False):
        if method != 'GET':
            self.mutations.append((path, method, body))
            if path.endswith('/protection'):
                self.protection = self.response(body)
            return {'html_url': 'https://github.com/example/demo/pull/1'}
        if path == 'repos/example/demo':
            return {'id': 1, 'visibility': 'public', 'archived': False, 'default_branch': 'main',
                    'fork': True, 'parent': {'full_name': 'upstream/demo'}, 'allow_squash_merge': True}
        if '/git/ref/' in path:
            return {'object': {'sha': self.head}}
        if path.endswith('/protection'):
            return copy.deepcopy(self.protection)
        if '/rules/branches/' in path:
            return self.rules
        if '/contents/' in path:
            self.file_calls += 1
            content = self.review_workflow if control.REVIEW_WORKFLOW in path else self.workflow
            return None if content is None else {'sha': 'b'*40, 'content': base64.b64encode(content.encode()).decode()}
        if '/actions/runs?' in path:
            return {'workflow_runs': [{'id': 5, 'path': control.WORKFLOW, 'event': 'push', 'status': 'completed', 'conclusion': self.status}]}
        raise AssertionError(path)

    def ssh(self, vm, program):
        return {'contract': policy.contract(self.policy, self.project),
                'hashes': {} if self.vm_bad else control.expected_hashes(),
                'origin': 'https://github.int.exe.xyz/example/demo.git',
                'instructions': {f: True for f in ['.claude/CLAUDE.md', '.codex/AGENTS.md', '.pi/agent/AGENTS.md']},
                'notes_hash': policy.contract(self.policy,self.project)['instructions_hash'],
                'hooks': {'claude': True, 'codex': True}, 'agents': {'claude': True, 'codex': True, 'pi': True}}

    def local(self, *args):
        self.mutations.append(args)
        self.vm_bad = False


class Contract(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.config = self.root/'config'; self.config.mkdir()
        (self.config/'vms').write_text('demo demo-dev example/demo -\n')
        self.policy = definition()
        self.services = Services(self.policy)

    def test_projects_cannot_override_shared_merge_or_spending_policy(self):
        for transform in [lambda p: p['merge'].update(human_approvals=1),
                          lambda p: p['projects'][0].update(merge='human'),
                          lambda p: p.update(spending='unlimited'),
                          lambda p: p['doors'].update(publish='open')]:
            value = copy.deepcopy(self.policy); transform(value)
            with self.assertRaises(ValueError): policy.validate(value)

    def test_plan_is_read_only_and_ready_requires_every_check(self):
        planned = control.plan(self.policy, self.config, self.services)
        self.assertTrue(planned['projects']['demo']['ready'])
        self.assertEqual(self.services.mutations, [])
        self.assertFalse((self.config/'cells').exists())
        self.services.protection['required_pull_request_reviews']['required_approving_review_count'] = 1
        row = control.plan(self.policy, self.config, self.services)['projects']['demo']
        self.assertFalse(row['ready'])
        self.assertEqual(row['actions'], ['protection'])

    def test_no_approval_removal_until_exact_managed_ci_is_merged_and_green(self):
        self.services.protection['required_pull_request_reviews']['required_approving_review_count'] = 1
        for workflow, conclusion in [(None, 'success'), ('different workflow', 'success'),
                                     (policy.workflow(self.services.project), 'skipped'),
                                     (policy.workflow(self.services.project), 'failure')]:
            self.services.workflow, self.services.status = workflow, conclusion
            row = control.plan(self.policy, self.config, self.services)['projects']['demo']
            self.assertNotIn('protection', row['actions'])
            self.assertFalse(row['ready'])

    def test_stale_plan_and_unapproved_apply_never_mutate(self):
        old = control.plan(self.policy, self.config, self.services)
        self.services.head = 'c'*40
        with self.assertRaisesRegex(ValueError, 'plan changed'):
            control.apply(self.policy, self.config, self.services, old['approval'])
        self.assertEqual(self.services.mutations, [])
        self.assertFalse((self.config/'cells').exists())

    def test_apply_reconciles_then_reobserves_and_second_apply_is_noop(self):
        self.services.vm_bad = True
        self.services.protection['required_pull_request_reviews']['required_approving_review_count'] = 1
        plan = control.plan(self.policy, self.config, self.services)
        result = control.apply(self.policy, self.config, self.services, plan['approval'])
        self.assertTrue(result['projects']['demo']['ready'])
        self.assertIn(('sync', 'demo'), self.services.mutations)
        self.assertTrue((self.config/'cells/forecasting.json').exists())
        count = len(self.services.mutations)
        plan = control.plan(self.policy, self.config, self.services)
        control.apply(self.policy, self.config, self.services, plan['approval'])
        self.assertEqual(len(self.services.mutations), count)

    def test_unmanaged_vm_and_cross_cell_ownership_are_not_overwritten(self):
        (self.config/'vms').write_text('')
        row = control.plan(self.policy, self.config, self.services)['projects']['demo']
        self.assertFalse(row['ready']); self.assertNotIn('new', row['actions'])
        (self.config/'vms').write_text('other demo-dev example/other -\n')
        with self.assertRaisesRegex(ValueError, 'already belongs'):
            control.plan(self.policy, self.config, self.services)
        (self.config/'vms').write_text('demo demo-dev example/demo -\n')
        (self.config/'cells').mkdir()
        (self.config/'cells/other.json').write_text(json.dumps({'version': 1, 'cell': 'other', 'projects': ['demo']}))
        with self.assertRaisesRegex(ValueError, 'two cells'):
            control.plan(self.policy, self.config, self.services)

    def test_extra_rulesets_never_silently_weakened(self):
        self.services.rules = [{'type': 'pull_request'}]
        self.services.protection['required_pull_request_reviews']['required_approving_review_count'] = 1
        row = control.plan(self.policy, self.config, self.services)['projects']['demo']
        self.assertNotIn('protection', row['actions'])
        self.assertFalse(row['ready'])

    def test_failed_audit_invalidates_previously_green_cache(self):
        control.audit(self.policy,self.config,self.services)
        self.assertTrue(policy.cached(self.config,self.policy,'demo').startswith('development verified'))
        with patch.object(self.services,'api',side_effect=ValueError('unavailable')):
            with self.assertRaises(ValueError): control.audit(self.policy,self.config,self.services)
        self.assertIn('verification unavailable',policy.cached(self.config,self.policy,'demo'))

    def test_missing_review_workflow_and_wrong_check_source_block_readiness(self):
        self.services.review_workflow=None
        self.services.protection['required_pull_request_reviews']['required_approving_review_count']=1
        row=control.observe(self.policy,self.config,self.services)['demo']
        self.assertIn('workflow-pr',row['actions']); self.assertNotIn('protection',row['actions'])
        self.services.review_workflow=policy.review_workflow(self.services.project)
        self.services.protection['required_status_checks']['checks'][0]['app_id']=-1
        self.assertFalse(control.observe(self.policy,self.config,self.services)['demo']['ready'])

    def test_unreachable_vm_remains_incomplete_without_proposing_overwrite(self):
        with patch.object(self.services,'ssh',side_effect=subprocess.TimeoutExpired('ssh',30)):
            row=control.audit(self.policy,self.config,self.services)['demo']
        self.assertFalse(row['ready'])
        self.assertIn('VM unavailable or probe invalid',row['issues'])
        self.assertNotIn('sync',row['actions'])
        self.assertIn('VM unavailable',policy.cached(self.config,self.policy,'demo'))

    def test_guard_refuses_incomplete_new_job_but_keeps_live_job_accessible(self):
        import cellctl
        control.atomic_json(self.config/'cells/forecasting.json',self.policy)
        self.services.vm_bad=True
        original=self.services.ssh
        live=False
        def ssh(vm,program):
            return live if 'has-session' in program else original(vm,program)
        with patch.dict(os.environ,KUGGIS_CONFIG=str(self.config)), \
             patch.object(sys,'argv',['cellctl','guard','demo','job']), \
             patch.object(control,'Services',return_value=self.services), \
             patch.object(self.services,'ssh',side_effect=ssh):
            with self.assertRaisesRegex(ValueError,'setup incomplete'): cellctl.main()
            live=True
            self.assertEqual(cellctl.main(),0)

    def test_setup_pr_updates_approved_workflows_and_retries_idempotently(self):
        p=self.services.project
        wanted=control.workflows(p)
        existing={path:{'sha':'b'*40,'content':base64.b64encode(b'old').decode()} for path in wanted}
        expected={path:'b'*40 for path in wanted}
        created=False; prs=[]; writes=[]
        def api(path,method='GET',body=None,missing=False):
            nonlocal created
            if method!='GET':
                writes.append((path,method))
                if path.endswith('/git/refs'): created=True; return {}
                if '/contents/' in path:
                    name=path.split('/contents/')[1]
                    self.assertEqual(body['sha'],existing[name]['sha'])
                    existing[name]={'sha':'c'*40,'content':body['content']}; return {}
                if path.endswith('/pulls'): prs.append({'html_url':'https://github.com/example/demo/pull/1'}); return prs[0]
            if '/git/ref/' in path:
                return {'object':{'sha':self.services.head}} if path.endswith('/main') or created else None
            if '/contents/' in path: return existing[path.split('/contents/')[1].split('?')[0]]
            if '/pulls?' in path: return prs
            raise AssertionError(path)
        with patch.object(self.services,'api',side_effect=api):
            first=control.workflow_pr(self.services,p,wanted,self.services.head,expected)
            count=len(writes)
            self.assertEqual(first,control.workflow_pr(self.services,p,wanted,self.services.head,expected))
            self.assertEqual(count,len(writes))
            existing[control.WORKFLOW]={'sha':'d'*40,'content':base64.b64encode(b'unreviewed').decode()}
            with self.assertRaisesRegex(ValueError,'unreviewed'):
                control.workflow_pr(self.services,p,wanted,self.services.head,expected)

    def test_dashboard_incomplete_expired_and_changed_policy(self):
        control.atomic_json(self.config/'cells/forecasting.json', self.policy)
        snap = {'projects': [{'name': 'demo', 'profile': '-'}], 'sessions': [], 'now': 1000}
        result = dashboard(snap, self.config)['cells'][0]
        self.assertEqual(result['needs_you'], 1)
        self.assertIn('setup incomplete', result['rows'][0]['state'])
        report = control.save_report(self.policy, self.config, control.observe(self.policy,self.config,self.services))
        snap['now'] = report['checked_at']
        self.assertEqual(dashboard(snap,self.config)['cells'][0]['needs_you'], 0)
        snap['now'] += policy.TTL + 1
        self.assertIn('expired', dashboard(snap,self.config)['cells'][0]['rows'][0]['state'])
        changed = copy.deepcopy(self.policy); changed['projects'][0]['test'] += ' -v'
        self.assertIn('policy changed', policy.cached(self.config, changed, 'demo', report['checked_at']))

    def test_onboard_command_and_legacy_guard_do_not_start_models(self):
        result = subprocess.run([str(ROOT/'kuggis'),'onboard','--help'], capture_output=True,text=True)
        self.assertEqual(result.returncode,0)
        self.assertIn('--apply', result.stdout)
        with patch.dict(os.environ, KUGGIS_CONFIG=str(self.config)):
            result = subprocess.run([sys.executable,'-I',str(ROOT/'cellctl.py'),'guard','legacy','task'],capture_output=True,text=True)
        self.assertEqual(result.returncode,0,result.stderr)


class Adoption(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.repo = self.root/'local'; self.repo.mkdir()
        self.policy = definition()
        self.git('init','-q','-b','main')
        self.git('config','user.name','Test'); self.git('config','user.email','test@example.invalid')
        self.git('config','core.hooksPath','/dev/null')
        (self.repo/'code.py').write_text('base\n'); self.git('add','.'); self.git('commit','-qm','base')
        self.base = self.git('rev-parse','HEAD')
        self.git('update-ref','refs/remotes/origin/main',self.base)
        self.git('remote','add','origin','https://github.com/upstream/demo.git')
        self.git('checkout','-qb','job')
        (self.repo/'code.py').write_text('finished code\n'); self.git('add','.'); self.git('commit','-qm','job work')
        (self.repo/'brief.md').write_text('Work finished; do not repeat experiments.')
        (self.repo/'secret.txt').write_text('private fixture, never selected')

    def git(self,*args):
        return subprocess.check_output(['git','-C',str(self.repo),*args],text=True,stderr=subprocess.PIPE).strip()

    def test_exact_selected_commits_and_files_only_no_source_mutation(self):
        before = self.git('status','--porcelain')
        manifest, blobs, _ = adoption.prepare(self.repo,self.policy,brief='brief.md')
        self.assertEqual(manifest['base'],self.base)
        self.assertEqual(len(manifest['commits']),1)
        self.assertEqual(set(blobs),{'job.bundle','artifacts/brief.md'})
        self.assertEqual(manifest['excluded_untracked_count'],1)
        self.assertNotIn('secret.txt',json.dumps(manifest))
        self.assertEqual(self.git('status','--porcelain'),before)
        self.assertEqual(manifest['approval'],adoption.prepare(self.repo,self.policy,brief='brief.md')[0]['approval'])
        (self.repo/'brief.md').write_text('Changed instructions')
        self.assertNotEqual(manifest['approval'],adoption.prepare(self.repo,self.policy,brief='brief.md')[0]['approval'])

    def test_reject_dirty_work_wrong_repo_and_symlinks(self):
        (self.repo/'link').symlink_to(self.repo/'secret.txt')
        with self.assertRaises(ValueError): adoption.prepare(self.repo,self.policy,includes=['link'])
        with self.assertRaises(ValueError): adoption.prepare(self.repo,self.policy,includes=['../outside'])
        self.git('remote','set-url','origin','git@github.com:other/private.git')
        with self.assertRaisesRegex(ValueError,'remote'): adoption.prepare(self.repo,self.policy)
        self.git('remote','set-url','origin','git@github.com:upstream/demo.git')
        (self.repo/'code.py').write_text('dirty')
        with self.assertRaisesRegex(ValueError,'tracked changes'): adoption.prepare(self.repo,self.policy)

    def module(self):
        sys.path.insert(0,str(ROOT/'vm'))
        self.addCleanup(lambda: sys.path.remove(str(ROOT/'vm')))
        spec=importlib.util.spec_from_file_location('adopt_job',ROOT/'vm/adopt-job.py')
        mod=importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)
        return mod

    def test_receiver_rejects_tampering_and_traversal(self):
        mod=self.module()
        manifest,blobs,_=adoption.prepare(self.repo,self.policy)
        parsed,payload=mod.unpack(adoption.archive(manifest,blobs))
        self.assertEqual(parsed,manifest)
        blobs['job.bundle'] += b'tampered'
        with self.assertRaisesRegex(ValueError,'artifact changed'): mod.unpack(adoption.archive(manifest,blobs))
        for name in ('../escape', 'receipt.json', 'pending.json', 'artifacts//file', 'artifacts/../receipt.json'):
            out=io.BytesIO()
            with tarfile.open(fileobj=out,mode='w') as tar:
                member=tarfile.TarInfo(name); member.size=1; tar.addfile(member,io.BytesIO(b'x'))
            with self.assertRaisesRegex(ValueError,'archive'): mod.unpack(out.getvalue())

    def test_receiver_import_is_exact_idempotent_and_does_not_start_agent(self):
        mod=self.module(); home=self.root/'remote'; home.mkdir(); box=home/'.box'; box.mkdir()
        repo=home/'demo'
        subprocess.run(['git','clone','-q',str(self.repo),str(repo)],check=True)
        subprocess.run(['git','-C',str(repo),'branch','-D','job'],capture_output=True)
        # Clone checked out job, so move to main and remove it now.
        subprocess.run(['git','-C',str(repo),'checkout','-q','main'],check=True)
        subprocess.run(['git','-C',str(repo),'branch','-D','job'],check=True,capture_output=True)
        (box/'cell-contract.json').write_text(json.dumps(policy.contract(self.policy,self.policy['projects'][0])))
        manifest,blobs,_=adoption.prepare(self.repo,self.policy,brief='brief.md')
        original_git=mod.git
        def fake_git(repo,*args):
            if args == ('remote','get-url','origin'): return 'https://github.int.exe.xyz/example/demo.git'
            return original_git(repo,*args)
        original_run=mod.subprocess.run
        def fake_run(args,**kwargs):
            if args[0]=='tmux': return subprocess.CompletedProcess(args,1)
            return original_run(args,**kwargs)
        with patch.object(mod,'git',side_effect=fake_git),patch.object(mod.subprocess,'run',side_effect=fake_run):
            emit=mod.events.emit
            def interrupted(*args,**kwargs):
                if args[3]=='stopped': raise OSError('simulated interruption')
                return emit(*args,**kwargs)
            with patch.object(mod.events,'emit',side_effect=interrupted):
                with self.assertRaisesRegex(OSError,'interruption'): mod.adopt(home,adoption.archive(manifest,blobs))
            original_id=json.loads((box/'ids/job').read_text())['session_id']
            first=mod.adopt(home,adoption.archive(manifest,blobs))
            second=mod.adopt(home,adoption.archive(manifest,blobs))
        self.assertEqual(first,second)
        self.assertEqual(first['session_id'],original_id)
        self.assertFalse(first['started'])
        self.assertEqual(original_git(repo,'rev-parse','refs/heads/job'),manifest['head'])
        self.assertFalse((repo/'.claude/worktrees/job').exists())
        self.assertTrue((box/'first/job').exists())
        self.assertNotIn('secret.txt', '\n'.join(str(p) for p in (box/'jobs').rglob('*')))


class MergeGate(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location('pr_gate', ROOT/'vm/pr-gate.py')
        self.gate = importlib.util.module_from_spec(spec); spec.loader.exec_module(self.gate)
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name); self.box = self.home/'.box'; (self.box/'ids').mkdir(parents=True)
        self.cell = definition(); self.c = policy.contract(self.cell,self.cell['projects'][0])
        (self.box/'cell-contract.json').write_text(json.dumps(self.c))
        self.sid, self.rid = '11111111-1111-4111-8111-111111111111', 'run-id'
        self.reviewer = '22222222-2222-4222-8222-222222222222'
        (self.box/'ids/author').write_text(json.dumps({'session_id':self.sid,'run_id':self.rid}))
        self.head = 'a'*40
        self.pr = {'state':'open','head':{'sha':self.head,'ref':'author','repo':{'full_name':'example/demo'}},
                   'base':{'ref':'main','repo':{'full_name':'example/demo'}},'merged':True,'html_url':'https://github.com/example/demo/pull/1'}
        self.checks=[{'name':name,'state':'SUCCESS'} for name in policy.CHECKS]
        self.calls=[]
        self.verdict={'id':7,'reviewer':self.reviewer,'verdict':'pass'}
        self.review={'context':'cell-review','state':'success','description':self.gate.review.description(self.verdict)}
        self.patchers=[patch.object(Path,'home',return_value=self.home),
                       patch.dict(os.environ,BOX_SESSION='author',BOX_SESSION_ID=self.sid,BOX_RUN_ID=self.rid),
                       patch.object(self.gate,'api',side_effect=self.api),patch.object(self.gate,'run',side_effect=self.fake_run)]
        for p in self.patchers: p.start(); self.addCleanup(p.stop)

    def api(self,repo,path,body=None):
        self.assertEqual(repo,'example/demo')
        if body is not None:
            self.calls.append((path,body)); return {}
        if path.startswith('pulls/'): return self.pr
        if path.startswith('commits/'): return [self.review]
        if path.startswith('issues/'):
            return [{'id':self.verdict['id'],'body':f"kuggis-cell-review/v1 {self.head} {self.verdict['reviewer']} {self.verdict['verdict']}",
                     'user':{'login':self.gate.review.BOT}}]
        raise AssertionError(path)

    def fake_run(self,args,body=None):
        if args[:3]==['gh','pr','checks']: return json.dumps(self.checks)
        self.calls.append(args); return ''

    def test_merge_pins_reviewed_head_and_never_uses_admin(self):
        self.gate.execute('merge',1,self.head)
        self.assertEqual(self.calls,[['gh','pr','merge','1','-R','example/demo','--squash','--match-head-commit',self.head]])

    def test_missing_skipped_failed_or_pending_check_blocks_merge(self):
        for state in ['SKIPPED','NEUTRAL','FAILURE','PENDING']:
            self.checks[0]['state']=state
            with self.assertRaises(ValueError): self.gate.execute('merge',1,self.head)
        self.checks=[]
        with self.assertRaises(ValueError): self.gate.execute('merge',1,self.head)
        self.assertEqual(self.calls,[])

    def test_changed_head_self_review_or_upstream_pr_is_refused(self):
        with self.assertRaises(ValueError): self.gate.execute('merge',1,'b'*40)
        with self.assertRaises(ValueError): self.gate.execute('review',1,self.head,'pass')
        self.pr['base']['repo']['full_name']='upstream/demo'
        with self.assertRaises(ValueError): self.gate.execute('merge',1,self.head)
        self.assertEqual(self.calls,[])

    def test_merge_refuses_self_certified_review_without_admin_api(self):
        self.verdict['reviewer']=self.sid
        self.review['description']=self.gate.review.description(self.verdict)
        with self.assertRaises(ValueError): self.gate.execute('merge',1,self.head)
        self.assertEqual(self.calls,[])

    def test_independent_session_can_record_fail_or_pass_on_exact_head(self):
        (self.box/'ids/reviewer').write_text(json.dumps({'session_id':self.reviewer,'run_id':'reviewer-run'}))
        with patch.dict(os.environ,BOX_SESSION='reviewer',BOX_SESSION_ID=self.reviewer,BOX_RUN_ID='reviewer-run'):
            self.gate.execute('review',1,self.head,'fail')
            self.gate.execute('review',1,self.head,'pass')
        self.assertEqual([c[1]['body'].split()[-1] for c in self.calls],['fail','pass'])
        self.assertTrue(all(c[0]=='issues/1/comments' for c in self.calls))

    def test_newer_failure_or_unpublished_verdict_blocks_old_passing_status(self):
        self.verdict['verdict']='fail'
        with self.assertRaises(ValueError): self.gate.execute('merge',1,self.head)
        self.verdict.update(verdict='pass',id=8)
        with self.assertRaises(ValueError): self.gate.execute('merge',1,self.head)
        self.assertEqual(self.calls,[])


class ReviewWorkflow(unittest.TestCase):
    def setUp(self):
        spec=importlib.util.spec_from_file_location('review_status',ROOT/'vm/review.py')
        self.mod=importlib.util.module_from_spec(spec); spec.loader.exec_module(self.mod)
        self.head='a'*40; self.sid='22222222-2222-4222-8222-222222222222'
        self.comments=[self.comment(1,'pass')]; self.writes=[]
        self.pr={'state':'open','head':{'sha':self.head,'repo':{'full_name':'example/demo'}},
                 'base':{'ref':'main','repo':{'full_name':'example/demo'}}}
        self.event={'issue':{'number':1,'pull_request':{}},'comment':{'user':{'login':self.mod.BOT}}}
        self.event['issue']['pull_request']={'url':'ignored'}

    def comment(self,number,verdict,head=None,user=None):
        return {'id':number,'body':f'kuggis-cell-review/v1 {head or self.head} {self.sid} {verdict}',
                'user':{'login':user or self.mod.BOT}}

    def api(self,path,body=None):
        if body is not None: self.writes.append((path,body)); return {}
        if path.startswith('pulls/'): return self.pr
        if path.startswith('issues/'): return self.comments
        raise AssertionError(path)

    def test_publishes_latest_exact_head_verdict_even_for_out_of_order_events(self):
        self.comments += [self.comment(2,'fail'),self.comment(3,'pass',head='b'*40),self.comment(4,'pass',user='outsider')]
        self.mod.publish(self.event,'example/demo','main',self.api)
        self.assertEqual(self.writes[0][0],'statuses/'+self.head)
        self.assertEqual(self.writes[0][1]['state'],'failure')
        self.assertIn('comment 2',self.writes[0][1]['description'])

    def test_refuses_external_pr_and_outside_commenter_and_invalidates_deleted_review(self):
        self.pr['head']['repo']['full_name']='outsider/demo'
        self.mod.publish(self.event,'example/demo','main',self.api); self.assertEqual(self.writes,[])
        self.pr['head']['repo']['full_name']='example/demo'
        self.event['comment']['user']['login']='outsider'
        self.mod.publish(self.event,'example/demo','main',self.api); self.assertEqual(self.writes,[])
        self.event['comment']['user']['login']=self.mod.BOT; self.comments=[]
        self.mod.publish(self.event,'example/demo','main',self.api)
        self.assertEqual(self.writes[0][1]['state'],'failure')

    def test_generated_workflow_executes_same_review_code_without_pr_checkout(self):
        value=policy.review_workflow(definition()['projects'][0])
        script=value.split("python3 - <<'PY'\n",1)[1].rsplit('          PY',1)[0]
        import textwrap
        self.assertEqual(textwrap.dedent(script),(ROOT/'vm/review.py').read_text())
        compile(textwrap.dedent(script),'generated-review-workflow','exec')
        self.assertNotIn('actions/checkout',value)
        self.assertNotIn('pull_request_target',value)

if __name__=='__main__': unittest.main()
