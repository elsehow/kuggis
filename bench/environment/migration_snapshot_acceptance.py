#!/usr/bin/env python3
"""Exercise the real snapshot namespace on a Linux canary; no live cell changes.

Usage: sudo python3 SCRIPT /trusted/path/migration_snapshot.py
Artifacts remain under a new private /var/lib/kuggis-migration/test-N directory.
"""
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess as sp
import sys
import tarfile
import uuid

spec = importlib.util.spec_from_file_location('snapshot', sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
assert os.geteuid() == 0
parent = Path('/var/lib/kuggis-migration')
parent.mkdir(mode=0o700, exist_ok=True)
assert parent.stat().st_mode & 0o077 == 0
work = parent / ('test-' + uuid.uuid4().hex)
work.mkdir(mode=0o700)
helper = m.helper(work)
source = work / 'source'
source.mkdir()
os.chown(source, m.BASE, m.BASE)
make = '''
import os,pathlib,socket
p=pathlib.Path('/payload')
assert pathlib.Path('/proc/self/uid_map').read_text().split()==['0','524288','65536']
assert not pathlib.Path('/etc/cell/secret').exists()
assert len(list(pathlib.Path('/sys/class/net').iterdir()))==1
(p/'a').mkdir()
(p/'a/data').write_bytes(b'private fixture\\x00data')
os.chown(p/'a/data',1000,1000)
os.chmod(p/'a/data',0o640)
os.setxattr(p/'a/data','user.acceptance',b'preserved')
os.link(p/'a/data',p/'hardlink')
os.symlink('/etc/cell/secret',p/'outside-link')
os.symlink('a/data',p/'relative-link')
os.mkfifo(p/'fifo',0o600)
with (p/'sparse').open('wb') as f:
 f.seek(16*1024*1024); f.write(b'end')
os.utime(p/'a/data',ns=(1720000000123456789,1720000000123456789))
'''
m.run(m.namespace(helper, source, ['/usr/bin/python3','-c',make], writable=True))
def manifest(payload):
    return m.run(m.namespace(helper,payload,['/usr/bin/python3','/snapshot.py','manifest']),capture_output=True).stdout
expected = manifest(source)
archive = work/'fixture.tar.gz'
with archive.open('xb') as f:
    m.run(m.namespace(helper,source,['/usr/bin/tar','--create','--use-compress-program=/bin/gzip -1','--format=pax',
        '--numeric-owner','--acls','--xattrs','--xattrs-include=*','--sparse','-C','/payload','.']),stdout=f)
archive.chmod(0o444)
restored = work/'restored'
restored.mkdir()
os.chown(restored,m.BASE,m.BASE)
m.run(m.namespace(helper,restored,['/usr/bin/tar','--extract','--gzip','--file=/archive/input.tar.gz',
    '--numeric-owner','--same-owner','--same-permissions','--acls','--xattrs','--xattrs-include=*',
    '--delay-directory-restore','-C','/payload'],writable=True,archive=archive))
assert manifest(restored)==expected, 'round-trip metadata/content mismatch'
print('PASS: UID mapping, no host secret, private network, contents, modes, owners, hardlinks, symlinks, xattrs, FIFO, sparse file, nanosecond timestamps',flush=True)
# Even a malicious archive cannot write a host path through absolute names,
# parent traversal, or an earlier symlink. The helper has no host-root mount.
sentinel = work/'host-sentinel'
sentinel.write_bytes(b'host unchanged')
evil = work/'evil.tar.gz'
with tarfile.open(evil,'w:gz') as f:
    for name in (str(sentinel), '../../'+str(sentinel).lstrip('/')):
        entry=tarfile.TarInfo(name); entry.size=4; f.addfile(entry,io.BytesIO(b'evil'))
    link=tarfile.TarInfo('escape'); link.type=tarfile.SYMTYPE; link.linkname=str(work)
    f.addfile(link)
    entry=tarfile.TarInfo('escape/host-sentinel'); entry.size=4; f.addfile(entry,io.BytesIO(b'evil'))
evil.chmod(0o444)
bad=work/'malicious'; bad.mkdir(); os.chown(bad,m.BASE,m.BASE)
sp.run(m.namespace(helper,bad,['/usr/bin/tar','-xzf','/archive/input.tar.gz','-C','/payload'],
    writable=True,archive=evil),stdout=sp.DEVNULL,stderr=sp.DEVNULL)
assert sentinel.read_bytes()==b'host unchanged'
print('PASS: malicious archive traversal and symlinks cannot modify host state',flush=True)
