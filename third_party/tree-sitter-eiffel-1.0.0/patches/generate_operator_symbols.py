"""Regenerate the scanner's printable operator ranges from retained Unicode data.

This maintenance command is never invoked by compilation or packaging. Its input
and both original license notices live beside this script under unicode/.
"""
import argparse
import hashlib
from pathlib import Path
import re


def scalar(text):
    if text.startswith("\\u{") and text.endswith("}"):
        return int(text[3:-1], 16)
    escapes = {"\\'": "'", '\\"': '"', "\\\\": "\\", "\\n": "\n", "\\r": "\r", "\\t": "\t", "\\0": "\0"}
    value = escapes.get(text, text)
    if len(value) != 1:
        raise ValueError(f"unsupported retained scalar spelling: {text!r}")
    return ord(value)


def main():
    """Write the deterministic C header to the explicit output path."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    arguments = parser.parse_args()
    source = Path(__file__).parent / "unicode/regex-syntax-general-category.rs"
    raw = source.read_bytes()
    if hashlib.sha256(raw).hexdigest() != "9488e3721f7c2ae20e1b77fcff9a59b4ed8f22954b8645ea6d8592eac1856423":
        raise ValueError("retained Unicode category input digest changed")
    text = raw.decode("utf-8")
    codepoints = set()
    for category in ["MARK", "NUMBER", "PUNCTUATION", "SYMBOL"]:
        block = text.split(f"pub const {category}: &'static [(char, char)] = &[\n", 1)[1].split("\n];", 1)[0]
        for line in block.splitlines():
            matched = re.fullmatch(r"\s*\('((?:\\.|[^'])*)', '((?:\\.|[^'])*)'\),", line)
            if matched is None:
                raise ValueError(f"unsupported retained category row: {line!r}")
            first, last = map(scalar, matched.groups())
            if first > last:
                raise ValueError("reversed retained Unicode range")
            codepoints.update(range(first, last + 1))
    # ECMA symbolic operators exclude identifier digits and Eiffel punctuation.
    codepoints.difference_update(map(ord, "_0123456789:;,?'\"$.=()[]{}"))
    ranges = []
    for value in sorted(codepoints):
        if ranges and ranges[-1][1] + 1 == value:
            ranges[-1][1] = value
        else:
            ranges.append([value, value])
    header = "// Generated from retained regex-syntax 0.8.11 Unicode 16.0.0 categories.\n// Original category tables and MIT/Apache-2.0 notices are preserved in patches/unicode/.\nstatic const TSCharacterRange operator_symbols[] = {\n"
    header += "".join(f"  {{0x{first:x}, 0x{last:x}}},\n" for first, last in ranges)
    header += "};\n"
    arguments.output.write_text(header, encoding="utf-8", newline="\n")


if __name__ == "__main__":
    main()
