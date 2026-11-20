"""Select an existing, reachable stable package tag for release comparison."""
import argparse
import pathlib
import re
import subprocess
import sys


def stable_version(value):
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", value):
        raise ValueError(f"expected a stable major.minor.patch version, got {value!r}")
    return tuple(int(part) for part in value.split("."))


def select_baseline(package, version, repository="."):
    current = stable_version(version)
    if not re.fullmatch(r"[A-Za-z0-9_-]+", package):
        raise ValueError(f"invalid package name: {package!r}")
    prefix = f"{package}-v"
    result = subprocess.run(
        ["git", "tag", "--list", f"{prefix}*", "--merged", "HEAD"],
        cwd=repository, check=True, capture_output=True, text=True,
    )
    candidates = []
    for tag in result.stdout.splitlines():
        if not tag.startswith(prefix):
            continue
        try:
            released = stable_version(tag[len(prefix):])
        except ValueError:
            continue
        if released <= current:
            candidates.append((released, tag))
    if not candidates:
        raise ValueError(
            f"no reachable stable {package} release tag at or below {version}; "
            "fetch release tags/history before preparing the comparison checkout"
        )
    return max(candidates)[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", required=True)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    try:
        print(select_baseline(args.package, args.version, pathlib.Path.cwd()))
    except (ValueError, subprocess.CalledProcessError) as error:
        print(f"release baseline: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
