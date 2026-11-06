#!/usr/bin/env bash
# Downloads the static Linux `graphersal` binaries into a local cache and prints the cache
# directory, for `docker buildx build --build-context bin=<that directory>` of
# docker/prebuilt/Dockerfile (book: "Running in Docker", section "A knowledge base image").
#
#   docker/prebuilt/fetch-binary.sh <source> [amd64|arm64 ...]      (both architectures by default)
#
# <source>:
#   v<version>     the assets of the GitHub Release v<version> (graphersal-v<version>-linux-*.tar.gz),
#                  checked against the release's SHA256SUMS;
#   run:<run-id>   the artifacts binaries-linux-* of a run of the workflow "CLI binaries"
#                  (.github/workflows/binaries.yml), for a build before its release; needs the
#                  GitHub CLI (`gh`, logged in); checked against the run's SHA256SUMS artifact
#                  (binaries-checksums). Cached under run-<run-id>.
#
# Cache: ${GRAPHERSAL_CACHE:-~/.cache/graphersal}/bin/<v<version>|run-<run-id>>/
#   SHA256SUMS                 the checksums of the archives, as published
#   VERSION                    the binary's version (from the archive name)
#   SOURCE                     where it came from (release URL or run URL and commit)
#   linux-<amd64|arm64>/graphersal          the binary
#   linux-<amd64|arm64>/graphersal.sha256   its SHA-256, recorded after the archive was checked
# A cached binary whose SHA-256 matches is used without a download (builds work offline). A
# mismatch deletes that binary and fails: run the script again to download it anew.
#
# Only the cache directory goes to stdout; progress and errors go to stderr. Exit 0 on success,
# 1 on a failed download or check, 2 on an invalid invocation.
# Environment: GRAPHERSAL_CACHE (cache root), GRAPHERSAL_REPO (default graphersal/graphersal).
set -euo pipefail

repo="${GRAPHERSAL_REPO:-graphersal/graphersal}"
cache_root="${GRAPHERSAL_CACHE:-$HOME/.cache/graphersal}"

usage() {
    echo "usage: $0 <v<version>|run:<run-id>> [amd64|arm64 ...]" >&2
    exit 2
}
fail() {
    echo "fetch-binary: $*" >&2
    exit 1
}
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

[ $# -ge 1 ] || usage
source_arg="$1"
shift
case "$source_arg" in
v[0-9]*) kind=release; key="$source_arg" ;;
run:[0-9]*)
    kind=run
    run_id="${source_arg#run:}"
    case "$run_id" in *[!0-9]*) usage ;; esac
    key="run-$run_id"
    ;;
*) usage ;;
esac
archs=("$@")
[ ${#archs[@]} -gt 0 ] || archs=(amd64 arm64)
for arch in "${archs[@]}"; do
    case "$arch" in amd64 | arm64) ;; *) usage ;; esac
done

dir="$cache_root/bin/$key"

# The platform name of the release archives for a Docker architecture.
platform_of() {
    case "$1" in
    amd64) echo linux-x86_64 ;;
    arm64) echo linux-aarch64 ;;
    esac
}

# A cached binary: kept when its recorded checksum matches, deleted (and a failure) otherwise.
cached_ok() {
    local bin="$dir/linux-$1/graphersal"
    [ -f "$bin" ] && [ -f "$bin.sha256" ] || return 1
    if [ "$(sha256_of "$bin")" = "$(cat "$bin.sha256")" ]; then
        return 0
    fi
    rm -f "$bin" "$bin.sha256"
    fail "the cached $bin does not match its recorded checksum: deleted; run the script again to download it anew"
}

missing=()
for arch in "${archs[@]}"; do
    if cached_ok "$arch"; then
        echo "fetch-binary: linux-$arch cached ($dir/linux-$arch/graphersal)" >&2
    else
        missing+=("$arch")
    fi
done

if [ ${#missing[@]} -gt 0 ]; then
    work="$(mktemp -d "${TMPDIR:-/tmp}/graphersal-fetch.XXXXXX")"
    trap 'rm -rf "$work"' EXIT
    # The checksums of the archives.
    if [ "$kind" = release ]; then
        base="https://github.com/$repo/releases/download/$key"
        curl -fsSL --retry 3 -o "$work/SHA256SUMS" "$base/SHA256SUMS" \
            || fail "cannot download $base/SHA256SUMS (is $key a release of $repo?)"
        echo "$base" >"$work/SOURCE"
    else
        command -v gh >/dev/null 2>&1 || fail "run:<id> needs the GitHub CLI (gh)"
        gh run download "$run_id" -R "$repo" -n binaries-checksums -D "$work/sums" >&2 \
            || fail "cannot download the artifact binaries-checksums of run $run_id (finished and successful?)"
        mv "$work/sums/SHA256SUMS" "$work/SHA256SUMS"
        commit="$(gh run view "$run_id" -R "$repo" --json headSha -q .headSha 2>/dev/null || true)"
        echo "https://github.com/$repo/actions/runs/$run_id commit ${commit:-unknown}" >"$work/SOURCE"
    fi
    for arch in "${missing[@]}"; do
        platform="$(platform_of "$arch")"
        archive="$(awk -v p="-$platform.tar.gz" '{ n = $2; sub(/^\*/, "", n) } substr(n, length(n) - length(p) + 1) == p { print n }' "$work/SHA256SUMS")"
        [ -n "$archive" ] || fail "SHA256SUMS lists no archive for $platform"
        expected="$(awk -v n="$archive" '{ f = $2; sub(/^\*/, "", f) } f == n { print $1 }' "$work/SHA256SUMS")"
        echo "fetch-binary: downloading $archive" >&2
        if [ "$kind" = release ]; then
            curl -fsSL --retry 3 -o "$work/$archive" "$base/$archive" \
                || fail "cannot download $base/$archive"
        else
            gh run download "$run_id" -R "$repo" -n "binaries-$platform" -D "$work/art-$arch" >&2 \
                || fail "cannot download the artifact binaries-$platform of run $run_id"
            mv "$work/art-$arch/$archive" "$work/$archive" \
                || fail "the artifact binaries-$platform holds no $archive"
        fi
        actual="$(sha256_of "$work/$archive")"
        [ "$actual" = "$expected" ] \
            || fail "$archive: SHA-256 $actual does not match SHA256SUMS ($expected)"
        mkdir -p "$work/x-$arch"
        tar -xzf "$work/$archive" -C "$work/x-$arch"
        name="${archive%.tar.gz}"
        [ -f "$work/x-$arch/$name/graphersal" ] || fail "$archive holds no $name/graphersal"
        mkdir -p "$dir/linux-$arch"
        install -m 0755 "$work/x-$arch/$name/graphersal" "$dir/linux-$arch/graphersal"
        sha256_of "$dir/linux-$arch/graphersal" >"$dir/linux-$arch/graphersal.sha256"
        version="${name#graphersal-v}"
        version="${version%-"$platform"}"
        echo "$version" >"$dir/VERSION"
        echo "fetch-binary: linux-$arch $version checked and cached" >&2
    done
    cp "$work/SHA256SUMS" "$dir/SHA256SUMS"
    cp "$work/SOURCE" "$dir/SOURCE"
fi

echo "$dir"
