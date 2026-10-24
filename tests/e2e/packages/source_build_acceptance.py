import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from contextlib import ExitStack
from pathlib import Path

repo = Path(os.environ.get('ROOT', Path(__file__).resolve().parents[3]))
parser = argparse.ArgumentParser()
parser.add_argument('--full', action='store_true')
parser.add_argument('--trace-resolution', action='store_true')
parser.add_argument('--release-fixture', type=Path, metavar='ABS_PATH')
arguments = parser.parse_args()
full = arguments.full
trace_resolution = arguments.trace_resolution
if arguments.release_fixture is not None:
    assert arguments.release_fixture.is_absolute(), 'release fixture requires an absolute path'
    assert arguments.release_fixture.is_dir(), 'release fixture directory is unavailable'
started = time.monotonic()
binary = repo / 'target/debug/pakro'
stage_binary = repo / 'target/debug/deps/pakro-80f217ff598af8de'
source = Path(os.environ.get('PAKRO_BYTECODE_RECOVERY_ARCHIVE') or '/Users/fuxinao/Library/Caches/pakro/python/native-v1/artifacts/sha256-d81d757a9785850393cd695a9a686c765a6370fbdc83738589ef2ecd3e5c44b3/cpython-3.11.17+20261013-aarch64-apple-darwin-install_only.tar.gz')
assert hashlib.sha256(source.read_bytes()).hexdigest() == 'd81d757a9785850393cd695a9a686c765a6370fbdc83738589ef2ecd3e5c44b3'
root = Path(tempfile.mkdtemp(prefix='isat-sdist-', dir=os.environ['TMPDIR']))
data, cache = root / 'data', root / 'cache'
env = os.environ.copy()
env.update(PAKRO_BYTECODE_RECOVERY_ARCHIVE=str(source),
           PAKRO_BYTECODE_RECOVERY_STAGE_DATA_ROOT=str(data),
           PAKRO_BYTECODE_RECOVERY_STAGE_CACHE_ROOT=str(cache))
result = subprocess.run([str(stage_binary), 'libraries::language::recovery::tests::stage_real_bytecode_recovery',
                         '--exact', '--ignored', '--nocapture'], cwd=repo, env=env,
                        capture_output=True, text=True)
if result.returncode:
    print(result.stdout + result.stderr, flush=True)
    raise SystemExit(result.returncode)
assert 'RECOVERY_STAGED_PYTHON=' in result.stdout
print('isolated original Python3.11 staged', flush=True)
project = root / 'project'
project.mkdir()
config = project / 'pakro.toml'
config.write_text('[tools]\npython="3.11.17+20261013"\n[packages."python@3.11.17+20261013"]\niopath="0.1.10"\nhydra-core="1.3.7"\n')
if full:
    original = Path('/Users/fuxinao/Projects/ISAT')
    pin_body = original.joinpath('mise.toml').read_text().split('[packages."python@3.11.17"]', 1)[1]
    pins = re.findall(r'^([A-Za-z0-9_-]+)\s*=\s*"([^"\n]+)"\s*$', pin_body, re.MULTILINE)
    assert len(pins) == 24 and len(dict(pins)) == 24
    config.write_text('[tools]\npython="3.11.17+20261013"\n[packages."python@3.11.17+20261013"]\n' + ''.join(f'{name}="{version}"\n' for name, version in pins))
    shutil.copytree(original / 'ISAT', project / 'ISAT', ignore=shutil.ignore_patterns('__pycache__', '*.pyc', '*.pth', '*.pt', '*.ckpt', '*.onnx', '*.safetensors'))
    shutil.copy2(original / 'main.py', project / 'main.py')
before = config.read_bytes()
manifest, = (data / 'languages/.control/catalog').glob('language-*.json')
original_manifest = manifest.read_bytes()
env = {key: value for key, value in os.environ.items() if not key.startswith(('PAKRO_', 'XDG_'))}
env.update(HOME=str(root / 'home'), PAKRO_DATA_DIR=str(data), PAKRO_CACHE_DIR=str(cache),
           PAKRO_CONFIG_DIR=str(root / 'config'), PAKRO_STATE_DIR=str(root / 'state'),
           PAKRO_SYSTEM_DIR=str(root / 'system'), PAKRO_TRUSTED_CONFIG_PATHS=str(root),
           PAKRO_HISTORY_NOTIFY='0', PAKRO_USE_VERSIONS_HOST='0', PAKRO_PYTHON_GITHUB_ATTESTATIONS='0',
           PAKRO_PYTHON_COMPILE='0', PAKRO_CACHE_PRUNE_AGE='0', PAKRO_YES='1',
           PAKRO_HTTP_TIMEOUT='2m', HTTPS_PROXY='socks5h://127.0.0.1:7892',
           HTTP_PROXY='socks5h://127.0.0.1:7892', NO_PROXY='', no_proxy='',
           PATH=str(binary.parent) + ':/usr/bin:/bin:/usr/sbin:/sbin', PAKRO_TEST_BINARY=str(binary))
