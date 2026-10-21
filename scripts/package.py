"""Package an already built native release. No installation or system changes."""
import argparse
import hashlib
import json
import platform
import plistlib
from pathlib import Path
import shutil
import tarfile
import zipfile

root = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser()
parser.add_argument('--binary', type=Path)
args = parser.parse_args()
system = platform.system()
binary = args.binary or root / 'target' / 'release' / ('aservice.exe' if system == 'Windows' else 'aservice')
if not binary.is_file():
    parser.error(f'Build first with cargo build --release --locked: {binary}')
out = root / 'artifacts' / 'packages'
out.mkdir(parents=True, exist_ok=True)
name = f'aservice-{system.lower()}-{platform.machine().lower()}'
# Keep previous packages; each build gets a separate staging directory.
import tempfile
with tempfile.TemporaryDirectory(prefix=name + '-', dir=out) as tmp:
    staging = Path(tmp) / name
    staging.mkdir()
    shutil.copy2(root / 'README.md', staging)
    shutil.copy2(root / 'LICENSE', staging)
    shutil.copy2(root / 'assets' / 'aservice.svg', staging)
    shutil.copy2(root / 'assets' / 'aservice.png', staging)
    if system == 'Darwin':
        contents = staging / 'AService.app' / 'Contents'
        exe = contents / 'MacOS' / 'aservice'
        exe.parent.mkdir(parents=True)
        shutil.copy2(binary, exe)
        exe.chmod(0o755)
        resources = contents / 'Resources'
        resources.mkdir()
        shutil.copy2(root / 'assets' / 'aservice.icns', resources)
        with (contents / 'Info.plist').open('wb') as f:
            plistlib.dump({'CFBundleName': 'AService', 'CFBundleDisplayName': 'AService',
                          'CFBundleExecutable': 'aservice', 'CFBundleIdentifier': 'dev.aservice.desktop',
                          'CFBundleVersion': '0.1.0', 'CFBundleShortVersionString': '0.1.0',
                          'CFBundlePackageType': 'APPL', 'CFBundleIconFile': 'aservice.icns',
                          'NSHighResolutionCapable': True,
                          'LSMinimumSystemVersion': '11.0'}, f)
    else:
        exe = staging / binary.name
        shutil.copy2(binary, exe)
        if system == 'Windows':
            shutil.copy2(root / 'assets' / 'aservice.ico', staging)
        else:
            exe.chmod(0o755)
            (staging / 'aservice.desktop').write_text('[Desktop Entry]\nType=Application\nName=AService\nComment=Manage background applications\nExec=aservice\nIcon=aservice\nTerminal=false\nCategories=System;Utility;\n', encoding='utf-8')
    if system == 'Windows':
        archive = out / (name + '.zip')
        with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED) as z:
            for f in staging.rglob('*'):
                if f.is_file():
                    z.write(f, f.relative_to(staging.parent))
    else:
        archive = out / (name + '.tar.gz')
        with tarfile.open(archive, 'w:gz') as t:
            t.add(staging, arcname=name)
digest = hashlib.sha256(archive.read_bytes()).hexdigest()
archive.with_suffix(archive.suffix + '.sha256').write_text(f'{digest}  {archive.name}\n', encoding='utf-8')
print(json.dumps({'archive': str(archive), 'sha256': digest}))
