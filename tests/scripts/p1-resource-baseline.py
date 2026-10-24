#!/usr/bin/env python3
"""Record bounded, isolated P1 query/apply and descriptor observations.

The executable is supplied by cargo's build output; no machine-specific hash,
archive, user library or ISAT path is embedded in this driver.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--test-binary', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--test-filter', default='p1_resource_sixty_four_package_query_and_apply_baseline')
parser.add_argument('--label', default='current')
parser.add_argument('--ignored', action='store_true')
args = parser.parse_args()
binary = args.test_binary.resolve(strict=True)
root = Path(__file__).resolve().parents[2]
listed = subprocess.run([str(binary), args.test_filter, '--list'], check=True, text=True, capture_output=True).stdout
names = [line.removesuffix(': test') for line in listed.splitlines() if line.endswith(': test')]
if not names:
    raise SystemExit('No registered P1 resource test; zero tests cannot be accepted')
records = []
for process_kind in ('cold-process', 'repeat-process'):
    for name in names:
        started = time.monotonic()
        run = subprocess.run([str(binary), name, '--exact', '--nocapture', '--test-threads=1'] + (['--ignored'] if args.ignored else []),
                             text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                             timeout=240)
        print(run.stdout, end='', flush=True)
        if run.returncode:
            raise SystemExit(run.returncode)
        observations = []
        for line in run.stdout.splitlines():
            if 'P1_RESOURCE ' in line:
                observations.append(json.loads(line.split('P1_RESOURCE ', 1)[1]))
        if not observations or '1 passed; 0 failed;' not in run.stdout:
            raise SystemExit('Resource test did not produce actual measurements and one passing test')
        records.append(dict(process_kind=process_kind, test=name,
                            elapsed_seconds=time.monotonic()-started, observations=observations))
metadata = dict(schema=1, label=args.label, platform=platform.platform(), machine=platform.machine(),
                git_head=subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),
                dirty=bool(subprocess.check_output(['git','status','--porcelain'],cwd=root)),
                binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                profile='test, all-features, incremental=0, debug=0',
                records=records,
                scope='Owned fixed graph; full ISAT and hard256 capacity are not claimed')
args.output.parent.mkdir(parents=True, exist_ok=True)
args.output.write_text(json.dumps(metadata, indent=2)+'\n')
print(f'Resource observations: {args.output}', flush=True)
