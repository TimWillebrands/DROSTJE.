import re
import sys


def release_version(tag):
    number = r"(0|[1-9][0-9]*)"
    match = re.fullmatch(
        rf"android-v{number}\.{number}\.{number}(?:-rc\.([1-9][0-9]*))?", tag
    )
    if match is None:
        raise ValueError("Expected android-vMAJOR.MINOR.PATCH or android-vMAJOR.MINOR.PATCH-rc.N")
    major, minor, patch = map(int, match.group(1, 2, 3))
    candidate = int(match[4]) if match[4] else 99
    if major > 209 or minor > 99 or patch > 999 or not 1 <= candidate <= 99:
        raise ValueError("Version limits: major 0..209, minor 0..99, patch 0..999, rc 1..98")
    if match[4] and candidate == 99:
        raise ValueError("Release candidates must be in 1..98; 99 is reserved for stable releases")
    return {
        "version_name": tag.removeprefix("android-v"),
        "version_code": major * 10_000_000 + minor * 100_000 + patch * 100 + candidate,
        "prerelease": "true" if match[4] else "false",
    }


if __name__ == "__main__":
    try:
        if len(sys.argv) != 2:
            raise ValueError("Pass one Android release tag")
        version = release_version(sys.argv[1])
    except ValueError as error:
        sys.exit(str(error))
    for key, value in version.items():
        print(f"{key}={value}")
