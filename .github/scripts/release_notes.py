#!/usr/bin/env python3
"""Turns a version's release notes of the book into the text of its GitHub Release.

Usage: release_notes.py <version> [<book src dir>] > notes.md

Reads `<book src>/release_notes/release_notes_<version>.md` (default book src: `book/src`) and
writes it to stdout with
- every relative book link made absolute on https://graphersal.dev (`../dates/index.md#x` ->
  `https://graphersal.dev/dates/index.html#x`, a same-page `#x` -> the notes page's own URL),
  inline links, images and reference definitions alike; absolute URLs and `mailto:` stay; fenced
  code blocks are left alone;
- the book's hard-wrapped lines joined into one line per paragraph or list item (GitHub renders
  each newline of a release text as a line break);
- the leading `# ...` title removed (the GitHub Release has its own title);
- a section about the prebuilt binaries inserted before the first `## ` heading.
Used by .github/workflows/release.yml; run it locally to preview a release text.
"""

import posixpath
import re
import sys

SITE = "https://graphersal.dev/"
NOTES_DIR = "release_notes"

BINARIES = (
    "## Prebuilt binaries\n\n"
    "The `graphersal` command line tool is attached to this release for Linux (x86_64, aarch64), "
    "macOS (Apple silicon) and Windows (x86_64), with a `SHA256SUMS` file and build provenance "
    "attestations (`gh attestation verify <archive> --repo graphersal/graphersal`); "
    "`cargo binstall graphersal-cli` installs it too. The Linux binaries are static musl builds, "
    "which run on every distribution but use musl's slower memory allocator: build from source "
    "for benchmarks ([Performance of the prebuilt Linux binaries]"
    "(https://graphersal.dev/getting_started/installation.html"
    "#performance-of-the-prebuilt-linux-binaries)).\n\n"
)

# `](target)` of an inline link or image, with an optional title: `](target "title")`.
INLINE = re.compile(r"\]\(\s*(<[^>]*>|[^\s)]+)(\s+\"[^\"]*\")?\s*\)")
# A reference definition: `[label]: target`.
REFERENCE = re.compile(r"^(\s{0,3}\[[^\]]+\]:\s*)(\S+)(.*)$")
EXTERNAL = re.compile(r"^[a-zA-Z][a-zA-Z0-9+.-]*:")
FENCE = re.compile(r"(`{3,}|~{3,})")
# A list item, heading, table row, quote, HTML block or thematic break.
BLOCK = re.compile(r"([-*+]\s|\d+[.)]\s|#{1,6}(\s|$)|\||>|<|(-{3,}|\*{3,}|_{3,})\s*$)")


def absolute(target, page):
    """The absolute site URL of `target`, a link written on the book page `page` (a .md path);
    None for a link that is already absolute."""
    if target.startswith("<") and target.endswith(">"):
        target = target[1:-1]
    if EXTERNAL.match(target) or target.startswith("//"):
        return None
    path, hash_, fragment = target.partition("#")
    if not path:
        path = page
    elif path.startswith("/"):
        path = path.lstrip("/")
    else:
        path = posixpath.normpath(posixpath.join(posixpath.dirname(page), path))
        if path.startswith(".."):
            raise SystemExit(f"error: the link {target!r} leaves the book")
    if path.endswith(".md"):
        path = path[: -len(".md")] + ".html"
    return SITE + path + hash_ + fragment


def rewrite_line(line, page):
    reference = REFERENCE.match(line)
    if reference:
        url = absolute(reference.group(2), page)
        return line if url is None else reference.group(1) + url + reference.group(3)

    def inline(match):
        url = absolute(match.group(1), page)
        if url is None:
            return match.group(0)
        return "](" + url + (match.group(2) or "") + ")"

    return INLINE.sub(inline, line)


def starts_block(stripped):
    """Whether a line starts a block of its own (never joined to the line before it)."""
    return bool(BLOCK.match(stripped))


def convert(text, page):
    out = []
    fence = None
    joinable = False  # the last output line is paragraph or list text a soft break may continue
    for line in text.splitlines():
        stripped = line.lstrip()
        marker = FENCE.match(stripped)
        if fence is None and marker:
            fence = marker.group(1)
            out.append(line)
            joinable = False
        elif fence is not None:
            if stripped.startswith(fence):
                fence = None
            out.append(line)
        elif not stripped:
            out.append(line)
            joinable = False
        elif joinable and not starts_block(stripped) and not out[-1].endswith("  "):
            # GitHub renders every newline of a release text as a line break: join the book's
            # hard-wrapped lines back into one line per paragraph or list item.
            out[-1] = out[-1].rstrip() + " " + rewrite_line(stripped, page)
        else:
            out.append(rewrite_line(line, page))
            joinable = not (stripped.startswith("#") or stripped.startswith("|"))
    # Drop the leading `# Title` and the blank lines around it.
    while out and not out[0].strip():
        out.pop(0)
    if out and out[0].startswith("# "):
        out.pop(0)
        while out and not out[0].strip():
            out.pop(0)
    body = "\n".join(out) + "\n"
    first = re.search(r"^## ", body, flags=re.MULTILINE)
    if first:
        return body[: first.start()] + BINARIES + body[first.start() :]
    return body + "\n" + BINARIES


def main():
    if len(sys.argv) not in (2, 3):
        raise SystemExit(__doc__)
    version = sys.argv[1]
    src = sys.argv[2] if len(sys.argv) == 3 else "book/src"
    page = f"{NOTES_DIR}/release_notes_{version}.md"
    with open(posixpath.join(src, page), encoding="utf-8") as notes:
        text = notes.read()
    sys.stdout.write(convert(text, page))


if __name__ == "__main__":
    main()
