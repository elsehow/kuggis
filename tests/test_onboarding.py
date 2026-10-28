"""Operator-flow tests; fake effects do not claim a live Linux boundary passed."""
import contextlib
import copy
import fcntl
import io
import json
import os
from pathlib import Path
import pty
import subprocess as sp
import tempfile
import unittest
from unittest.mock import patch
from test_runtime import module, ROOT

o = module('onboard')
h = module('host')


def github(name='repo', repos=None, readonly=True, attachments=None):
    return {'name':name,'type':'github','repositories':repos or ['owner/repo'],
            'readOnly':readonly,'attachments':attachments or []}


def payload(root):
    binary=root/'linux'
    binary.write_bytes(b'\x7fELF\x02\x01'+b'\0'*12+b'\x3e\0')
    return {'request':{'version':1,'name':'test','repo':'owner/repo','github_access':'read',
                       'host':'exe','agent':'pi','model':'exe-managed','cpus':2,'memory_gib':8},
            'directory':str(root/'workspace'),'binary':str(binary),'cli':'kuggis',
            'workspace':{'version':2,'name':'test'},'ssh_config':str(root/'operator-ssh'),
            'json_output':True,'host_executor':'unused fake',
            'recipes':{agent:{'release.json':'{}','install.py':'','models.json':'{}','smoke.py':''} for agent in ('pi','codex')}}


class Fake(o.Setup):
    def __init__(self, p, remote):
        super().__init__(p)
        self.remote=remote

    def progress(self, step, message): self.step=step

    def download(self, agent):
        p=self.directory/(agent+'.tar.gz');p.write_bytes(b'archive');return p

    def control(self, *words, as_json=False):
        self.remote['calls'].append(words)
        if words==('ls',): return {'vms':copy.deepcopy(self.remote['vms'])}
        if words==('integrations','list'): return copy.deepcopy(self.remote['integrations'])
        if words[0]=='new':
            vm={'vm_name':self.vm,'created_at':'2026-10-28T00:00:00Z','comment':self.receipt['marker'],
                'allocated_cpus':2,'memory_capacity_bytes':8*1024**3}
            self.remote['vms'].append(vm)
            if self.remote.pop('lose_create_response',False): raise o.Failure('timeout','lost create response')
        elif words[:2]==('integrations','attach'):
            next(i for i in self.remote['integrations'] if i['name']==words[2])['attachments'].append(words[3])
        elif words[:3]==('integrations','add','github'):
            name=words[words.index('--name')+1]
            self.remote['integrations'].append(github(name,readonly='--readonly' in words))
        return None

    def ssh(self, target, words, **kwargs):
        if self.remote.pop('fail_host_connection',False): raise o.Failure('command_failed','host key mismatch')
        if 'platform.machine()' in words[-1]:
            data={'arch':'x86_64','os':'ID=ubuntu\nVERSION_ID="24.04"\n'}
        elif words[-1]=='/etc/kuggis/workspace.json': data=self.p['workspace']
        else: data={}
        return sp.CompletedProcess(words,0,json.dumps(data).encode(),b'')

    def cli(self, verb, *args, timeout=120):
        self.remote['calls'].append(('cli',verb,*map(str,args)))
        if verb=='exec' and str(args[-2]).endswith('/smoke.py') and self.remote.pop('fail_model',False):
            raise o.Failure('command_failed','model unavailable')
        return sp.CompletedProcess([],0,b'{"ok":true}',b'')

    def github(self, *args):
        self.remote['calls'].append(('github-cli', *map(str, args)))
        if self.remote.pop('fail_github', False):
            raise o.Failure('command_failed', 'GitHub broker unavailable')
        return sp.CompletedProcess([], 0, b'{"ok":true}', b'')


