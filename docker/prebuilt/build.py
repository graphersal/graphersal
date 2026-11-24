#!/usr/bin/env python3
"""Builds a self-contained Graphersal knowledge base image: one command, nothing compiled.

The image holds a prebuilt static Linux `graphersal` binary, a compact copy of one Store and the
MCP configuration (with its Markdown instructions), and serves them READ-ONLY over HTTP: the
playground at `/`, MCP at `/mcp`. It needs no volume and writes nothing. See README.md.

    python build.py                                  # everything from this folder (see README)
    python build.py --store kb.gstore --binary graphersal-v0.7.1-linux-x86_64.tar.gz -t my-kb:1
    python build.py --store kb.gstore --version 0.7.1 -t registry.example.com/my-kb:1 --push

Requires Python 3.9+ (standard library only) and Docker with buildx. Works on Linux, macOS and
Windows. Exit codes: 0 ok, 1 a failed step, 2 an invalid invocation.
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
CONTEXT = HERE / ".context"
CONTEXT_MARKER = ".graphersal-build-context"
DEFAULT_REPO = "graphersal/graphersal"
# Docker architecture -> platform suffix of the release archives.
RELEASE_PLATFORM = {"amd64": "x86_64", "arm64": "aarch64"}
# ELF e_machine -> Docker architecture.
ELF_MACHINE = {0x3E: "amd64", 0xB7: "arm64"}
ARCHIVE_RE = re.compile(r"^graphersal-v(?P<version>.+)-linux-(?P<platform>x86_64|aarch64)\.tar\.gz$")


class BuildError(Exception):
    """A failed step: printed without a traceback, exit code 1."""


def log(message: str) -> None:
    print(f"build: {message}", file=sys.stderr, flush=True)


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_sums(path: Path) -> dict[str, str]:
    """`SHA256SUMS` lines `<hash>  <name>` (or `<hash> *<name>`) as {name: hash}."""
    sums = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        parts = line.split(None, 1)
        if len(parts) == 2:
            sums[parts[1].strip().lstrip("*")] = parts[0].lower()
    return sums


# --------------------------------------------------------------------------------------------
# Binaries: arch -> Binary
# --------------------------------------------------------------------------------------------


class Binary:
    def __init__(self, arch: str, path: Path, version: str | None, source: str):
        self.arch = arch
        self.path = path
        self.version = version
        self.source = source


def elf_arch(path: Path) -> str | None:
    """The Docker architecture of a 64-bit little-endian ELF file, None for anything else."""
    try:
        with path.open("rb") as f:
            head = f.read(20)
    except OSError:
        return None
    if len(head) < 20 or head[:4] != b"\x7fELF" or head[4] != 2 or head[5] != 1:
        return None
    return ELF_MACHINE.get(int.from_bytes(head[18:20], "little"))


def from_archive(archive: Path, work: Path, sums: dict[str, str] | None, source: str) -> Binary:
    """The binary inside a release archive `graphersal-v<version>-linux-<platform>.tar.gz`,
    checked against `sums` (or a `SHA256SUMS` next to the archive) when there is one."""
    match = ARCHIVE_RE.match(archive.name)
    if not match:
        raise BuildError(f"{archive}: not a release archive graphersal-v<version>-linux-<x86_64|aarch64>.tar.gz")
    if sums is None and (archive.parent / "SHA256SUMS").is_file():
        sums = read_sums(archive.parent / "SHA256SUMS")
    if sums is not None:
        expected = sums.get(archive.name)
        if expected is None:
            raise BuildError(f"SHA256SUMS lists no {archive.name}")
        actual = sha256_of(archive)
        if actual != expected:
            raise BuildError(f"{archive}: SHA-256 {actual} does not match SHA256SUMS ({expected})")
    else:
        log(f"{archive.name}: no SHA256SUMS next to it, checksum not checked")
    member = f"{archive.name[: -len('.tar.gz')]}/graphersal"
    arch = {v: k for k, v in RELEASE_PLATFORM.items()}[match["platform"]]
    target = work / f"linux-{arch}" / "graphersal"
    target.parent.mkdir(parents=True, exist_ok=True)
    try:
        with tarfile.open(archive, "r:gz") as tar:
            src = tar.extractfile(member)
            if src is None:
                raise KeyError(member)
            with src, target.open("wb") as dst:
                shutil.copyfileobj(src, dst)
    except (KeyError, tarfile.TarError, OSError) as err:
        raise BuildError(f"{archive}: cannot extract {member} ({err})") from err
    return Binary(arch, target, match["version"], source)


def from_path(path: Path, work: Path) -> list[Binary]:
    """Binaries from a `--binary` argument: a Linux binary, a release archive, or a directory
    holding `linux-<arch>/graphersal`, release archives or Linux binaries."""
    if path.is_file():
        if path.name.endswith(".tar.gz"):
            return [from_archive(path, work, None, f"archive {path.name}")]
        arch = elf_arch(path)
        if arch is None:
            raise BuildError(f"{path}: not a Linux x86_64/aarch64 binary or release archive")
        return [Binary(arch, path, None, f"file {path.name} sha256 {sha256_of(path)}")]
    if not path.is_dir():
        raise BuildError(f"no binary at {path}")
    found = []
    for arch in RELEASE_PLATFORM:
        candidate = path / f"linux-{arch}" / "graphersal"
        if candidate.is_file():
            found += from_path(candidate, work)
    for entry in sorted(path.iterdir()):
        if entry.is_file() and ARCHIVE_RE.match(entry.name):
            found.append(from_archive(entry, work, None, f"archive {entry.name}"))
        elif entry.is_file() and entry.name.startswith("graphersal") and elf_arch(entry):
            found += from_path(entry, work)
    return found


def fetch(url: str, target: Path) -> None:
    request = urllib.request.Request(url, headers={"User-Agent": "graphersal-prebuilt-build"})
    try:
        with urllib.request.urlopen(request, timeout=120) as response, target.open("wb") as f:
            shutil.copyfileobj(response, f)
    except (urllib.error.URLError, OSError) as err:
        raise BuildError(f"cannot download {url} ({err})") from err


def latest_tag(repo: str) -> str:
    url = f"https://github.com/{repo}/releases/latest"
    request = urllib.request.Request(url, headers={"User-Agent": "graphersal-prebuilt-build"})
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            final = response.geturl()
    except urllib.error.URLError as err:
        raise BuildError(f"cannot resolve the latest release of {repo} ({err})") from err
    tag = final.rstrip("/").rsplit("/", 1)[-1]
    if not tag.startswith("v"):
        raise BuildError(f"{repo} has no latest release ({final})")
    return tag


def download(version: str, archs: list[str], repo: str, work: Path) -> list[Binary]:
    """The binaries of a GitHub Release (`0.7.1`, `v0.7.1`, `latest`) or of a run of the "CLI
    binaries" workflow (`run:<id>`, needs `gh`), kept in a local cache and reused offline."""
    cache_root = Path(os.environ.get("GRAPHERSAL_CACHE") or Path.home() / ".cache" / "graphersal")
    if version.startswith("run:"):
        run_id = version[4:]
        if not run_id.isdigit():
            raise BuildError(f"--version {version}: expected run:<run-id>")
        key = f"run-{run_id}"
    else:
        key = latest_tag(repo) if version == "latest" else "v" + version.lstrip("v")
    cache = cache_root / "bin" / key
    result, missing = [], []
    for arch in archs:
        binary = cache / f"linux-{arch}" / "graphersal"
        recorded = binary.with_name("graphersal.sha256")
        if binary.is_file() and recorded.is_file():
            if sha256_of(binary) != recorded.read_text().strip():
                binary.unlink()
                recorded.unlink()
                raise BuildError(f"the cached {binary} does not match its recorded checksum: deleted; run again to download it anew")
            log(f"linux-{arch}: cached ({binary})")
            source = (cache / "SOURCE").read_text().strip() if (cache / "SOURCE").is_file() else key
            ver = (cache / "VERSION").read_text().strip() if (cache / "VERSION").is_file() else None
            result.append(Binary(arch, binary, ver, source))
        else:
            missing.append(arch)
    if not missing:
        return result

    dl = work / "download"
    dl.mkdir(parents=True, exist_ok=True)
    if key.startswith("run-"):
        if shutil.which("gh") is None:
            raise BuildError("--version run:<id> needs the GitHub CLI (gh), logged in")

        def gh_artifact(name: str, into: Path) -> None:
            if subprocess.run(["gh", "run", "download", run_id, "-R", repo, "-n", name, "-D", str(into)],
                              stdout=sys.stderr).returncode != 0:
                raise BuildError(f"cannot download the artifact {name} of run {run_id}")

        gh_artifact("binaries-checksums", dl)
        commit = subprocess.run(["gh", "run", "view", run_id, "-R", repo, "--json", "headSha", "-q", ".headSha"],
                                capture_output=True, text=True).stdout.strip() or "unknown"
        source = f"https://github.com/{repo}/actions/runs/{run_id} commit {commit}"
    else:
        base = f"https://github.com/{repo}/releases/download/{key}"
        fetch(f"{base}/SHA256SUMS", dl / "SHA256SUMS")
        source = base
    sums = read_sums(dl / "SHA256SUMS")
    for arch in missing:
        suffix = f"-linux-{RELEASE_PLATFORM[arch]}.tar.gz"
        names = [n for n in sums if n.endswith(suffix)]
        if not names:
            raise BuildError(f"SHA256SUMS of {key} lists no archive for linux-{arch}")
        archive = dl / names[0]
        log(f"downloading {names[0]}")
        if key.startswith("run-"):
            gh_artifact(f"binaries-linux-{RELEASE_PLATFORM[arch]}", dl)
            if not archive.is_file():
                raise BuildError(f"the artifact of run {run_id} holds no {names[0]}")
        else:
            fetch(f"{base}/{names[0]}", archive)
        extracted = from_archive(archive, work / "extract", sums, source)
        target = cache / f"linux-{arch}" / "graphersal"
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(extracted.path, target)
        target.with_name("graphersal.sha256").write_text(sha256_of(target) + "\n")
        (cache / "VERSION").write_text(f"{extracted.version}\n")
        (cache / "SOURCE").write_text(f"{source}\n")
        shutil.copyfile(dl / "SHA256SUMS", cache / "SHA256SUMS")
        log(f"linux-{arch}: {extracted.version} checked and cached ({target})")
        result.append(Binary(arch, target, extracted.version, source))
    return result