(root / 'home').mkdir()
if trace_resolution:
    # Existing solver INFO only; HTTP debug can contain signed locators.
    env['PAKRO_LOG_VERBOSE_DEPS'] = '1'
fixture_stack = ExitStack()
release_fixture = None
try:
    if arguments.release_fixture is not None:
        from isat_release_fixture import start_fixture
        release_fixture = fixture_stack.enter_context(start_fixture(arguments.release_fixture))
        env['PAKRO_URL_REPLACEMENTS'] = json.dumps({
            'https://pypi.org': release_fixture.origin,
            'https://files.pythonhosted.org': release_fixture.origin,
        })
        env.update(NO_PROXY='localhost,127.0.0.1,::1', no_proxy='localhost,127.0.0.1,::1')
    print('installing full ISAT24 closure' if full else 'installing iopath/Hydra with their actual source-only dependencies', flush=True)
    if trace_resolution:
        result = subprocess.Popen([str(binary), 'deps', 'install', 'python'], cwd=project, env=env,
                                  stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
        traced_bytes = 0
        while True:
            line = result.stdout.readline(16 * 1024 + 1)
            if not line:
                break
            rendered = f'[{time.monotonic() - started:7.2f}s] {line.rstrip()}\n'
            traced_bytes += len(rendered.encode('utf-8'))
            if len(line) > 16 * 1024 or traced_bytes > 32 * 1024 * 1024:
                result.terminate()
                try:
                    result.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    result.kill()
                    result.wait(timeout=3)
                raise AssertionError('solver diagnostic line or total text limit exceeded')
            print(rendered, end='', flush=True)
        result.wait()
    else:
        result = subprocess.run([str(binary), 'deps', 'install', 'python'], cwd=project, env=env,
                                stderr=subprocess.STDOUT, text=True)
    if result.returncode:
        raise SystemExit(result.returncode)
    check = root / 'check.sh'
    check.write_text('''set -euo pipefail
eval "$("$PAKRO_TEST_BINARY" hook-env -s bash --force)"
python - <<'PY'
import importlib.metadata as m
import tempfile
from pathlib import Path
from iopath.common.file_io import PathManager
from hydra.core.override_parser.overrides_parser import OverridesParser
assert m.version('iopath') == '0.1.10'
assert m.version('hydra-core') == '1.3.7'
assert m.version('antlr4-python3-runtime') == '4.9.3'
with tempfile.TemporaryDirectory() as d:
    path = str(Path(d) / 'payload.txt')
    pm = PathManager()
    with pm.open(path, 'w') as out: out.write('source-built')
    with pm.open(path) as inp: assert inp.read() == 'source-built'
parsed = OverridesParser.create().parse_overrides(['answer=42', '+items=[one,two]'])
assert len(parsed) == 2
assert parsed[0].value() == 42
print('iopath0.1.10 PathManager and Hydra1.3.7 ANTLR4.9.3 actual override parsing passed')
PY
''')
    subprocess.run(['/bin/bash', str(check)], cwd=project, env=env, check=True)
    if full:
        env.update(QT_QPA_PLATFORM='offscreen', HF_HUB_OFFLINE='1', TRANSFORMERS_OFFLINE='1',
                   ISAT_EXPECTED_PINS=json.dumps(pins), ISAT_EXPECTED_SOURCE_ROOT=str(project),
                   PYTHONPATH=str(project))
        smoke = root / 'smoke.sh'
        smoke.write_text('''set -euo pipefail
eval "$("$PAKRO_TEST_BINARY" hook-env -s bash --force)"
export PYTHONPATH="$ISAT_EXPECTED_SOURCE_ROOT${PYTHONPATH:+:$PYTHONPATH}"
python - <<'PY'
import importlib.metadata as metadata
import json, os, sys, tempfile
from pathlib import Path
sys.path.insert(0, os.environ['ISAT_EXPECTED_SOURCE_ROOT'])
for name, version in json.loads(os.environ['ISAT_EXPECTED_PINS']):
    actual = metadata.version(name)
    assert actual == version, (name, version, actual)
import numpy as np
import cv2, torch, torchvision
from PIL import Image
from shapely.geometry import Point
from pycocotools import mask
from skimage.color import rgb2gray
assert cv2.cvtColor(np.zeros((2,2,3), dtype=np.uint8), cv2.COLOR_RGB2GRAY).shape == (2,2)
assert rgb2gray(np.zeros((2,2,3))).shape == (2,2)
assert Image.new('RGB', (2,2)).size == (2,2)
assert Point(0,0).buffer(1).area > 3
assert mask.encode(np.asfortranarray(np.ones((2,2), dtype=np.uint8)))['size'] == [2,2]
assert (torch.ones(2) + 1).tolist() == [2,2]
boxes = torch.tensor([[0,0,2,2], [0,0,2,2]], dtype=torch.float32)
scores = torch.tensor([0.9, 0.8], dtype=torch.float32)
assert torchvision.ops.nms(boxes, scores, 0.5).tolist() == [0]
from PyQt5 import QtWidgets
app = QtWidgets.QApplication([])
widget = QtWidgets.QWidget()
widget.resize(100,100)
app.processEvents()
widget.close()
import ISAT.main
assert Path(ISAT.main.__file__).resolve().is_relative_to(
    Path(os.environ['ISAT_EXPECTED_SOURCE_ROOT']).resolve() / 'ISAT')
from ISAT.annotation import Annotation, Object
from ISAT.formats.isat import ISAT as IsatFormat
from ISAT.formats.labelme import LABELME
from ISAT.formats.voc import VOC
import imgviz
with tempfile.TemporaryDirectory() as directory:
    directory = Path(directory)
    image_path, label_path = directory / 'tiny.png', directory / 'tiny.json'
    Image.new('RGB', (8,6), (17,31,47)).save(image_path)
    points = [[1,1], [5,1], [5,4], [1,4]]
    annotation = Annotation(str(image_path), str(label_path))
    assert (annotation.width, annotation.height, annotation.depth) == (8,6,3)
    annotation.note = '图像备注'
    annotation.objects.append(Object('target', 1, points, 12.0, 2, [1,1,5,4], False, '目标备注'))
    assert annotation.save_annotation() is True
    restored = Annotation(str(image_path), str(label_path)).load_annotation()
    assert (restored.width, restored.height, restored.depth) == (8,6,3)
    assert restored.note == annotation.note and len(restored.objects) == 1
    restored_object = restored.objects[0]
    assert (restored_object.category, restored_object.group) == ('target', 1)
    assert restored_object.segmentation == points and restored_object.bbox == [1,1,5,4]
    assert (restored_object.area, restored_object.layer, restored_object.iscrowd) == (12.0,2,False)
    assert restored_object.note == '目标备注'
    formatted = IsatFormat().load_one_isat_json(str(label_path))
    labelme_path = directory / 'labelme.json'
    labelme = LABELME()
    assert labelme.save_one_labelme_json(formatted, str(labelme_path)) is True
    imported = labelme.load_one_labelme_json(str(labelme_path))
    assert (imported.info.width, imported.info.height) == (8,6)
    assert len(imported.objs) == 1
    assert (imported.objs[0].category, imported.objs[0].group) == ('target', 1)
    assert imported.objs[0].segmentation == points
    mask_path = directory / 'mask.png'
    assert VOC().save_one_voc_png(formatted, str(mask_path), imgviz.label_colormap(),
                                  {'background':0, 'target':1}) is True
    with Image.open(mask_path) as mask_image:
        assert mask_image.mode == 'P'
        exported_mask = np.array(mask_image)
    assert exported_mask.shape == (6,8)
    assert exported_mask[2,2] == 1 and exported_mask[0,0] == 0
print('ISAT exact24 pins, native ops, offscreenQt, annotation roundtrip and LabelMe/VOC export passed')
app.quit()
PY
''')
        subprocess.run(['/bin/bash', str(smoke)], cwd=project, env=env, check=True)
    assert config.read_bytes() == before and manifest.read_bytes() == original_manifest
    requests_before_reuse = None if release_fixture is None else len(release_fixture.requests)
    env.update(HTTP_PROXY='http://127.0.0.1:9', HTTPS_PROXY='http://127.0.0.1:9',
               http_proxy='http://127.0.0.1:9', https_proxy='http://127.0.0.1:9',
               ALL_PROXY='http://127.0.0.1:9', all_proxy='http://127.0.0.1:9')
    subprocess.run([str(binary), 'deps', 'install', 'python'], cwd=project, env=env, check=True)
    second = root / 'second-project'
    second.mkdir()
    (second / 'pakro.toml').write_bytes(before)
    subprocess.run([str(binary), 'deps', 'install', 'python'], cwd=second, env=env, check=True)
    subprocess.run(['/bin/bash', str(check)], cwd=second, env=env, check=True)
    if full:
        subprocess.run(['/bin/bash', str(smoke)], cwd=second, env=env, check=True)
    assert config.read_bytes() == before and manifest.read_bytes() == original_manifest
    assert (second / 'pakro.toml').read_bytes() == before
    if release_fixture is not None:
        assert not release_fixture.missing, 'release fixture encountered missing public inputs'
        assert len(release_fixture.requests) == requests_before_reuse, 'ready reuse fetched registry inputs'
    print('ISAT source packages actual build/install/use/repeat/cross-project no-network reuse passed', flush=True)
finally:
    try:
        fixture_stack.close()
    finally:
        if release_fixture is not None:
            for missing in release_fixture.missing:
                name = Path(str(missing).split('?', 1)[0]).name
                print(f'release fixture missing public input: {name}', flush=True)
