"""The two conditions under which the nightly translations pull request may queue itself.

`l10n/crowdin` is the one pull request that merges without a person (docs/release-flow.md, "Merging"), so
`tools/l10n_merge_guard.py` reads its whole diff: language files and their record only, and every changed
string one a proofreader approved. These tests drive it over a tiny repository, one condition at a time.
"""
import contextlib
import io
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from tools import l10n_merge_guard

LANG = "crates/horizon-client/assets/horizon/lang"
REVIEW = "tools/lang_review/game"


def run(*arguments: str) -> tuple[int, str, str]:
    """The tool's exit code and its two streams."""
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = l10n_merge_guard.main(list(arguments))
    return code, out.getvalue(), err.getvalue()


class GuardTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "test@example.com")
        self.git("config", "user.name", "test")
        self.write(f"{LANG}/en_us.json", '{\n  "a": "Apple",\n  "b": "Banana"\n}\n')
        self.write(f"{LANG}/xx.json", '{\n  "a": "Manzana",\n  "b": "Platano"\n}\n')
        self.write(f"{REVIEW}/xx.json", '{\n  "a": "Manzana",\n  "b": "Platano"\n}\n')
        self.base = self.commit("base")

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, *arguments: str) -> str:
        result = subprocess.run(["git", *arguments], cwd=self.root, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def write(self, path: str, body: str) -> None:
        file = self.root / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(body, encoding="utf-8")

    def commit(self, message: str) -> str:
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message)
        return self.git("rev-parse", "HEAD").strip()

    def guard(self, *arguments: str) -> tuple[int, str, str]:
        with mock.patch.object(l10n_merge_guard, "ROOT", self.root):
            return run("--base", self.base, *arguments)

    def test_an_approved_string_may_queue_itself(self):
        self.write(f"{LANG}/xx.json", '{\n  "a": "Manzana",\n  "b": "Platano verde"\n}\n')
        self.write(f"{REVIEW}/xx.json", '{\n  "a": "Manzana",\n  "b": "Platano verde"\n}\n')
        self.commit("both move together")
        code, out, _ = self.guard()
        self.assertEqual(code, 0, out)
        self.assertIn("all language files", out)

    def test_a_string_without_a_proofreaders_mark_holds_the_pull_request(self):
        self.write(f"{LANG}/xx.json", '{\n  "a": "Manzana",\n  "b": "Platano verde"\n}\n')
        self.commit("a translator wrote it, nobody proofread it")
        code, out, _ = self.guard()
        self.assertEqual(code, 1)
        self.assertIn("1 string no proofreader approved", out)
        self.assertIn(f"{LANG}/xx.json", out)

    def test_a_mark_for_an_older_text_does_not_cover_the_new_one(self):
        self.write(f"{LANG}/xx.json", '{\n  "a": "Manzana",\n  "b": "Banano"\n}\n')
        self.commit("the text changed, the mark did not follow")
        code, out, _ = self.guard()
        self.assertEqual(code, 1)
        self.assertIn("b", out)

    def test_an_empty_value_is_a_gap_not_a_string(self):
        self.write(f"{LANG}/xx.json", '{\n  "a": "Manzana",\n  "b": ""\n}\n')
        self.commit("a proofreader emptied it")
        code, out, _ = self.guard()
        self.assertEqual(code, 0, out)

    def test_the_english_source_may_never_change_here(self):
        self.write(f"{LANG}/en_us.json", '{\n  "a": "Apple",\n  "b": "Banana",\n  "c": "Cherry"\n}\n')
        self.commit("the source changed, which is not this branch's job")
        code, out, _ = self.guard()
        self.assertEqual(code, 1)
        self.assertIn("en_us.json: not a language file or a review record", out)

    def test_any_other_file_holds_it_too(self):
        self.write("crates/horizon-client/Cargo.toml", "[package]\nname = \"horizon-client\"\n")
        self.commit("a manifest rode along")
        code, out, _ = self.guard()
        self.assertEqual(code, 1)
        self.assertIn("Cargo.toml: not a language file or a review record", out)

    def test_the_paths_alone_are_what_the_required_check_reads(self):
        self.write(f"{LANG}/xx.json", '{\n  "a": "Manzana",\n  "b": "Platano verde"\n}\n')
        self.commit("unproofread, which the required check does not judge")
        self.assertEqual(self.guard("--paths-only")[0], 0)
        self.assertEqual(self.guard()[0], 1)

    def test_a_record_on_its_own_is_allowed(self):
        self.write(f"{REVIEW}/xx.json", '{\n  "a": "Manzana"\n}\n')
        self.commit("a withdrawn approval drops a mark")
        code, out, _ = self.guard()
        self.assertEqual(code, 0, out)

    def test_nothing_changed_is_not_a_pull_request_to_queue(self):
        self.assertEqual(self.guard()[0], 1)
        self.assertIn("nothing changed", self.guard()[1])

    def test_root_names_the_checkout_so_the_required_check_runs_mains_copy(self):
        """The branch may rewrite the guard; the required check runs the file main has, against it."""
        self.write(f"{LANG}/xx.json", '{\n  "a": "Manzana",\n  "b": "Platano verde"\n}\n')
        self.commit("unproofread, which --paths-only does not judge")
        with tempfile.TemporaryDirectory() as elsewhere:
            copy = Path(elsewhere) / "l10n_merge_guard.py"
            copy.write_text(Path(l10n_merge_guard.__file__).read_text(encoding="utf-8"), encoding="utf-8")
            result = subprocess.run([sys.executable, str(copy), "--root", str(self.root),
                                     "--base", self.base, "--paths-only"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("all language files", result.stdout)


if __name__ == "__main__":
    unittest.main()
