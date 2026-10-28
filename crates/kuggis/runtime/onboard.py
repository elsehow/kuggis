"""Local exe setup effects. Input is compiled and validated by Rust.

No daemon or guest management credentials. All guest programs run through the
existing host helper, after namespace entry and the drop to the agent user.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess as sp
import sys
import tempfile
import uuid


class Failure(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


def atomic(path, data):
    path = Path(path)
    fd, tmp = tempfile.mkstemp(prefix='.pending-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    finally:
        Path(tmp).unlink(missing_ok=True)


def save(path, value):
    atomic(path, (json.dumps(value, indent=2) + '\n').encode())


def digest(path):
    with Path(path).open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()


def owned_vm(vms, name, receipt):
    matches = [v for v in vms if v.get('vm_name') == name]
    if not matches:
        if receipt.get('created_at'):
            raise Failure('host_missing', 'The recorded host is gone; refusing to create a replacement implicitly.')
        return None
    if len(matches) != 1 or matches[0].get('comment') != receipt['marker']:
        raise Failure('name_conflict', f'{name} already exists and is not owned by this setup.')
    vm = matches[0]
    if receipt.get('created_at') and receipt['created_at'] != vm.get('created_at'):
        raise Failure('host_replaced', 'The provider host identity changed; refusing to adopt it.')
    if not vm.get('created_at'):
        raise Failure('provider_response', 'Provider did not return a host creation identity.')
    return vm


def effective_grants(integrations, vm):
    specs = {'auto:all', 'vm:' + vm['vm_name'], *('tag:' + t for t in vm.get('tags', []))}
    return [i for i in integrations if specs.intersection(i.get('attachments') or [])]


def check_grants(integrations, vm):
    grants = effective_grants(integrations, vm)
    for grant in grants:
        if grant["name"] != "llm" or grant.get("type") != "llm":
            raise Failure("broader_gateway_grant", "Additional integration reaches this host: " + grant["name"] + ". Remove its attachment before creating this workspace.")
    if not any(g["name"] == "llm" for g in grants):
        raise Failure("missing_attachment", "Model attachment was not observed on the new VM.")
    return sorted(g["name"] for g in grants)


STAGE = '/home/agent/.local/share/kuggis/setup/agents'
PREPARE = '''from pathlib import Path
import sys
p=Path('/home/agent/.local/share/kuggis/setup/agents');p.mkdir(parents=True,exist_ok=True)
(p/sys.argv[1]).mkdir(exist_ok=True)
link=Path('/home/agent/workspace/.kuggis-setup')
if link.is_symlink() and str(link.readlink())=='/home/agent/.local/share/kuggis/setup/pi':link.unlink()
if link.is_symlink():
    assert link.readlink()==p, 'setup staging link belongs to another target'
elif link.exists():
    raise RuntimeError('project uses reserved setup staging path .kuggis-setup')
else:link.symlink_to(p,target_is_directory=True)
'''
CLONE = '''import os,subprocess as sp,tempfile,sys,json
from pathlib import Path
root=Path('/home/agent/workspace'); url=sys.argv[1]
link=root/'.kuggis-setup'
if link.is_symlink() and str(link.readlink()) in ('/home/agent/.local/share/kuggis/setup/pi','/home/agent/.local/share/kuggis/setup/agents'):link.unlink()
env=dict(os.environ,GIT_TERMINAL_PROMPT='0')
if (root/'.git').exists():
    actual=sp.check_output(['git','-C',str(root),'remote','get-url','origin'],env=env,text=True).strip()
    assert actual==url, 'existing checkout has a different origin'
    sp.run(['git','-C',str(root),'rev-parse','--verify','HEAD'],check=True,env=env,stdout=sp.DEVNULL)
else:
    assert not any(root.iterdir()), 'workspace is not empty; refusing to overwrite it'
    with tempfile.TemporaryDirectory(dir='/home/agent/.local/share/kuggis/setup') as temp:
        clone=Path(temp)/'clone'
        sp.run(['git','clone','--',url,str(clone)],check=True,env=env)
        os.rename(clone,root)
sp.run(['git','-C',str(root),'ls-remote','origin','HEAD'],check=True,env=env,stdout=sp.DEVNULL)
print(json.dumps({'repository_read':'verified'}))
'''
DEFAULT_MODEL = '''import json,os,tempfile
from pathlib import Path
p=Path('/home/agent/.pi/agent/settings.json')
settings=json.loads(p.read_text())
settings.setdefault('defaultProvider','exe')
settings.setdefault('defaultModel','gpt-6.1-sol')
with tempfile.NamedTemporaryFile(mode='w',dir=p.parent,delete=False) as f:
    json.dump(settings,f); temporary=f.name
os.replace(temporary,p)
'''


class Setup:
    def __init__(self, payload):
        self.p = payload
        self.request = payload['request']
        self.directory = Path(payload['directory'])
        self.vm = 'kuggis-' + self.request['name']
        self.config = self.directory/'workspace.json'
        self.receipt = {}
        self.step = 'preflight'

    def progress(self, step, message):
        self.step = step
        print(message, file=sys.stderr, flush=True)

    def run(self, argv, data=None, timeout=120, check=True):
        try:
            result = sp.run([str(x) for x in argv], input=data,
                            stdin=sp.DEVNULL if data is None else None,
                            capture_output=True, timeout=timeout)
        except sp.TimeoutExpired as error:
            raise Failure('timeout', f'{self.step} timed out; rerun create to reconcile and resume.') from error
        if check and result.returncode:
            detail = (result.stderr or result.stdout).decode(errors='replace')[-2000:].strip()
            raise Failure('command_failed', f'{self.step} failed: {detail or "exit " + str(result.returncode)}')
        return result

    def ssh(self, target, words, data=None, generated=False, timeout=120):
        argv = ['ssh','-T','-o','BatchMode=yes','-o','StrictHostKeyChecking=yes',
                '-o','ForwardAgent=no','-o','ForwardX11=no','-o','ClearAllForwardings=yes',
                '-o','ControlMaster=no','-o','ControlPath=none','-o','PermitLocalCommand=no',
                '-o','ConnectTimeout=15','-o','ServerAliveInterval=15','-o','ServerAliveCountMax=3']
        config = self.directory/'ssh-config' if generated else self.p.get('ssh_config')
        if config:
            argv += ['-F',str(config)]
        return self.run([*argv,target,shlex.join(words)], data=data, timeout=timeout)

    def control(self, *words, as_json=False):
        output = self.ssh('exe.dev', list(words) + (['--json'] if as_json else [])).stdout
        try:
            result = json.loads(output)
        except ValueError:
            if as_json:
                raise Failure('provider_response', 'Expected a structured response from exe.dev.')
            if output.lstrip().lower().startswith(b'error'):
                raise Failure('provider_error', output.decode(errors='replace')[:1000])
            return None
        if isinstance(result, dict) and result.get('error'):
            raise Failure('provider_error', str(result['error'])[:1000])
        return result

    def integrations(self):
        value = self.control('integrations','list',as_json=True)
        if not isinstance(value,list) or any(not isinstance(i,dict) or not i.get('name') for i in value):
            raise Failure('provider_response', 'Invalid integration inventory.')
        return value

    def vms(self):
        value = self.control('ls',as_json=True)
        if not isinstance(value,dict) or not isinstance(value.get('vms'),list):
            raise Failure('provider_response','Invalid VM inventory.')
        return value['vms']

    def checkpoint(self, **fields):
        self.receipt.update(fields)
        save(self.directory/'receipt.json',self.receipt)

    def cli(self, verb, *args, timeout=120):
        return self.run([self.p['cli'],verb,self.config,*args],timeout=timeout)

    def github(self, *args):
        return self.run([self.p["cli"], *args], timeout=180)

    def guest(self, *argv, timeout=120):
        return self.cli('exec','--',*argv,timeout=timeout)

    def download(self, agent):
        release = json.loads(self.p['recipes'][agent]['release.json'])
        spec = release['archives']['x86_64']
        archive = self.directory/(agent+'.tar.gz')
        if not archive.exists() or digest(archive) != spec['sha256']:
            partial = archive.with_suffix('.gz.partial')
            self.run(['curl','--fail','--location','--silent','--show-error','--retry','2',
                      '--connect-timeout','20','--max-time','300',spec['url'],'--output',partial],timeout=660)
            if digest(partial) != spec['sha256']:
                raise Failure('checksum_mismatch','Agent archive checksum does not match the pinned release.')
            os.replace(partial,archive)
        return archive

    def prepare_local(self):
        if sys.version_info < (3,11):
            raise Failure('python_version','Workspace creation requires local Python 3.11 or newer.')
        binary = Path(self.p['binary'])
        if not binary.is_file():
            raise Failure('missing_linux_helper', 'Matching Linux helper not found. In a development checkout, run scripts/build-linux.sh once; packaged installs place kuggis-linux-x86_64 beside kuggis.')
        with binary.open('rb') as f:
            header = f.read(20)
        if len(header)<20 or header[:6]!=b'\x7fELF\x02\x01' or int.from_bytes(header[18:20],'little')!=62:
            raise Failure('wrong_linux_helper','Expected the matching x86-64 Linux helper.')
        old = self.directory/'setup.json'
        if old.exists() and json.loads(old.read_text()) != self.request:
            raise Failure('request_changed','An existing setup has different inputs. create resumes the saved request; it does not overwrite its configuration.')
        save(old,self.request)
        receipt = self.directory/'receipt.json'
        self.receipt = json.loads(receipt.read_text()) if receipt.exists() else {'version':1,'marker':'kuggis-create:'+str(uuid.uuid4())}
        self.p['ssh_config'] = self.p.get('ssh_config') or self.receipt.get('ssh_config')
        self.receipt['ssh_config'] = self.p['ssh_config']
        self.checkpoint(status='in_progress')
        original = self.p.get('ssh_config') or '~/.ssh/config'
        if any(c in str(original) for c in '\r\n\0'):
            raise Failure('invalid_ssh_config','SSH configuration path contains control characters.')
        # exe documents a shared SSH host identity. Verify new hostnames using
        # the already trusted exe.dev key, never an unauthenticated key scan.
        quoted = str(original).replace('\\','\\\\').replace('"','\\"')
        atomic(self.directory/'ssh-config',
               (f'Host {self.vm}.exe.xyz\n    HostKeyAlias exe.dev\nHost *\n    Include "{quoted}"\n').encode())
        save(self.config,self.p['workspace'])
        return binary

    def provision(self):
        binary = self.prepare_local()
        self.progress('preflight','Checking provider access, repository scope and installation artifacts…')
        integrations = self.integrations()
        llm = next((i for i in integrations if i['name']=='llm' and i.get('type')=='llm' and not i.get('isTeam')),None)
        if not llm:
            raise Failure('missing_model_integration','Configure the managed llm integration in exe.dev before creating a workspace.')
        model_provider = llm.get('config',{}).get('providers',{}).get('openai',{})
        if not model_provider.get('enabled') or model_provider.get('source')!='managed':
            raise Failure('missing_model_integration','Enable the managed OpenAI provider in the exe llm integration before creating this recipe.')
        vm = owned_vm(self.vms(),self.vm,self.receipt)
        # Auto-attached broad grants would become reachable at the same gateway.
        for grant in integrations:
            if 'auto:all' in (grant.get('attachments') or []) and grant['name']!='llm':
                raise Failure('broader_gateway_grant','Account-wide integration '+grant['name']+' would broaden this workspace. Adjust its attachment first.')
        archives = {agent:self.download(agent) for agent in self.p['recipes']}
        self.github("github", "check", "--repo", self.request["repo"], "--access", self.request["github_access"])
        if vm is None:
            if self.receipt.get('creation_started'):
                raise Failure('creation_unknown','A previous create had an uncertain outcome and no matching VM is visible. No second VM was allocated; inspect the provider operation before retrying.')
            self.progress('create_host','Creating dedicated exe host…')
            self.checkpoint(creation_started=True)
            self.control('new','--name',self.vm,'--cpu',str(self.request['cpus']),
                         '--memory',str(self.request['memory_gib'])+'GB','--comment',self.receipt['marker'])
            vm = owned_vm(self.vms(),self.vm,self.receipt)
            if not vm:
                raise Failure('creation_unknown','Created host is not yet visible; rerun create to reconcile it.')
        self.checkpoint(created_at=vm['created_at'],vm=self.vm)
        if (vm.get('allocated_cpus',0)<self.request['cpus'] or
                vm.get('memory_capacity_bytes',0)<self.request['memory_gib']*1024**3):
            raise Failure('insufficient_capacity','The allocated host has less CPU or memory than the requested profile.')
        self.progress('resources','Configuring private host and repository/model access…')
        self.control('share','set-private',self.vm)
        for integration in ("llm",):
            current = next(i for i in self.integrations() if i['name']==integration)
            specs = set(current.get('attachments') or [])
            if 'auto:all' not in specs and 'vm:'+self.vm not in specs:
                self.control('integrations','attach',integration,'vm:'+self.vm)
        vm = owned_vm(self.vms(),self.vm,self.receipt)
        self.checkpoint(effective_integrations=check_grants(self.integrations(),vm))
        probe = "import json,platform;from pathlib import Path;print(json.dumps({'arch':platform.machine(),'os':Path('/etc/os-release').read_text()}))"
        observed = json.loads(self.ssh(self.vm+'.exe.xyz',['sudo','-n','python3','-c',probe],generated=True).stdout)
        if observed['arch']!='x86_64' or 'ID=ubuntu\n' not in observed['os'] or 'VERSION_ID="24.04"' not in observed['os']:
            raise Failure('unsupported_host','This recipe requires an x86-64 Ubuntu 24.04 host.')
        self.progress('install_workspace','Installing workspace boundary…')
        host_script = ('import sys\nnamespace={"__name__":"kuggis_bootstrap"}\nexec('+repr(self.p['host_executor'])+',namespace)\n'
                       'namespace["safe_directory"]("/etc/kuggis/recipes",0o700)\n'
                       'namespace["atomic_write"]("/etc/kuggis/recipes/pi-models.json",sys.stdin.buffer.read(),0o600)\n')
        self.ssh(self.vm+'.exe.xyz',['sudo','-n','python3','-c',host_script],
                 data=self.p['recipes']['pi']['models.json'].encode(),generated=True)
        setup_digest = hashlib.sha256((digest(binary)+json.dumps(self.p['workspace'],sort_keys=True)).encode()).hexdigest()
        install = self.receipt.get('setup_digest') != setup_digest
        if not install:
            try:
                current = self.ssh(self.vm+'.exe.xyz',['sudo','-n','cat','/etc/kuggis/workspace.json'],generated=True)
                install = json.loads(current.stdout) != self.p['workspace']
                if not install: self.cli('check')
            except (Failure,ValueError): install=True
        if install:
            self.cli('setup','--bin',binary,timeout=1800)
            self.checkpoint(setup_digest=setup_digest)
        self.github("github", "attach", self.config)
        if not self.receipt.get("github_initialized"):
            self.github("repo", "add", self.config, self.request["repo"], "--access", self.request["github_access"])
            self.checkpoint(github_initialized=True)
        for agent, recipes in self.p['recipes'].items():
            self.progress('install_agent','Installing pinned '+agent+' recipe…')
            self.guest('/usr/bin/python3','-c',PREPARE,agent)
            local = self.directory/'recipes'/agent
            local.mkdir(parents=True,exist_ok=True)
            for name,content in recipes.items():
                path = local/name
                atomic(path,content.encode())
                if name!='models.json' or agent=='codex':
                    self.cli('put',path,'.kuggis-setup/'+agent+'/'+name,'--replace')
            self.cli('put',archives[agent],'.kuggis-setup/'+agent+'/agent.tar.gz','--replace',timeout=300)
            self.guest('/usr/bin/python3',STAGE+'/'+agent+'/install.py',STAGE+'/'+agent+'/agent.tar.gz')
        self.guest('/usr/bin/python3','-c',DEFAULT_MODEL)
        self.progress('repository','Preparing repository and checking Git access…')
        self.guest('/usr/bin/python3','-c',CLONE,'https://github.com/'+self.request['repo']+'.git',timeout=300)
        self.progress('verify','Checking isolation and a real model/tool round trip…')
        boundary = json.loads(self.cli('check').stdout)
        if not boundary.get('ok'): raise Failure('boundary_failed','Boundary check did not pass.')
        smoke = {}
        for agent in self.p['recipes']:
            smoke[agent] = json.loads(self.guest('/usr/bin/python3',STAGE+'/'+agent+'/smoke.py','exe/gpt-6.1-sol',timeout=240).stdout)
            if not smoke[agent].get('ok'): raise Failure('model_failed',agent+' model/tool check did not pass.')
        self.checkpoint(status='ready',checks={'boundary':boundary,'model':smoke,'git_read':'verified',
                                              'github_access':self.request['github_access'],'git_push':'not_tested'})
        return {'ok':True,'name':self.request['name'],'status':'ready','vm':self.vm,
                'configuration':str(self.config),'connect':'kuggis connect '+self.request['name'],
                'model':'exe/gpt-6.1-sol','github_access':self.request['github_access'],
                'effective_integrations':self.receipt['effective_integrations']}


def main(payload):
    os.umask(0o077)
    setup = Setup(payload)
    try:
        setup.directory.mkdir(parents=True,exist_ok=True,mode=0o700)
        with (setup.directory/'create.lock').open('a') as lock:
            try: fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
            except BlockingIOError: raise Failure('setup_busy','Another create is already running for this workspace.')
            report = setup.provision()
        if payload['json_output']: print(json.dumps(report))
        else: print('\nReady. Run: '+report['connect'])
        return 0
    except (Exception,KeyboardInterrupt) as error:
        code = error.code if isinstance(error,Failure) else 'interrupted' if isinstance(error,KeyboardInterrupt) else 'setup_failed'
        message = str(error) or 'Setup interrupted; rerun the same create command to resume.'
        report = {'ok':False,'name':setup.request['name'],'error':{'code':code,'step':setup.step,'message':message},
                  'receipt':str(setup.directory/'receipt.json')}
        if payload['json_output']: print(json.dumps(report))
        else: print(f'kuggis: {message}\nSetup record: {report["receipt"]}',file=sys.stderr)
        return 2


if __name__=='__main__':
    sys.exit(main(json.load(sys.stdin)))
