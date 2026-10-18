"""Turns the upstream source into this repository's build.

    python prepare.py <source dir> <version> <owner/repo>

Sets the version this build reports, and points the app's built-in updater
at this repository's releases instead of crmne/spotifast's, so copies
installed from here update to builds that keep Local Files.
"""

import pathlib
import re
import sys

source, version, repository = sys.argv[1:]
root = pathlib.Path(source)

cargo = root / "Cargo.toml"
text = cargo.read_text(encoding="utf-8")
text, count = re.subn(r'(?m)^version = "[^"]+"', f'version = "{version}"', text, count=1)
assert count == 1, "Cargo.toml has no package version line"
cargo.write_text(text, encoding="utf-8")

updates = root / "src" / "updates.rs"
text = updates.read_text(encoding="utf-8")
assert text.count('"crmne/spotifast"') == 1, "updates.rs no longer names crmne/spotifast once"
text = text.replace('"crmne/spotifast"', f'"{repository}"')
# Upstream's releases are signed with crmne's key, which this repository
# does not have. Its releases are checked against their published
# checksums instead, downloaded from GitHub over HTTPS.
key = 'publisher_key: Some(include_str!("../assets/update-public-key.hex")),'
assert text.count(key) == 1, "updates.rs no longer sets the publisher key this way"
text = text.replace(key, "publisher_key: None,")
updates.write_text(text, encoding="utf-8")

print(f"Prepared Spotifast {version}, updating from {repository}")
