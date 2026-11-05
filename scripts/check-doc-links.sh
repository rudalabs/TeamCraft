#!/usr/bin/env bash
# Checks that every local image and link target in the Markdown documentation
# exists: src="…", ![…](…) and […](…) in README.md, CONTRIBUTING.md,
# SECURITY.md, platforms/*.md, desktop/README.md and docs/src/**/*.md.
# Targets with a scheme (http:, https:, mailto:, …), anchors (#…) and
# protocol-relative URLs are skipped; "#anchor" and "?query" suffixes are
# removed. Each target is resolved relative to the file that names it.
# Fenced code blocks and inline code are ignored.
#
#   scripts/check-doc-links.sh            # the default file set
#   scripts/check-doc-links.sh FILE...    # only these Markdown files
#
# Exits 1 and lists every missing target (file:line: target). Used by
# .github/workflows/docs.yml; see CONTRIBUTING.md.
#
# Paths the documentation build creates and that therefore do not exist in
# the tree (the rustdoc reference under docs/src/api/) are allowed below.
set -euo pipefail

cd "$(dirname "$0")/.."

GENERATED_PREFIXES=(
    "docs/src/api/" # rustdoc, copied into site/api/ by docs.yml
)

if [[ $# -gt 0 ]]; then
    files=("$@")
else
    files=(README.md CONTRIBUTING.md SECURITY.md desktop/README.md)
    while IFS= read -r f; do files+=("$f"); done < <(find platforms docs/src -name '*.md' -type f | sort)
fi

# targets FILE – prints "LINE<TAB>TARGET" for every link and image target,
# outside fenced code blocks and inline code.
targets() {
    awk '
        /^[[:space:]]*(```|~~~)/ { fence = !fence; next }
        fence { next }
        {
            line = $0
            gsub(/`[^`]*`/, "", line)
            rest = line
            while (match(rest, /src="[^"]*"/)) {
                print NR "\t" substr(rest, RSTART + 5, RLENGTH - 6)
                rest = substr(rest, RSTART + RLENGTH)
            }
            rest = line
            while (match(rest, /\]\([^)]*\)/)) {
                print NR "\t" substr(rest, RSTART + 2, RLENGTH - 3)
                rest = substr(rest, RSTART + RLENGTH)
            }
        }
    ' "$1"
}

missing=0
checked=0
for file in "${files[@]}"; do
    [[ -f $file ]] || continue
    dir=$(dirname "$file")
    while IFS=$'\t' read -r line target; do
        target=${target#<}
        target=${target%%>*}
        target=${target%% *} # drop a link title: (path "title")
        case $target in
        '' | '#'* | //* | [a-zA-Z]*:*) continue ;;
        esac
        path=${target%%#*}
        path=${path%%\?*}
        path=${path//%20/ }
        [[ -n $path ]] || continue
        if [[ $path == /* ]]; then
            resolved=${path#/}
        else
            resolved=$dir/$path
        fi
        # normalise "a/./b" and "a/x/../b" without touching the file system
        resolved=$(awk -v p="$resolved" 'BEGIN {
            n = split(p, parts, "/"); k = 0
            for (i = 1; i <= n; i++) {
                if (parts[i] == "" || parts[i] == ".") continue
                if (parts[i] == ".." && k > 0 && out[k] != "..") { k--; continue }
                out[++k] = parts[i]
            }
            s = (substr(p, 1, 1) == "/") ? "/" : ""
            for (i = 1; i <= k; i++) s = s (i > 1 ? "/" : "") out[i]
            print s
        }')
        checked=$((checked + 1))
        generated=0
        for prefix in "${GENERATED_PREFIXES[@]}"; do
            [[ $resolved/ == "$prefix"* ]] && generated=1
        done
        if [[ $generated -eq 0 && ! -e $resolved ]]; then
            printf '%s:%s: %s (not found: %s)\n' "$file" "$line" "$target" "$resolved" >&2
            missing=$((missing + 1))
        fi
    done < <(targets "$file")
done

if [[ $missing -gt 0 ]]; then
    printf '%d of %d local link and image targets are missing.\n' "$missing" "$checked" >&2
    exit 1
fi
printf 'all %d local link and image targets exist (%d files).\n' "$checked" "${#files[@]}"
