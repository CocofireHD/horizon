#!/usr/bin/env python3
"""Whether the nightly Crowdin pull request may merge itself (docs/release-flow.md, "Merging").

`.github/workflows/crowdin.yml` imports Crowdin's export, commits it to `l10n/crowdin` and, when the
repository sets `CROWDIN_AUTOMERGE`, asks GitHub to merge that pull request once its checks pass. This
script is the part that decides whether it may: two conditions, both about the whole diff and not the
push that happened to be last.

*Every changed path is a language file or the review record.* Nothing else belongs on that branch, so
`en_us.json` (the source) may never be among them, and neither may a workflow, a tool or a manifest. A
later push that adds one takes the pull request off the automatic path without anyone noticing, which
is why the same check runs as `l10n (changed paths)` on the pull request itself.

*Every changed string is one a proofreader approved.* With `--approved`, the export this run downloaded
with `export_only_approved` has to carry the text for every changed key: the record in the repository is
not proof on its own, because a person may have marked a string by hand and a proofreader may have
withdrawn one. A string the export does not carry holds the pull request for a Team member, as
everything did before.

    python3 tools/l10n_merge_guard.py --base origin/main --approved l10n/game-approved
    python3 tools/l10n_merge_guard.py --base origin/main --paths-only   # what CI requires
    python3 tools/l10n_merge_guard.py --patch l10n.patch --paths-only   # a patch, before it is applied

`--paths-only` is the required check's half: the paths, not the approvals. Without it a language with
unproofread strings would never merge; with it the branch stays what it claims to be. `--root` names the
checkout to read, for the case that matters most: the required check runs *main's* copy of this file
against a branch that may have rewritten it, so the file it executes is not the one beside it. `--head`
names the revision to judge when it is not the checkout's. `--patch` reads the paths out of a patch
instead, the way the job that pushes checks what it received from the job that downloaded it.

Exit code 0 means the pull request may merge itself, 1 means it waits for a person, 2 means the script
could not tell.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parent.parent
LANG = "crates/horizon-client/assets/horizon/lang"
REVIEW = "tools/lang_review/game"
CROWDIN = "tools/lang_review/crowdin"
REFERENCE = "en_us.json"


def shown(path: str) -> str:
    return path


def git(*arguments: str) -> str:
    """One git call's stdout, or a failure that names it."""
    result = subprocess.run(["git", *arguments], cwd=ROOT, capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(f"git {' '.join(arguments)}: {result.stderr.strip() or result.stdout.strip()}")
    return result.stdout


def changed(base: str, head: str = "HEAD") -> list[tuple[str, str]]:
    """`(status, path)` for every path this range touches, renames spelled as what they are."""
    body = git("diff", "--name-status", "--no-renames", base, head)
    out = []
    for line in body.splitlines():
        if not line.strip():
            continue
        parts = line.split("\t")
        out.append((parts[0][:1], parts[-1]))
    return out


def in_patch(text: str) -> list[tuple[str, str]]:
    """`(status, path)` for every path a `git diff --binary --no-renames` patch touches.

    The patch is read, never applied here: it arrives as an artifact, and what it would write is the
    question. The caller writes it only after this answers. `--no-renames` on the side that wrote the
    patch is what makes the two halves of a `diff --git` line the same path.
    """
    files: list[list[str]] = []
    for line in text.splitlines():
        if line.startswith("diff --git a/"):
            left, _, right = line[len("diff --git a/"):].rpartition(" b/")
            if not left or left != right:
                raise RuntimeError(f"cannot read the patch's file line: {line}")
            files.append([" ", right])
        elif files and line.startswith("new file mode"):
            files[-1][0] = "A"
        elif files and line.startswith("deleted file mode"):
            files[-1][0] = "D"
    return [(status, path) for status, path in files]


def allowed(path: str) -> bool:
    """A language's text or its record, in the two folders the import writes; the source never."""
    pure = PurePosixPath(path)
    if pure.name == REFERENCE:
        return False
    if str(pure.parent) == LANG and pure.suffix == ".json":
        return True
    return pure.suffix == ".json" and str(pure.parent) in (REVIEW, CROWDIN)


def entries(text: str, path: str) -> dict[str, str]:
    """One flat map of key to text, as `tools/lang_coverage.py` reads it, or a failure that names it."""
    try:
        data = json.loads(text)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"{shown(path)}: not JSON: {error}") from None
    if not isinstance(data, dict) or not all(
            isinstance(key, str) and isinstance(value, str) for key, value in data.items()):
        raise RuntimeError(f"{shown(path)}: not a flat map of key to text")
    return data


