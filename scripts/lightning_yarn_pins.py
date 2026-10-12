"""Read an exact Yarn-v1 registry/local pin graph without dependency execution.

Pins retain their authored host and fragment. This adapter is separate from npm
consumer policy and proves neither archive possession nor producer build closure.
"""

from __future__ import annotations

import base64
import binascii
import hashlib
import json
import math
import re
from urllib.parse import urlsplit


MAX_LOCK_BYTES = 8 * 1024 * 1024
MAX_MANIFEST_BYTES = 512 * 1024
MAX_STANZAS = 8192
MAX_SELECTORS = 65536
MAX_EDGES = 65536
NAME_PART = r"[a-z0-9][a-z0-9._-]*"
NAME = re.compile(rf"(?:@{NAME_PART}/)?{NAME_PART}\Z")
WINDOWS_DEVICES = {"con", "prn", "aux", "nul", "conin$", "conout$"} | {
    f"{prefix}{index}" for prefix in ("com", "lpt") for index in range(1, 10)
}
VERSION = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
                     r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?\Z")


class YarnPinError(ValueError):
    """A static source-free rejection category for an unsupported or unsafe graph."""


def _require(condition, category):
    if not condition:
        raise YarnPinError(category)


def _name(value):
    _require(isinstance(value, str) and len(value) <= 256 and NAME.fullmatch(value), "invalid_package_name")
    _require(all(not part.endswith(".") and part.split(".")[0] not in WINDOWS_DEVICES
                 for part in value.split("/")), "unsafe_package_path")
    return value


def _version(value):
    _require(isinstance(value, str) and len(value) <= 256 and VERSION.fullmatch(value), "nonexact_package_version")
    return value


def _request(value):
    _require(isinstance(value, str) and 0 < len(value) <= 1024 and value == value.strip()
             and re.fullmatch(r"[0-9A-Za-z*^~<>=|.+ -]+", value), "unsupported_package_request")
    return value


def _selector(value):
    name, separator, request = value.rpartition("@")
    _require(separator and name, "invalid_selector")
    _name(name)
    if request != "link:.":
        _request(request)
    return name, request


def _object(pairs):
    result = {}
    for key, value in pairs:
        _require(key not in result, "duplicate_manifest_key")
        result[key] = value
    return result


def _manifest(payload):
    _require(type(payload) is bytes and len(payload) <= MAX_MANIFEST_BYTES, "manifest_byte_limit")
    try:
        def finite_float(value):
            number = float(value)
            _require(math.isfinite(number), "nonfinite_manifest")
            return number
        document = json.loads(payload.decode("utf-8"), object_pairs_hook=_object,
                              parse_float=finite_float,
                              parse_constant=lambda _: (_ for _ in ()).throw(YarnPinError("nonfinite_manifest")))
    except YarnPinError:
        raise
    except (UnicodeError, ValueError, RecursionError) as error:
        raise YarnPinError("invalid_manifest") from error
    _require(isinstance(document, dict), "invalid_manifest")
    _name(document.get("name"))
    _version(document.get("version"))
    _require(document.get("peerDependencies", {}) == {}, "unsupported_root_peer_dependencies")
    resolutions = document.get("resolutions", {})
    _require(isinstance(resolutions, dict), "invalid_resolutions")
    for name, target in resolutions.items():
        _require(_name(name) == document["name"] and target == "link:.", "unsupported_resolution")
    return document


def _token(text, cursor, offset):
    _require(cursor < len(text), "missing_token")
    start = cursor
    if text[cursor] == '"':
        try:
            value, cursor = json.JSONDecoder().raw_decode(text, cursor)
        except json.JSONDecodeError as error:
            raise YarnPinError("malformed_quoted_token") from error
        _require(isinstance(value, str), "nonstring_token")
    else:
        while cursor < len(text) and text[cursor] not in ' ,"':
            cursor += 1
        value = text[start:cursor]
    _require(isinstance(value, str) and (value or text[start] == '"') and len(value) <= 4096
             and all(32 <= ord(char) < 127 for char in value), "invalid_token")
    return {"value": value, "token": text[start:cursor],
            "source": {"start_byte": offset + start, "end_byte": offset + cursor}}, cursor


def _space(text, cursor):
    while cursor < len(text) and text[cursor] == " ":
        cursor += 1
    return cursor


def _pair(text, offset):
    left, cursor = _token(text, 0, offset)
    end = _space(text, cursor)
    _require(end > cursor, "missing_pair_separator")
    right, cursor = _token(text, end, offset)
    _require(cursor == len(text), "unexpected_pair_tail")
    return left, right


