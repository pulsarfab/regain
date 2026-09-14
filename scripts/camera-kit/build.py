"""Build a Windows folder distribution; users need no Python or compiler."""
import argparse
import importlib.metadata
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import uuid
import xml.etree.ElementTree as ET
import zipfile

from camera_kit import sha
from verify_host import verify_host


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--stage', type=Path, default=Path('artifacts/stage'))
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    stage = args.stage.resolve()
    version = ET.parse(root / 'Directory.Build.props').findtext('.//Version')
    work = root / 'artifacts' / ('camera-kit-build-' + uuid.uuid4().hex[:8])
    work.mkdir(parents=True)
    subprocess.run([sys.executable, '-m', 'PyInstaller', '--noconfirm', '--clean', '--onedir',
                    '--name', 'ZwoGain-CameraKit', '--contents-directory', 'runtime',
                    '--distpath', str(work / 'dist'), '--workpath', str(work / 'work'), '--specpath', str(work),
                    '--collect-all', 'frida', '--add-data', str(root / 'scripts/inspection/trace-transport.js') + ';.',
                    str(Path(__file__).with_name('camera_kit.py'))], check=True)
    kit = work / 'dist/ZwoGain-CameraKit'
    for name in ('zwogain-host.exe', 'ASICamera2.dll', 'LICENSE', 'THIRD_PARTY_NOTICES.md'):
        shutil.copy2(stage / name, kit / name)
    shutil.copytree(stage / 'licenses', kit / 'licenses')
    for name in ('vcruntime140.dll', 'vcruntime140_1.dll'):
        source = Path(sys.base_prefix) / name
        if source.exists():
            shutil.copy2(source, kit / name)
    if not (kit / 'vcruntime140.dll').exists():
        raise RuntimeError('Python installation must supply vcruntime140.dll for the Rust host')
    shutil.copy2(Path(sys.base_prefix) / 'LICENSE.txt', kit / 'licenses/Python.txt')
    for name in ('frida', 'pyinstaller', 'pyinstaller-hooks-contrib', 'altgraph', 'packaging', 'pefile', 'pywin32-ctypes', 'setuptools'):
        dist = importlib.metadata.distribution(name)
        dest = kit / 'licenses' / f'{name}-{dist.version}'
        dest.mkdir()
        copied = 0
        for file in dist.files or []:
            if any(token in file.name.upper() for token in ('LICENSE', 'COPYING', 'COPYRIGHT', 'NOTICE')):
                source = dist.locate_file(file)
                if source.is_file():
                    shutil.copy2(source, dest / f'{copied}-{file.name}')
                    copied += 1
        if not copied:
            # Metadata can itself contain the complete license text.
            (dest / 'METADATA.txt').write_text(dist.read_text('METADATA'), encoding='utf-8')
    shutil.copy2(Path(__file__).with_name('README.md'), kit / 'README.md')
    source_dir = kit / 'source'
    source_dir.mkdir()
    for source in Path(__file__).parent.iterdir():
        if source.is_file():
            shutil.copy2(source, source_dir / source.name)
    shutil.copy2(root / 'scripts/inspection/trace-transport.js', source_dir / 'trace-transport.js')
    shutil.copy2(root / 'vendor/zwo/ASICamera2.h', source_dir / 'ASICamera2.h')
    commit = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
    dirty = bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=root, text=True).strip())
    build = dict(version=version, commit=commit, dirty=dirty, python=sys.version.split()[0],
                 dependencies={name: importlib.metadata.version(name) for name in ('frida', 'pyinstaller')})
    (kit / 'camera-kit-build.json').write_text(json.dumps(build, indent=2) + '\n', encoding='utf-8')
    verify_host(kit / 'zwogain-host.exe', kit / 'ASICamera2.dll')
    # Exercise the frozen executable, embedded tracer, native DLL dependencies,
    # binary host protocol, sample writer and ZIP builder without hardware.
    isolated_env = dict(os.environ, PATH=str(Path(os.environ['SystemRoot']) / 'System32'))
    isolated_env.pop('PYTHONHOME', None)
    isolated_env.pop('PYTHONPATH', None)
    subprocess.run([str(kit / 'ZwoGain-CameraKit.exe'), '--self-test', '--include-pixels',
                    '--output', str(work / 'self-test')], cwd=kit, env=isolated_env, check=True, timeout=90)
    (kit / 'SHA256SUMS').write_text(''.join(f'{sha(p)}  {p.relative_to(kit).as_posix()}\n'
                                         for p in sorted(kit.rglob('*')) if p.is_file() and p.name != 'SHA256SUMS'), encoding='utf-8')
    archive = root / 'artifacts' / f'ZwoGain-CameraKit-{version}-win-x64.zip'
    with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED, compresslevel=6, strict_timestamps=False) as output:
        for p in sorted(kit.rglob('*')):
            if p.is_file():
                output.write(p, 'ZwoGain-CameraKit/' + p.relative_to(kit).as_posix())
    archive.with_suffix('.zip.sha256').write_text(f'{sha(archive)}  {archive.name}\n', encoding='utf-8')
    print(f'Camera kit: {archive}\nExtracted kit: {kit}', flush=True)


if __name__ == '__main__':
    main()
