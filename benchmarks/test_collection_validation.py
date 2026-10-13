"""Regression coverage uses real Git trees, including the Generation52 shape."""

from pathlib import Path
import subprocess
import tempfile
import unittest

from collection_validation import OriginalTestsChanged, verify_original_tests


class CollectionValidationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Collection fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        (self.root / "tests/test_igel").mkdir(parents=True)
        (self.root / "tests/test_igel/test_igel.py").write_text("assert True\n")
        (self.root / "tests/test.sh").write_text("exit 0\n")
        self.commit()
        self.baseline = self.git("rev-parse", "HEAD").strip()

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, text=True, stderr=subprocess.PIPE)

    def commit(self):
        self.git("add", "-A")
        self.git("commit", "-qm", "fixture")

    def check(self):
        verify_original_tests(self.root, self.baseline)

    def test_added_feature_schema_test_is_allowed(self):
        (self.root / "tests/test_igel/test_feature_schema.py").write_text("assert 1 == 1\n")
        self.commit()
        self.check()

    def test_unchanged_tests_are_allowed(self):
        self.check()

    def test_original_modified_even_with_added_test_is_rejected(self):
        (self.root / "tests/test_igel/test_feature_schema.py").write_text("assert True\n")
        (self.root / "tests/test_igel/test_igel.py").write_text("assert False\n")
        self.commit()
        with self.assertRaisesRegex(OriginalTestsChanged, "test_igel.py"):
            self.check()

    def test_original_deletion_is_rejected(self):
        (self.root / "tests/test_igel/test_igel.py").unlink()
        self.commit()
        with self.assertRaises(OriginalTestsChanged):
            self.check()

    def test_original_rename_is_rejected(self):
        self.git("mv", "tests/test_igel/test_igel.py", "tests/test_igel/renamed.py")
        self.commit()
        with self.assertRaises(OriginalTestsChanged):
            self.check()

    def test_original_mode_change_is_rejected(self):
        self.git("update-index", "--chmod=+x", "tests/test.sh")
        self.git("commit", "-qm", "mode")
        with self.assertRaises(OriginalTestsChanged):
            self.check()

    def test_original_symlink_replacement_is_rejected(self):
        p = self.root / "tests/test.sh"
        p.unlink()
        p.symlink_to("test_igel/test_igel.py")
        self.commit()
        with self.assertRaises(OriginalTestsChanged):
            self.check()

    def test_invalid_baseline_fails_closed(self):
        with self.assertRaises(subprocess.CalledProcessError):
            verify_original_tests(self.root, "missing-baseline")

    def test_tab_and_newline_paths_are_not_split(self):
        p = self.root / "tests/tab\tnewline\n.py"
        p.write_text("assert True\n")
        self.commit()
        self.baseline = self.git("rev-parse", "HEAD").strip()
        p.write_text("assert False\n")
        self.commit()
        with self.assertRaises(OriginalTestsChanged):
            self.check()


if __name__ == "__main__":
    unittest.main()
