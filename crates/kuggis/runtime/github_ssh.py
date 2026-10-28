"""Git-only upstream using a protected developer SSH key on the broker host."""
import os
from pathlib import Path
import shlex
import stat
import subprocess
from github_common import Failure, private_read, repository

SERVICES = {'git-upload-pack': 'read', 'git-receive-pack': 'write'}


class SSHGit:
    transport = 'ssh-gateway'
    identity_fields = ('upstream_repo',)

    def __init__(self, key_file, known_hosts_file):
        self.key_file, self.known_hosts = str(key_file), str(known_hosts_file)
        private_read(self.key_file)
        fd = os.open(self.known_hosts, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        try:
            info = os.fstat(fd)
            if not stat.S_ISREG(info.st_mode) or info.st_uid not in (0, os.geteuid()) or info.st_mode & 0o022:
                raise Failure('SSH known_hosts must be a protected regular file')
        finally:
            os.close(fd)
        if not Path(self.key_file).is_absolute() or not Path(self.known_hosts).is_absolute():
            raise Failure('SSH credential paths must be absolute')

    @staticmethod
    def permissions(access, workflows=False):
        if access not in ('read', 'write') or workflows:
            raise Failure('SSH grants accept read or write; Git write includes workflow files, so omit --workflows')
        return {'git': access}

    @staticmethod
    def environment():
        return {'PATH': '/usr/bin:/bin', 'LANG': 'C', 'GIT_CONFIG_NOSYSTEM': '1',
                'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_CONFIG_SYSTEM': '/dev/null', 'GIT_TERMINAL_PROMPT': '0'}

    def ssh(self):
        return ['/usr/bin/ssh', '-F', '/dev/null', '-T', '-i', self.key_file,
                '-o', 'BatchMode=yes', '-o', 'IdentitiesOnly=yes', '-o', 'IdentityAgent=none',
                '-o', 'StrictHostKeyChecking=yes', '-o', 'UserKnownHostsFile=' + self.known_hosts,
                '-o', 'GlobalKnownHostsFile=/dev/null', '-o', 'ForwardAgent=no', '-o', 'ForwardX11=no',
                '-o', 'ClearAllForwardings=yes', '-o', 'ControlMaster=no', '-o', 'ControlPath=none',
                '-o', 'ProxyCommand=none', '-o', 'ProxyJump=none', '-o', 'PermitLocalCommand=no',
                '-o', 'PasswordAuthentication=no', '-o', 'KbdInteractiveAuthentication=no',
                '-o', 'ConnectTimeout=15', '-o', 'ServerAliveInterval=15', '-o', 'ServerAliveCountMax=2']

    def command(self, repo, service):
        repo = repository(repo)
        if service not in SERVICES:
            raise Failure('unsupported Git service')
        return [*self.ssh(), 'git@github.com', service + ' ' + shlex.quote(repo + '.git')]

    def resolve(self, repo, scope):
        repo = repository(repo)
        result = subprocess.run(['/usr/bin/git', '-c', 'core.sshCommand=' + shlex.join(self.ssh()),
                                 'ls-remote', '--', 'ssh://git@github.com/' + repo + '.git'],
                                env=self.environment(), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=45)
        if result.returncode:
            raise Failure('broker SSH identity cannot read this repository; check the key, host key and GitHub/SSO access', 403)
        # SSH identifies repositories by path. No GitHub numeric-ID lookup is available.
        # Read availability is tested; write authority is still checked by GitHub on push.
        return {'upstream_repo': repo}

    def open(self, repo, service):
        return subprocess.Popen(self.command(repo, service), env=self.environment(),
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                start_new_session=True)
