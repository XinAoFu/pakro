#!/usr/bin/env python3
"""Short capability tests; no uncontained fork/setsid fixture is launched."""
import errno
import importlib.util
import json
import os
from pathlib import Path
import resource
import signal
import subprocess
import sys
import tempfile
import time

spec=importlib.util.spec_from_file_location('p1_watchdog',Path(__file__).with_name('p1-watchdog.py'))
watchdog=importlib.util.module_from_spec(spec);spec.loader.exec_module(watchdog)


def run_tests():
    before=resource.getrlimit(resource.RLIMIT_NPROC)
    with tempfile.TemporaryDirectory(prefix='p1-watchdog-capability-') as temporary:
        root=Path(temporary)
        for delay in (0.025,0.04,0.06):
            marker=root/('must-not-start-'+str(delay))
            command=[sys.executable,'-c','import pathlib,sys;pathlib.Path(sys.argv[1]).touch()',str(marker)]
            with (root/'denied.log').open('w') as output:
                result=watchdog.supervise(command,output,timeout=1)
            assert result['exit']==125 and result['launched'] is False, result
            assert result['coverage_complete'] is False and result['survivors'] is None,result
            assert not marker.exists(), 'unsupported tree runner launched its command'
            print('PASS rapid-detachment case refused before spawn:',delay,flush=True)
        if sys.platform!='darwin' or os.geteuid()==0:
            print('Single-process backend unsupported here; no enforcement pass is claimed.',flush=True)
            return
        restricted='''import errno,json,os,resource,subprocess,sys,threading
assert resource.getrlimit(resource.RLIMIT_NPROC)==(0,0)
try:
    child=os.fork()
except OSError as error:
    assert error.errno in (errno.EAGAIN,errno.EPERM),error
else:
    if child==0:os._exit(0)
    os.waitpid(child,0)
    raise AssertionError('fork was not blocked')
try:
    child=subprocess.Popen([sys.executable,'-c','pass'],start_new_session=True)
except OSError as error:
    assert error.errno in (errno.EAGAIN,errno.EPERM),error
else:
    child.wait()
    raise AssertionError('new session child was not blocked')
seen=[]
thread=threading.Thread(target=lambda:seen.append(True));thread.start();thread.join()
assert seen==[True]
print('kernel fork/spawn denial and thread creation verified')
'''
        with (root/'restricted.log').open('w') as output:
            result=watchdog.supervise([sys.executable,'-c',restricted],output,timeout=2,mode='single-process')
        assert result['exit']==0 and result['coverage_complete'] and result['survivors']==[],result
        assert 'kernel fork/spawn denial' in (root/'restricted.log').read_text()
        assert resource.getrlimit(resource.RLIMIT_NPROC)==before,'parent NPROC changed'
        print('PASS explicit single-process kernel enforcement, parent unchanged',flush=True)
        leaf='import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);time.sleep(30)'
        with (root/'timeout.log').open('w') as output:
            result=watchdog.supervise([sys.executable,'-c',leaf],output,timeout=1,mode='single-process')
        assert result['exit']==124 and result['survivors']==[] and result['elapsed_seconds']<1,result
        print('PASS single owned process ignores TERM, is killed within total budget',flush=True)
        assert resource.getrlimit(resource.RLIMIT_NPROC)==before


if __name__=='__main__':run_tests()
