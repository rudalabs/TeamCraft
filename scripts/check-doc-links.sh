#!/usr/bin/env bash
# Checks that every local image and link target in the Markdown documentation
# exists: src="…" and href="…" (also single-quoted), ![…](…), […](…) and
# reference definitions ([name]: target) in README.md, CONTRIBUTING.md,
# SECURITY.md, platforms/*.md, desktop/README.md and docs/src/**/*.md.
# Targets with a scheme (http:, https:, mailto:, …), anchors (#…) and
# protocol-relative URLs are skipped; "#anchor" and "?query" suffixes and
# link titles are removed, <angle brackets> and %XX escapes are decoded.
# Each target is resolved relative to the file that names it.
# Fenced code blocks (``` or ~~~, closed by a fence of the same kind and at
# least the same length) and inline code are ignored.
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

root=$(cd "$(dirname "$0")/.." && pwd)

GENERATED_PREFIXES=(
    "docs/src/api/" # rustdoc, copied into site/api/ by docs.yml
)

files=()
# FILE arguments are relative to the caller's directory; inside the
# repository they become repository-relative, like the default file set
for arg in "$@"; do
    if abs_dir=$(cd "$(dirname "$arg")" 2>/dev/null && pwd); then
        arg=$abs_dir/$(basename "$arg")
        [[ $arg == "$root"/* ]] && arg=${arg#"$root"/}
    fi
    files+=("$arg")
done
cd "$root"

if [[ ${#files[@]} -eq 0 ]]; then
    files=(README.md CONTRIBUTING.md SECURITY.md desktop/README.md)
    while IFS= read -r f; do files+=("$f"); done < <(find platforms docs/src -name '*.md' -type f | sort)
fi

# targets FILE – prints "LINE<TAB>TARGET" for every link and image target,
# outside fenced code blocks and inline code. Link titles and <angle
# brackets> are already removed. POSIX awk (gawk and mawk).
targets() {
    awk '
        # the destination at the start of s: <…> or up to the first blank,
        # with balanced parentheses inside an inline link (inline = 1)
        function dest(s, inline,    c, i, depth, out) {
            sub(/^[ \t]+/, "", s)
            if (substr(s, 1, 1) == "<") {
                i = index(s, ">")
                return i ? substr(s, 2, i - 2) : ""
            }
            depth = 0; out = ""
            for (i = 1; i <= length(s); i++) {
                c = substr(s, i, 1)
                if (c == " " || c == "\t") break
                if (inline && c == "(") depth++
                if (inline && c == ")") { if (depth == 0) break; depth-- }
                out = out c
            }
            return out
        }
        {
            if (match($0, /^ ? ? ?(```+|~~~+)/)) {
                run = substr($0, RSTART, RLENGTH); sub(/^ +/, "", run)
                if (!fence) { fence = 1; fch = substr(run, 1, 1); flen = length(run); next }
                rest = substr($0, RSTART + RLENGTH)
                if (substr(run, 1, 1) == fch && length(run) >= flen && rest ~ /^[ \t]*$/) fence = 0
                next
            }
            if (fence) next
            line = $0
            gsub(/``[^`]+``/, "", line)
            gsub(/`[^`]*`/, "", line)
            rest = line
            while (match(rest, /(src|href)=("[^"]*"|\047[^\047]*\047)/)) {
                t = substr(rest, RSTART, RLENGTH); sub(/^[a-z]+=./, "", t)
                print NR "\t" substr(t, 1, length(t) - 1)
                rest = substr(rest, RSTART + RLENGTH)
            }
            if (match(line, /^ ? ? ?\[[^]]+\]:/)) {
                print NR "\t" dest(substr(line, RSTART + RLENGTH), 0)
            }
            rest = line
            while ((i = index(rest, "](")) > 0) {
                rest = substr(rest, i + 2)
                print NR "\t" dest(rest, 1)
            }
        }
    ' "$1"
}

missing=0
checked=0
for file in "${files[@]}"; do
    if [[ ! -f $file ]]; then
        printf '%s: file not found\n' "$file" >&2
        missing=$((missing + 1))
        continue
    fi
    dir=$(dirname "$file")
    # a plain assignment, so that an awk error stops the script (set -e)
    # instead of silently yielding no targets
    found=$(targets "$file")
    seen_line=
    while IFS=$'\t' read -r line target; do
        case $target in
        '' | '#'* | //*) continue ;;
        esac
        [[ $target =~ ^[a-zA-Z][a-zA-Z0-9+.-]*: ]] && continue # a scheme
        # <a href="x.png"><img src="x.png"></a> names x.png once per line
        # (a plain string, no associative array: bash 3.2 on macOS)
        [[ $line == "$seen_line" ]] || seen=$'\n'
        seen_line=$line
        [[ $seen != *$'\n'"$target"$'\n'* ]] || continue
        seen+=$target$'\n'
        path=${target%%#*}
        path=${path%%\?*}
        # decode %XX escapes (%20, UTF-8 such as %C3%A4)
        if [[ $path == *%[0-9A-Fa-f][0-9A-Fa-f]* ]]; then
            path=$(printf '%b' "${path//%/\\x}")
        fi
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
    done <<<"$found"
done

if [[ $missing -gt 0 ]]; then
    printf '%d missing (%d local link and image targets checked).\n' "$missing" "$checked" >&2
    exit 1
fi
printf 'all %d local link and image targets exist (%d files).\n' "$checked" "${#files[@]}"
