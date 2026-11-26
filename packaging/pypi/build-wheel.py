"""Writes the rembro wheel from the launcher package next to this script and the files in FILES:

    python3 build-wheel.py VERSION FILES OUT

FILES holds the six rembro builds, README.md, LICENSE-MIT and LICENSE-APACHE. The wheel is a
zip of files that are already built, so this writes it with the standard library alone.
"""

import base64
import hashlib
import sys
import zipfile
from pathlib import Path

BUILDS = [
    "rembro-x86_64",
    "rembro-aarch64",
    "rembro-armv7",
    "rembro-macos-aarch64",
    "rembro-macos-x86_64",
    "rembro-windows-x86_64.exe",
]
LICENSES = ["LICENSE-MIT", "LICENSE-APACHE"]

METADATA = """\
Metadata-Version: 2.4
Name: rembro
Version: {version}
Summary: Open a browser that uses the network of an SSH host, container, sandbox or Kubernetes pod
Keywords: browser,docker,kubernetes,ssh,proxy
License-Expression: MIT OR Apache-2.0
License-File: LICENSE-MIT
License-File: LICENSE-APACHE
Project-URL: Repository, https://github.com/ScriptSmith/rembro
Requires-Python: >=3.8
Description-Content-Type: text/markdown

"""

WHEEL = """\
Wheel-Version: 1.0
Generator: rembro build-wheel.py
Root-Is-Purelib: true
Tag: py3-none-any
"""


def main(version, files, out):
    files = Path(files)
    launcher = Path(__file__).resolve().parent / "rembro"
    info = f"rembro-{version}.dist-info"
    entries = [(f"rembro/{f}", (launcher / f).read_bytes(), 0o644) for f in ["__init__.py", "__main__.py"]]
    entries += [(f"rembro/{f}", (files / f).read_bytes(), 0o755) for f in BUILDS]
    entries += [(f"{info}/licenses/{f}", (files / f).read_bytes(), 0o644) for f in LICENSES]
    metadata = METADATA.format(version=version) + (files / "README.md").read_text(encoding="utf-8")
    entries += [
        (f"{info}/METADATA", metadata.encode(), 0o644),
        (f"{info}/WHEEL", WHEEL.encode(), 0o644),
        (f"{info}/entry_points.txt", b"[console_scripts]\nrembro = rembro:main\n", 0o644),
    ]
    record = "".join(f"{name},sha256={digest(data)},{len(data)}\n" for name, data, _ in entries)
    entries.append((f"{info}/RECORD", f"{record}{info}/RECORD,,\n".encode(), 0o644))

    wheel = Path(out) / f"rembro-{version}-py3-none-any.whl"
    with zipfile.ZipFile(wheel, "w") as z:
        for name, data, mode in entries:
            entry = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            # Unix file modes, which installers use to keep the builds executable.
            entry.create_system = 3
            entry.external_attr = (0o100000 | mode) << 16
            entry.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(entry, data)
    print(wheel)


def digest(data):
    return base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()


if __name__ == "__main__":
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    main(*sys.argv[1:])