class SetupEffects(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name)
        self.p=payload(self.root)
        self.remote={'vms':[],'calls':[],'integrations':[github(),
            {'name':'llm','type':'llm','attachments':['auto:all'],
             'config':{'providers':{'openai':{'enabled':True,'source':'managed'}}}}]}
        Path(self.p['directory']).mkdir()

    def tearDown(self): self.temp.cleanup()

    def run_setup(self): return Fake(copy.deepcopy(self.p),self.remote).provision()

    def test_both_agents_are_installed_regardless_of_default(self):
        self.p['request']['agent']='codex'
        result=self.run_setup()
        self.assertTrue(result['ok'])
        calls=self.remote['calls']
        for agent in ('pi','codex'):
            self.assertTrue(any(c[:2]==('cli','put') and '.kuggis-setup/'+agent+'/install.py' in c for c in calls))
        self.assertTrue(any(c[:2]==('cli','put') and '.kuggis-setup/codex/models.json' in c for c in calls))
        self.assertTrue(any(o.DEFAULT_MODEL in c for c in calls))

    def test_creation_timeout_reconciles_owned_host_instead_of_duplicate(self):
        self.remote['lose_create_response']=True
        with self.assertRaises(o.Failure): self.run_setup()
        self.assertEqual(self.run_setup()['status'],'ready')
        self.assertEqual(sum(c[0]=='new' for c in self.remote['calls']),1)
        self.assertEqual(len(self.remote['vms']),1)

    def test_uncertain_missing_host_never_issues_a_second_create(self):
        self.remote['lose_create_response']=True
        with self.assertRaises(o.Failure): self.run_setup()
        self.remote['vms']=[]
        with self.assertRaisesRegex(o.Failure,'No second VM'): self.run_setup()
        self.assertEqual(sum(c[0]=='new' for c in self.remote['calls']),1)

    def test_retry_after_model_failure_preserves_host_and_skips_reinstall(self):
        self.remote['fail_model']=True
        with self.assertRaisesRegex(o.Failure,'model unavailable'): self.run_setup()
        self.assertTrue(self.run_setup()['ok'])
        self.assertEqual(sum(c[:2]==('cli','setup') for c in self.remote['calls']),1)
        self.assertEqual(sum(c[0]=='new' for c in self.remote['calls']),1)

    def test_host_key_failure_is_not_a_fallback_or_new_vm(self):
        self.remote['fail_host_connection']=True
        with self.assertRaisesRegex(o.Failure,'host key mismatch'): self.run_setup()
        self.assertTrue(self.run_setup()['ok'])
        self.assertEqual(sum(c[0]=='new' for c in self.remote['calls']),1)

    def test_existing_name_and_changed_requests_cannot_overwrite(self):
        self.remote['vms']=[{'vm_name':'kuggis-test','comment':'someone else'}]
        with self.assertRaisesRegex(o.Failure,'not owned'): self.run_setup()
        self.assertFalse(any(c[0]=='new' for c in self.remote['calls']))
        self.p['request']['repo']='owner/other'
        before=len(self.remote['calls'])
        with self.assertRaisesRegex(o.Failure,'different inputs'): self.run_setup()
        self.assertEqual(len(self.remote['calls']),before)

    def test_missing_artifacts_fail_before_provider_and_shared_grants_are_not_modified(self):
        Path(self.p['binary']).unlink()
        with self.assertRaisesRegex(o.Failure,'Linux helper'): self.run_setup()
        self.assertEqual(self.remote['calls'],[])
        self.p=payload(self.root)
        self.remote['integrations'].append(github('broad',['owner/repo','owner/secret'],False,['auto:all']))
        with self.assertRaisesRegex(o.Failure,'broaden'): self.run_setup()
        self.assertFalse(any(c[0]=='new' or c[:2]==('integrations','attach') for c in self.remote['calls']))

    def test_exact_scope_selection_and_creating_new_scope_does_not_edit_shared_grant(self):
        self.remote['integrations'][0]=github('write',['owner/repo'],False,['vm:production'])
        self.assertTrue(self.run_setup()['ok'])
        self.assertEqual(self.remote['integrations'][0],github('write',['owner/repo'],False,['vm:production']))
        self.assertFalse(any(c[:3]==('integrations','add','github') or c[:2]==('integrations','edit') for c in self.remote['calls']))
        self.assertTrue(any(c[:3] == ('github-cli','repo','add') for c in self.remote['calls']))

    def test_broker_preflight_precedes_vm_creation_and_retry_does_not_restore_removed_grants(self):
        self.remote['fail_github'] = True
        with self.assertRaisesRegex(o.Failure, 'GitHub broker unavailable'):
            self.run_setup()
        self.assertFalse(any(c[0] == 'new' for c in self.remote['calls']))
        self.run_setup()
        self.run_setup()
        self.assertEqual(sum(c[:3] == ('github-cli', 'repo', 'add') for c in self.remote['calls']), 1)

    def test_locks_return_one_json_error(self):
        with (Path(self.p['directory'])/'create.lock').open('w') as lock:
            fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
            out=io.StringIO()
            with contextlib.redirect_stdout(out): self.assertEqual(o.main(self.p),2)
            self.assertEqual(json.loads(out.getvalue())['error']['code'],'setup_busy')

    def test_zero_exit_provider_error_is_still_failure(self):
        setup=o.Setup(self.p)
        with patch.object(setup,'ssh',return_value=sp.CompletedProcess([],0,b'{"error":"denied"}',b'')):
            with self.assertRaisesRegex(o.Failure,'denied'):setup.control('new')

    def test_guest_exec_enters_namespace_with_literal_arguments(self):
        argv=['/bin/echo',"';touch /HOST;$(whoami)"]
        with patch.object(h,'load',return_value={'name':'test'}),patch.object(h,'guard') as guard,patch.object(h.os,'execv') as execute, \
             patch.object(h.os,'geteuid',return_value=0),patch.object(h.sys,'platform','linux'):
            h.dispatch(['exec','test','--',*argv])
            guard.assert_called_once()
            executable,command=execute.call_args.args
            self.assertEqual(executable,'/usr/bin/systemd-run')
            self.assertIn('--machine=kuggis-v2',command)
            self.assertIn('--uid=agent',command)
            self.assertNotIn('--pty',command)
            self.assertEqual(command[-2:],argv)