def _headers(text, offset):
    _require(text.endswith(":"), "missing_stanza_colon")
    text, cursor, tokens = text[:-1], 0, []
    while True:
        token, cursor = _token(text, cursor, offset)
        _selector(token["value"])
        tokens.append(token)
        _require(len(tokens) <= MAX_SELECTORS, "selector_limit")
        cursor = _space(text, cursor)
        if cursor == len(text):
            return tokens
        _require(text[cursor] == ",", "invalid_selector_separator")
        cursor = _space(text, cursor + 1)
        _require(cursor < len(text), "trailing_selector_comma")


def _read_lock(payload):
    _require(type(payload) is bytes and len(payload) <= MAX_LOCK_BYTES, "lock_byte_limit")
    _require(b"\x00" not in payload and b"\t" not in payload
             and b"\r" not in payload.replace(b"\r\n", b""), "unsupported_lock_control")
    stanzas, current, section, marker, offset, selectors, edges = [], None, None, False, 0, 0, 0
    for raw in payload.splitlines(keepends=True):
        content = raw.removesuffix(b"\n").removesuffix(b"\r")
        try:
            text = content.decode("ascii")
        except UnicodeError as error:
            raise YarnPinError("unsupported_lock_encoding") from error
        _require(all(32 <= ord(char) < 127 for char in text), "unsupported_lock_control")
        if text == "# yarn lockfile v1":
            _require(not marker and not stanzas, "invalid_version_marker")
            marker = True
        elif text.startswith("#") or not text:
            pass
        elif not text.startswith(" "):
            _require(marker, "missing_version_marker")
            current = {"index": len(stanzas), "selectors": _headers(text, offset), "fields": {}, "edges": [], "edge_names": set()}
            selectors += len(current["selectors"])
            stanzas.append(current)
            _require(len(stanzas) <= MAX_STANZAS and selectors <= MAX_SELECTORS, "lock_record_limit")
            section = None
        elif text.startswith("    ") and not text.startswith("     "):
            _require(current is not None and section is not None, "orphan_dependency_entry")
            name, request = _pair(text[4:], offset + 4)
            _name(name["value"])
            _request(request["value"])
            selector = name["value"] + "@" + request["value"]
            _require(name["value"] not in current["edge_names"], "duplicate_dependency_name")
            current["edge_names"].add(name["value"])
            current["edges"].append({"section": section, "name": name, "request": request, "selector": selector})
            edges += 1
            _require(edges <= MAX_EDGES, "dependency_edge_limit")
        else:
            _require(current is not None and text.startswith("  ") and not text.startswith("   "), "invalid_indentation")
            body = text[2:]
            if body in ("dependencies:", "optionalDependencies:"):
                section = body[:-1]
                _require(section not in current["fields"], "duplicate_dependency_section")
                current["fields"][section] = {"token": body, "source": {"start_byte": offset + 2, "end_byte": offset + len(text)}}
            else:
                field, value = _pair(body, offset + 2)
                key = field["value"]
                _require(key in ("version", "resolved", "integrity", "uid") and field["token"] == key,
                         "unknown_stanza_field")
                _require(key not in current["fields"], "duplicate_stanza_field")
                current["fields"][key] = value
                section = None
        offset += len(raw)
    _require(marker and stanzas, "missing_lock_records")
    for stanza in stanzas:
        del stanza["edge_names"]
    return stanzas


def _pin(name, version, fields):
    _require(set(fields) <= {"version", "resolved", "integrity", "dependencies", "optionalDependencies"}
             and all(key in fields for key in ("version", "resolved", "integrity")), "unbound_registry_record")
    resolved, integrity = fields["resolved"]["value"], fields["integrity"]["value"]
    try:
        parsed = urlsplit(resolved)
    except ValueError as error:
        raise YarnPinError("unsafe_archive_url") from error
    _require(re.fullmatch(r"[0-9a-f]{40}", parsed.fragment)
             and parsed.netloc in {"registry.yarnpkg.com", "registry.npmjs.org"}
             and resolved == f"https://{parsed.netloc}/{name}/-/{name.rsplit('/', 1)[-1]}-{version}.tgz#{parsed.fragment}",
             "archive_url_identity_mismatch")
    _require(integrity.startswith("sha512-"), "unsupported_archive_integrity")
    try:
        digest = base64.b64decode(integrity[7:], validate=True)
    except (ValueError, binascii.Error) as error:
        raise YarnPinError("invalid_archive_integrity") from error
    _require(len(digest) == 64 and base64.b64encode(digest).decode("ascii") == integrity[7:], "invalid_archive_integrity")
    return {"name": name, "version": version, "resolved": resolved, "url_path": parsed.path,
            "url_fragment_sha1": parsed.fragment, "integrity": integrity,
            "archive_filename": hashlib.sha256(resolved.encode("ascii")).hexdigest() + ".tgz"}