def at_revision(revision: str, path: str) -> dict[str, str]:
    """The file's text in `revision`, or nothing when that revision does not have it."""
    result = subprocess.run(["git", "show", f"{revision}:{path}"], cwd=ROOT, capture_output=True, text=True)
    if result.returncode != 0:
        return {}
    return entries(result.stdout, f"{revision}:{path}")


def at_head(path: str, head: str = "HEAD") -> dict[str, str]:
    if head != "HEAD":
        return at_revision(head, path)
    file = ROOT / path
    if not file.exists():
        return {}
    return entries(file.read_text(encoding="utf-8"), path)


def rewrote(base: str, head: str, path: str) -> list[str]:
    """The keys this range gives a new text (an empty value is a gap, so it never counts)."""
    before, after = at_revision(base, path), at_head(path, head)
    return sorted(key for key, value in after.items()
                  if value.strip() and before.get(key) != value)


def unmarked(path: str, keys: list[str], head: str = "HEAD") -> list[str]:
    """The keys among `keys` the record does not carry in their present text."""
    text = at_head(path, head)
    record = at_head(f"{REVIEW}/{PurePosixPath(path).stem}.json", head)
    return [key for key in keys if record.get(key) != text.get(key)]


def unproven(path: str, keys: list[str], approved: str, head: str = "HEAD") -> list[str]:
    """The keys among `keys` this run's proofreader-approved export does not carry in their present text."""
    text = at_head(path, head)
    file = Path(approved) / f"{PurePosixPath(path).stem}.json"
    proof = entries(file.read_text(encoding="utf-8"), str(file)) if file.exists() else {}
    return [key for key in keys if proof.get(key) != text.get(key)]


def check(files: list[tuple[str, str]], base: str, head: str, paths_only: bool,
          approved: str | None) -> list[str]:
    """Every reason the branch may not merge itself, most specific first; empty when there is none."""
    problems: list[str] = []
    if not files:
        problems.append(f"nothing changed: {base}..{head} is empty, so there is nothing to merge")
    for status, path in files:
        if not allowed(path):
            problems.append(f"{shown(path)}: not a language file or a review record")
        elif status == "D":
            continue
        elif PurePosixPath(path).parent == PurePosixPath(LANG) and not paths_only:
            keys = rewrote(base, head, path)
            stale = unmarked(path, keys, head)
            if stale:
                listed = ", ".join(stale[:6]) + (f" (+{len(stale) - 6} more)" if len(stale) > 6 else "")
                noun = "string" if len(stale) == 1 else "strings"
                problems.append(f"{shown(path)}: {len(stale)} {noun} no proofreader approved: {listed}")
            if approved is not None:
                absent = unproven(path, keys, approved, head)
                if absent:
                    listed = ", ".join(absent[:6]) + (f" (+{len(absent) - 6} more)" if len(absent) > 6 else "")
                    noun = "string" if len(absent) == 1 else "strings"
                    problems.append(f"{shown(path)}: {len(absent)} {noun} not in this run's approved "
                                    f"export ({approved}): {listed}")
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base", default="origin/main", help="the revision the branch is measured against")
    parser.add_argument("--head", default="HEAD", help="the revision to judge (default: the checkout's HEAD)")
    parser.add_argument("--paths-only", action="store_true",
                        help="check the changed paths alone, as the required check does")
    parser.add_argument("--approved",
                        help="this run's proofreader-approved export: every changed string must be in it")
    parser.add_argument("--patch",
                        help="read the changed paths from this patch instead of the checkout (with --paths-only)")
    parser.add_argument("--root", help="the checkout to read (default: the one this file sits in)")
    arguments = parser.parse_args(argv)
    if arguments.root:
        global ROOT
        ROOT = Path(arguments.root).resolve()
    if arguments.patch and not arguments.paths_only:
        print("l10n_merge_guard: --patch reads the changed paths alone; pass --paths-only", file=sys.stderr)
        return 2
    try:
        if arguments.patch:
            files = in_patch(Path(arguments.patch).read_text(encoding="utf-8"))
        else:
            files = changed(arguments.base, arguments.head)
        problems = check(files, arguments.base, arguments.head, arguments.paths_only, arguments.approved)
    except (RuntimeError, OSError) as error:
        print(f"l10n_merge_guard: {error}", file=sys.stderr)
        return 2
    if problems:
        for problem in problems:
            print(problem)
        print(f"\n{len(problems)} reason(s) to keep this pull request for a Team member.")
        return 1
    print(f"nothing to hold this pull request back: {len(files)} changed file(s), all language files"
          + ("" if arguments.paths_only else " with only approved strings"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