def collect_binaries(paths: list[Path], work: Path) -> dict[str, Binary]:
    by_arch: dict[str, Binary] = {}
    for path in paths:
        for binary in from_path(path, work):
            known = by_arch.get(binary.arch)
            if known and sha256_of(known.path) != sha256_of(binary.path):
                raise BuildError(f"two different linux-{binary.arch} binaries: {known.source} and {binary.source}")
            by_arch.setdefault(binary.arch, binary)
    return by_arch


# --------------------------------------------------------------------------------------------
# Container engine: Docker (buildx) or Podman
# --------------------------------------------------------------------------------------------


def _output(command: list[str]) -> str | None:
    try:
        out = subprocess.run(command, capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return out.stdout.strip() if out.returncode == 0 else None


def engine_arch(engine: str) -> str | None:
    """The architecture the engine builds on (for Podman on macOS/Windows: its machine's), None
    when the engine does not answer."""
    if engine == "podman":
        arch = _output(["podman", "info", "-f", "{{.Host.Arch}}"])
    else:
        arch = _output(["docker", "version", "-f", "{{.Server.Arch}}"])
    return arch if arch in RELEASE_PLATFORM else None


def pick_engine(wanted: str) -> str:
    """`auto`: Docker when a real Docker with buildx answers, else Podman. A `docker` command that
    is Podman's compatibility shim counts as Podman."""
    if wanted != "auto":
        return wanted
    version = _output(["docker", "--version"]) if shutil.which("docker") else None
    if version and "podman" not in version.lower() and _output(["docker", "buildx", "version"]) is not None:
        return "docker"
    if shutil.which("podman"):
        return "podman"
    return "docker"


def build_commands(engine: str, platforms: list[str], tags: list[str], build_args: list[str],
                   push: bool, extra: list[str]) -> list[list[str]]:
    """The commands that build (and push) the image."""
    platform_list = ",".join(f"linux/{p}" for p in platforms)
    common = ["--platform", platform_list, "-f", str(HERE / "Dockerfile"), *build_args]
    if engine == "docker":
        tag_args = [a for tag in tags for a in ("-t", tag)]
        return [["docker", "buildx", "build", *common, *tag_args,
                 "--push" if push else "--load", *extra, str(CONTEXT)]]
    # Podman: the docker image format keeps HEALTHCHECK (the OCI format drops it).
    common = ["podman", "build", "--format", "docker", *common]
    if not push:
        if len(platforms) > 1:
            raise BuildError("podman keeps one platform per local image: pass --platform with one "
                             "architecture, or --push")
        return [[*common, *[a for tag in tags for a in ("-t", tag)], *extra, str(CONTEXT)]]
    # A pushed image is a manifest list: built under a temporary name, pushed to every tag.
    manifest = podman_manifest_name()
    commands = [[*common, "--manifest", manifest, *extra, str(CONTEXT)], ["fix-variants", manifest]]
    commands += [["podman", "manifest", "push", "--all", "--format", "v2s2", manifest, f"docker://{tag}"]
                 for tag in tags]
    return commands


def podman_manifest_name() -> str:
    return f"localhost/graphersal-kb-build-{os.getpid()}"


def fix_podman_variants(manifest: str) -> None:
    """Older Podman (seen with 4.7 on an arm64 machine) gives the amd64 entry of a manifest list
    the machine's arm64 variant `v8`, so Docker finds no linux/amd64 image in it. Such an entry
    gets `v1`, the baseline x86-64 level the static binary is built for."""
    listing = _output(["podman", "manifest", "inspect", manifest])
    try:
        entries = json.loads(listing or "")["manifests"]
    except (ValueError, KeyError, TypeError) as err:
        raise BuildError(f"cannot read the manifest list {manifest}") from err
    for entry in entries:
        platform_ = entry.get("platform", {})
        if platform_.get("architecture") == "amd64" and platform_.get("variant") not in (None, "", "v1"):
            log(f"podman gave linux/amd64 the variant {platform_['variant']}: set to v1")
            if subprocess.run(["podman", "manifest", "annotate", "--variant", "v1", manifest, entry["digest"]],
                              stdout=subprocess.DEVNULL).returncode != 0:
                raise BuildError("podman manifest annotate failed (see above)")


def host_arch() -> str | None:
    machine = platform.machine().lower()
    return {"x86_64": "amd64", "amd64": "amd64", "arm64": "arm64", "aarch64": "arm64"}.get(machine)


def link_or_copy(src: str, dst: str) -> None:
    try:
        os.link(src, dst)
    except OSError:
        shutil.copy2(src, dst)


def place(src: Path, dst: Path) -> None:
    """Puts a file or a directory into the build context: hard links where possible (no copy of a
    large store), copies otherwise."""
    dst.parent.mkdir(parents=True, exist_ok=True)
    if src.is_dir():
        shutil.copytree(src, dst, copy_function=link_or_copy)
    else:
        link_or_copy(str(src), str(dst))


def reset_context() -> None:
    if CONTEXT.exists():
        if not (CONTEXT / CONTEXT_MARKER).is_file():
            raise BuildError(f"{CONTEXT} exists but was not written by build.py: remove it yourself")
        shutil.rmtree(CONTEXT)
    CONTEXT.mkdir()
    (CONTEXT / CONTEXT_MARKER).write_text("Build context written by build.py; recreated on every run.\n")


# --------------------------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------------------------


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="build.py",
        description="Build a read-only Graphersal knowledge base image (binary + store + MCP config). "
        "Paths not given are looked up next to this script: the one *.gstore, mcp-config.json, "
        "instructions.md, release archives / Linux binaries (also under bin/).",
        epilog="Arguments after `--` go to `docker buildx build` (or `podman build`) unchanged.",
    )
    parser.add_argument("--store", type=Path, help="the Store: a *.gstore file or a store directory")
    parser.add_argument("--config", type=Path, help="the MCP configuration (JSON); default: mcp-config.json if present")
    parser.add_argument("--instructions", type=Path,
                        help="a Markdown file of MCP instructions for agents; default: instructions.md if present "
                        "(a config naming its own instructions file needs no flag)")
    parser.add_argument("--instructions-mode", choices=["append", "replace"], default="append",
                        help="--instructions after the built-in text (append, default) or instead of it")
    source = parser.add_mutually_exclusive_group()
    source.add_argument("--binary", type=Path, action="append", default=[],
                        help="a prebuilt Linux binary, a release archive graphersal-v<version>-linux-<arch>.tar.gz, "
                        "or a directory of them (repeat per architecture)")
    source.add_argument("--version", help="download a GitHub Release instead: 0.7.1, v0.7.1, latest, or run:<id>")
    parser.add_argument("--platform", help="amd64, arm64 or amd64,arm64; default: every binary found with --push, "
                        "else the engine's own")
    parser.add_argument("-t", "--tag", action="append", default=[], help="image name (repeatable); default graphersal-kb:latest")
    parser.add_argument("--push", action="store_true", help="push to the registry instead of keeping the image locally")
    parser.add_argument("--engine", choices=["auto", "docker", "podman"],
                        default=os.environ.get("CONTAINER_ENGINE", "auto"),
                        help="container engine (default auto: Docker with buildx if present, else Podman; "
                        "env CONTAINER_ENGINE)")
    parser.add_argument("--repo", default=os.environ.get("GRAPHERSAL_REPO", DEFAULT_REPO),
                        help=f"GitHub repository of the releases (default {DEFAULT_REPO})")
    parser.add_argument("--dry-run", action="store_true", help="write the build context and print the build command only")
    if "--" in argv:
        cut = argv.index("--")
        args = parser.parse_args(argv[:cut])
        args.docker_args = argv[cut + 1:]
    else:
        args = parser.parse_args(argv)
        args.docker_args = []
    return args