def parse_yarn_lock(lock_bytes: bytes, manifest_bytes: bytes) -> dict:
    """Return a complete pinned Yarn-v1 graph bound to its authored root manifest.

    Args:
        lock_bytes: Original bounded ASCII Yarn-v1 bytes, with LF or CRLF lines.
        manifest_bytes: Original bounded UTF-8 JSON package manifest bytes.

    Returns:
        Source-ordered stanzas, exact raw selector/field/dependency tokens and byte
        spans, all manifest/dependency edges, complete iterative reachability and
        unique archive identities usable by the existing pure _verify_archive.
        Authored URLs retain the npm or Yarn registry and their SHA-1 fragments; the
        SHA-512 SRI, not a host rewrite or weaker fragment, authenticates archives.
        Local link:. placeholder stanzas are bound to the root manifest resolution,
        never presented as registry archives or evaluated package versions.

    Raises:
        YarnPinError: Unsupported fields/protocols/aliases, malformed tokens,
            incomplete pins, conflicting identities, missing selectors, unreachable
            stanzas, invalid local association or any configured resource limit.
            No files, dependencies, processes or network resources are accessed.

    Graph closure includes optional/development inputs without consumer selection,
    semver evaluation, install-script execution or build/preferred-source claims.
    """
    manifest, stanzas = _manifest(manifest_bytes), _read_lock(lock_bytes)
    selectors, pins, local, urls = {}, {}, [], {}
    for stanza in stanzas:
        names = {_selector(token["value"])[0] for token in stanza["selectors"]}
        _require(len(names) == 1, "mixed_stanza_package_names")
        name = names.pop()
        fields = stanza["fields"]
        _require("version" in fields, "missing_stanza_version")
        version = _version(fields["version"]["value"])
        for token in stanza["selectors"]:
            _require(token["value"] not in selectors, "duplicate_selector")
            selectors[token["value"]] = stanza["index"]
        if "uid" in fields:
            _require(set(fields) == {"version", "uid"} and fields["uid"]["value"] == ""
                     and version == "0.0.0" and name == manifest["name"]
                     and manifest.get("resolutions", {}).get(name) == "link:.", "invalid_local_association")
            stanza["distribution_kind"] = "local_manifest_resolution"
            local.append({"stanza_index": stanza["index"], "name": name, "placeholder_version": version,
                          "manifest_version": manifest["version"], "resolution": "link:."})
        else:
            _require(all(_selector(token["value"])[1] != "link:." for token in stanza["selectors"]), "unbound_link_selector")
            pin = _pin(name, version, fields)
            key = (name, version)
            _require(key not in pins or pins[key] == pin, "conflicting_archive_identity")
            _require(pin["resolved"] not in urls or urls[pin["resolved"]] == key, "conflicting_archive_url")
            pins[key], urls[pin["resolved"]] = pin, key
            stanza["distribution_kind"] = "registry_archive"
    roots = []
    for section in ("dependencies", "devDependencies", "optionalDependencies"):
        values = manifest.get(section, {})
        _require(isinstance(values, dict), "invalid_manifest_dependencies")
        for name, request in values.items():
            selector = _name(name) + "@" + _request(request)
            roots.append({"section": section, "name": name, "request": request, "selector": selector})
    for name, target in manifest.get("resolutions", {}).items():
        roots.append({"section": "resolutions", "name": name, "request": target, "selector": name + "@" + target})
    _require(len(roots) <= MAX_EDGES, "manifest_edge_limit")
    for stanza in stanzas:
        for edge in stanza["edges"]:
            _require(edge["selector"] in selectors, "missing_dependency_selector")
            edge["target_stanza"] = selectors[edge["selector"]]
    for root in roots:
        _require(root["selector"] in selectors, "missing_manifest_selector")
        root["target_stanza"] = selectors[root["selector"]]
    pending, reached = [root["target_stanza"] for root in roots], set()
    while pending:
        index = pending.pop()
        if index not in reached:
            reached.add(index)
            pending.extend(edge["target_stanza"] for edge in stanzas[index]["edges"])
    _require(len(reached) == len(stanzas), "unreachable_lock_stanza")
    return {"schema": "rootlight.yarn-v1-pin-graph/1", "lockfile_sha256": hashlib.sha256(lock_bytes).hexdigest(),
            "manifest_sha256": hashlib.sha256(manifest_bytes).hexdigest(), "stanzas": stanzas,
            "selector_count": len(selectors), "stanza_count": len(stanzas), "archive_count": len(pins),
            "dependency_edge_count": sum(len(stanza["edges"]) for stanza in stanzas), "roots": roots,
            "reachable_stanza_count": len(reached), "packages": [pins[key] for key in sorted(pins)],
            "local_associations": local, "graph_selectors_complete": True, "archives_verified": False,
            "preferred_source_closure_complete": False, "producer_build_verified": False,
            "consumer_install_policy_verified": False, "provenance_signatures_verified": False}
