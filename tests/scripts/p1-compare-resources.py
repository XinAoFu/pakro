#!/usr/bin/env python3
"""Compare the same recorded selection fixture across two production versions."""
import argparse
import json
from pathlib import Path
import statistics

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--before',type=Path,required=True)
parser.add_argument('--after',type=Path,required=True)
parser.add_argument('--output',type=Path,required=True)
args=parser.parse_args()
before=json.loads(args.before.read_text());after=json.loads(args.after.read_text())
for key in ('platform','machine','profile'):
    assert before[key]==after[key], ('configuration mismatch',key)
assert len(before['records'])==len(after['records'])==2
comparisons=[]
for left,right in zip(before['records'],after['records']):
    assert left['process_kind']==right['process_kind'] and left['test']==right['test']
    a,=left['observations'];b,=right['observations']
    for key in ('scenario','graph','language','runtime','packages','package_files','package_bytes','offline'):
        assert a[key]==b[key],('fixture mismatch',key,a[key],b[key])
    assert a['registry_connections']==b['registry_connections']==0
    assert a['fd_initial']==a['fd_released']==b['fd_initial']==b['fd_released']
    for x,y in zip(a['samples'],b['samples']):
        for key in ('iteration','query_package_inventories','query_language_inventories','apply_package_inventories','apply_language_inventories'):
            assert x[key]==y[key],('inventory work changed',key,x[key],y[key])
    comparisons.append(dict(process_kind=left['process_kind'],before=a,after=b))
def measurements(doc,key,iteration=None):
    return [sample[key] for record in doc['records'] for item in record['observations']
            for sample in item['samples'] if iteration is None or sample['iteration']==iteration]
summary={}
for label,key,iteration in [('query','query_match_verify_ms',None),('first_apply','apply_query_and_commit_ms',0),('repeated_apply','apply_query_and_commit_ms',1)]:
    left=statistics.median(measurements(before,key,iteration));right=statistics.median(measurements(after,key,iteration))
    summary[label]=dict(before_median_ms=left,after_median_ms=right,change_percent=100*(right/left-1))
result=dict(schema=1,before_label=before['label'],after_label=after['label'],
            before_binary_sha256=before['binary_sha256'],after_binary_sha256=after['binary_sha256'],
            configuration=dict(platform=before['platform'],machine=before['machine'],profile=before['profile']),
            equivalent_fixture=True,inventory_work_unchanged=True,zero_requests=True,no_fd_accumulation=True,
            timings=summary,records=comparisons,
            scope='Production selection/apply core only; deterministic runtime and real wheel trees; excludes CLI startup and full ISAT. Timing samples are observational, not a speed guarantee.')
args.output.write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(summary,indent=2))
print('Equivalent fixture, unchanged inventory counts, zero requests and full FD release verified.')
