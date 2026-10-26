#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
temporary="$(mktemp -d)"
container=''
cleanup() {
    if [[ -n "$container" ]]; then docker rm -f "$container" >/dev/null; fi
    rm -rf -- "$temporary"
}
trap cleanup EXIT

# NUL delimiters preserve spaces, Cyrillic and Git's normally quoted filenames.
# Include local untracked sources for act --bind, and skip locally deleted ones.
git -c safe.directory="$root" ls-files --cached --others --exclude-standard -z -- '*.drawio' > "$temporary/sources"
: > "$temporary/images"
mkdir -p -- diagrams
while IFS= read -r -d '' source; do
    [[ -f "$source" ]] || continue
    printf 'Processing %s\n' "$source"
    # Each source has its own output folder, including its repository path.
    # drawio-export otherwise flattens paths and overwrites equal basenames.
    destination="diagrams/${source%.drawio}"
    container="$(docker create "${DRAWIO_IMAGE:-rlespinasse/drawio-export:latest}" \
        -f png --remove-page-suffix -o /output "/data/${source##*/}")"
    # docker cp also works inside act: no host-specific bind-mount paths.
    docker cp "$root/$source" "$container:/data/${source##*/}"
    docker start -a "$container"
    mkdir -p -- "$temporary/export"
    docker cp "$container:/output/." "$temporary/export/"
    docker rm "$container" >/dev/null
    container=''
    shopt -s nullglob
    images=("$temporary/export/"*.png)
    if (( ${#images[@]} == 0 )); then
        printf 'No PNG files generated for %s\n' "$source" >&2
        exit 1
    fi
    mkdir -p -- "$destination"
    for png in "${images[@]}"; do
        target="$destination/${png##*/}"
        cp -- "$png" "$target"
        printf '%s\0%s\0' "$source" "$target" >> "$temporary/images"
    done
    rm -rf -- "$temporary/export"
done < "$temporary/sources"
python3 "$root/scripts/update-diagrams-readme.py" "$temporary/images"