def find_store(args: argparse.Namespace) -> Path:
    if args.store:
        if not args.store.exists():
            raise BuildError(f"no store at {args.store}")
        return args.store.resolve()
    stores = sorted(HERE.glob("*.gstore"))
    if len(stores) != 1:
        found = ", ".join(s.name for s in stores) or "none"
        raise BuildError(f"pass --store: expected exactly one *.gstore next to build.py, found {found}")
    return stores[0]


def stage_config(args: argparse.Namespace, kb: Path) -> str:
    """Writes kb/mcp-config.json and the instructions file; answers a one-line description."""
    config_path = args.config or (HERE / "mcp-config.json" if (HERE / "mcp-config.json").is_file() else None)
    config: dict = {}
    if config_path:
        try:
            config = json.loads(Path(config_path).read_text(encoding="utf-8"))
        except (OSError, ValueError) as err:
            raise BuildError(f"cannot read the MCP configuration {config_path} ({err})") from err
        if not isinstance(config, dict):
            raise BuildError(f"{config_path}: the MCP configuration must be a JSON object")
    instructions = config.get("instructions")
    named = instructions.get("file") if isinstance(instructions, dict) else None
    explicit = args.instructions
    if explicit and instructions is not None:
        raise BuildError(f"{config_path} already sets \"instructions\": drop --instructions or that key")
    if named:
        source = (Path(config_path).resolve().parent / named)
        if not source.is_file():
            raise BuildError(f"{config_path}: instructions file {source} not found")
        instructions["file"] = source.name
    else:
        source = explicit
        if source is None and instructions is None and (HERE / "instructions.md").is_file():
            source = HERE / "instructions.md"
        if source is not None:
            if not source.is_file():
                raise BuildError(f"no instructions file {source}")
            config["instructions"] = {"mode": args.instructions_mode, "file": source.name}
    if source is not None:
        if source.name == "mcp-config.json":
            raise BuildError("the instructions file cannot be named mcp-config.json")
        shutil.copyfile(source, kb / source.name)
    (kb / "mcp-config.json").write_text(json.dumps(config, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    what = f"config {config_path}" if config_path else "default config (every reading tool)"
    return what + (f", instructions {source}" if source is not None else "")


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    store = find_store(args)
    if args.engine not in ("auto", "docker", "podman"):
        raise BuildError(f"--engine / CONTAINER_ENGINE {args.engine}: expected auto, docker or podman")
    engine = pick_engine(args.engine)
    arch = engine_arch(engine)
    if arch is None and not args.dry_run:
        hint = "podman machine start" if engine == "podman" else "start Docker"
        raise BuildError(f"{engine} does not answer (is it installed and running? {hint})")
    build_arch = arch or host_arch() or "amd64"

    requested = None
    if args.platform:
        requested = [p.strip().removeprefix("linux/") for p in args.platform.split(",") if p.strip()]
        bad = [p for p in requested if p not in RELEASE_PLATFORM]
        if bad or not requested:
            raise BuildError(f"--platform {args.platform}: expected amd64, arm64 or amd64,arm64")

    with tempfile.TemporaryDirectory(prefix="graphersal-build-") as tmp:
        work = Path(tmp)
        if args.version:
            wanted = requested or (list(RELEASE_PLATFORM) if args.push else [build_arch])
            # The store is prepared by the binary of the building engine's architecture.
            fetch_archs = sorted(set(wanted) | {build_arch})
            binaries = {b.arch: b for b in download(args.version, fetch_archs, args.repo, work)}
        else:
            paths = args.binary or [p for p in (HERE, HERE / "bin") if p.is_dir()]
            binaries = collect_binaries(paths, work)
            if not binaries:
                raise BuildError("no graphersal binary: put a release archive graphersal-v<version>-linux-<arch>.tar.gz "
                                 "(or a Linux binary) next to build.py, pass --binary, or download with --version")
        if requested:
            platforms = requested
        elif args.push:
            platforms = sorted(binaries)
        else:
            platforms = [build_arch] if build_arch in binaries else sorted(binaries)[:1]
        missing = [p for p in platforms if p not in binaries]
        if missing:
            raise BuildError(f"no linux-{'/'.join(missing)} binary for --platform (found: {', '.join(sorted(binaries)) or 'none'})")
        versions = {binaries[p].version for p in platforms if binaries[p].version}
        if len(versions) > 1:
            raise BuildError(f"the binaries have different versions: {', '.join(sorted(versions))}")
        # The store is forked and checked by the binary that will serve it, natively when the
        # engine's architecture is among the binaries, under emulation otherwise.
        prep_arch = build_arch if build_arch in binaries else platforms[0]
        if prep_arch != build_arch:
            log(f"no linux-{build_arch} binary: the store is prepared under linux/{prep_arch} emulation (slower)")

        reset_context()
        for arch in sorted(set(platforms) | {prep_arch}):
            place(binaries[arch].path, CONTEXT / "bin" / f"linux-{arch}" / "graphersal")
        place(store, CONTEXT / "store" / "graph.gstore")
        kb = CONTEXT / "kb"
        kb.mkdir()
        described = stage_config(args, kb)

    version = versions.pop() if versions else "unknown"
    sources = "; ".join(f"linux-{p}: {binaries[p].source}" for p in platforms)
    tags = args.tag or ["graphersal-kb:latest"]
    build_args = ["--build-arg", f"PREP_ARCH={prep_arch}",
                  "--build-arg", f"GRAPHERSAL_VERSION={version}",
                  "--build-arg", f"GRAPHERSAL_SOURCE={sources}",
                  "--build-arg", f"BUILD_TIME={datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')}"]
    commands = build_commands(engine, platforms, tags, build_args, args.push, args.docker_args)

    log(f"store {store}")
    log(described)
    log(f"graphersal {version} for {', '.join(platforms)} (store prepared by linux-{prep_arch}), engine {engine}")
    if args.dry_run:
        for command in commands:
            if command[0] == "fix-variants":
                print(f"# then: an amd64 entry with a foreign variant gets variant v1 (podman manifest annotate)")
            else:
                print(subprocess.list2cmdline(command) if os.name == "nt" else shlex.join(command))
        return 0
    try:
        for command in commands:
            if command[0] == "fix-variants":
                fix_podman_variants(command[1])
            elif subprocess.run(command).returncode != 0:
                raise BuildError(f"{' '.join(command[:3])} failed (see above)")
    finally:
        if engine == "podman" and args.push:
            subprocess.run(["podman", "manifest", "rm", podman_manifest_name()], capture_output=True)
    log(f"built {', '.join(tags)}" + (" and pushed" if args.push else ""))
    if not args.push and len(platforms) == 1:
        cost = subprocess.run([engine, "run", "--rm", "--network", "none", "--entrypoint", "/usr/local/bin/graphersal",
                               tags[0], "mcp-cost", "--graph", "/kb/graph.gstore", "--read-only",
                               "--mcp-config", "/kb/mcp-config.json"], capture_output=True, text=True)
        if cost.returncode == 0:
            log("what the MCP prompt costs an agent:")
            for line in cost.stdout.splitlines():
                if line.strip().startswith(("instructions:", "tools:", "total:")):
                    print(f"  {line.strip()}", file=sys.stderr)
        log(f"run:     {engine} run --rm --read-only -p 127.0.0.1:8080:8080 {tags[0]}")
        log("connect: claude mcp add --transport http kb http://localhost:8080/mcp   (playground: http://localhost:8080/)")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except BuildError as err:
        print(f"build: error: {err}", file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:
        sys.exit(130)
