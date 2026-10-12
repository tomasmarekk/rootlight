"""Keep directory identities strict without trusting Windows allocation sizes."""
from pathlib import Path
import stat
import tempfile
from types import SimpleNamespace
import unittest

import install_lightning_from_archives as installer
import verify_lightning_installed_placements as observer
import verify_local_npm_sources as local
import verify_npm_source_bundle as npm


def metadata(*, directory=True, **changes):
    """Model stable OS identity fields while varying one field independently."""
    return SimpleNamespace(**{"st_dev": 1, "st_ino": 2,
                              "st_mode": (stat.S_IFDIR if directory else stat.S_IFREG) | 0o755,
                              "st_size": 0, "st_mtime_ns": 3, "st_ctime_ns": 4,
                              "st_file_attributes": 0, "st_reparse_tag": 0, **changes})


class DirectoryConservationTests(unittest.TestCase):
    """Check allocation drift, meaningful identity drift, and exact child bytes."""

    def equivalent(self, first, second):
        return (npm._same_path_stamp(first, second), observer._stamp(first) == observer._stamp(second),
                installer._stamp(SimpleNamespace(stat=lambda: first)) ==
                installer._stamp(SimpleNamespace(stat=lambda: second)))

    def test_directory_allocation_size_does_not_describe_source_content(self):
        self.assertEqual(self.equivalent(metadata(), metadata(st_size=4096)), (True, True, True))

    def test_file_size_and_directory_identity_mode_and_mtime_remain_strict(self):
        self.assertEqual(self.equivalent(metadata(directory=False), metadata(directory=False, st_size=4096)),
                         (False, False, False))
        for field, value in (("st_dev", 9), ("st_ino", 9), ("st_mtime_ns", 9),
                             ("st_mode", stat.S_IFDIR | 0o700)):
            with self.subTest(field=field):
                self.assertEqual(self.equivalent(metadata(), metadata(**{field: value})), (False, False, False))

    def test_added_child_and_modified_file_payload_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "inputs").mkdir()
            source = root / "inputs/payload"
            source.write_bytes(b"reviewed")
            inputs = local._Inputs(root)
            inputs.directory("inputs", {"payload"})
            inputs.read("inputs/payload")
            inputs.unchanged()
            (root / "inputs/foreign").write_bytes(b"foreign")
            with self.assertRaises(ValueError):
                inputs.unchanged()
            (root / "inputs/foreign").unlink()
            renewed = local._Inputs(root)
            renewed.directory("inputs", {"payload"})
            renewed.read("inputs/payload")
            source.write_bytes(b"modified")
            with self.assertRaises(ValueError):
                renewed.unchanged()


if __name__ == "__main__":
    unittest.main()