class InputsAndRecovery(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name)
        self.binary=os.environ.get('KUGGIS_BINARY',str(ROOT/'target/debug/kuggis'))
        self.env={**os.environ,'KUGGIS_STATE_DIR':str(self.root/'state'),
                  'GIT_CONFIG_NOSYSTEM':'1','GIT_CONFIG_GLOBAL':'/dev/null','GIT_CONFIG_SYSTEM':'/dev/null'}

    def tearDown(self):self.temp.cleanup()

    def test_wizard_flags_and_file_compile_to_same_request(self):
        master,slave=pty.openpty()
        try:
            child=sp.Popen([self.binary,'create','demo','--plan'],stdin=slave,stdout=sp.PIPE,stderr=sp.PIPE,env=self.env)
            os.close(slave);slave=None
            os.write(master,b'owner/repo\nwrite\n')
            stdout,stderr=child.communicate(timeout=10)
            self.assertEqual(child.returncode,0,stderr.decode())
            wizard=json.loads(stdout)
        finally:
            os.close(master)
            if slave is not None:os.close(slave)
        flags=sp.run([self.binary,'create','demo','--repo','owner/repo','--github-access','write','--plan','--json'],capture_output=True,check=True,env=self.env)
        setup=self.root/'setup.json';setup.write_text(json.dumps(wizard['request']))
        file=sp.run([self.binary,'create','--file',str(setup),'--plan','--json'],capture_output=True,check=True,env=self.env)
        self.assertEqual(wizard,json.loads(flags.stdout))
        self.assertEqual(wizard,json.loads(file.stdout))
        self.assertFalse((self.root/'state').exists())

    def test_name_lookup_and_structured_missing_helper(self):
        directory=self.root/'state/demo';directory.mkdir(parents=True)
        config=json.loads((ROOT/'examples/workspace.json').read_text())
        (directory/'workspace.json').write_text(json.dumps(config))
        result=sp.run([self.binary,'validate','demo'],capture_output=True,env=self.env)
        self.assertEqual(result.returncode,0,result.stderr.decode())
        result=sp.run([self.binary,'create','fresh','--repo','owner/repo','--bin',str(self.root/'missing'),'--json'],capture_output=True,env=self.env)
        self.assertEqual(result.returncode,2)
        self.assertEqual(json.loads(result.stdout)['error']['code'],'missing_linux_helper')

    def test_clone_retry_preserves_edits_and_model_default_preserves_selection(self):
        env={**self.env,'PATH':'/usr/bin:/bin:/usr/sbin:/sbin'}
        source=self.root/'source';source.mkdir()
        def git(*args):return sp.run(['/usr/bin/git','-C',str(source),*args],env=env,check=True,capture_output=True)
        git('init');(source/'file').write_text('original');git('add','file')
        git('-c','user.name=Test','-c','user.email=test@example.invalid','commit','-m','fixture')
        home=self.root/'agent';workspace=home/'workspace';workspace.mkdir(parents=True)
        (home/'.local/share/kuggis/setup').mkdir(parents=True)
        clone=o.CLONE.replace('/home/agent',str(home))
        for attempt in range(2):
            staging=home/'.local/share/kuggis/setup/agents'
            staging.mkdir(exist_ok=True)
            (workspace/'.kuggis-setup').symlink_to(staging)
            sp.run([os.sys.executable,'-c',clone,source.as_uri()],env=env,check=True,capture_output=True)
            if not attempt:(workspace/'file').write_text('pending user work')
        self.assertEqual((workspace/'file').read_text(),'pending user work')
        settings=home/'.pi/agent/settings.json';settings.parent.mkdir(parents=True)
        settings.write_text('{"defaultProvider":"chosen","defaultModel":"chosen-model"}')
        sp.run([os.sys.executable,'-c',o.DEFAULT_MODEL.replace('/home/agent',str(home))],env=env,check=True)
        self.assertEqual(json.loads(settings.read_text())['defaultModel'],'chosen-model')


if __name__=='__main__':unittest.main()
