#!/bin/sh
# worktree-prune -- remove git worktrees together with the per-worktree cargo
# target dir that cargo-target-provision gave them.
#
# `git worktree remove` deletes the checkout and nothing else. The target dir
# lives outside the repo (under $CARGO_TARGET_BASE_DIR), so it survives as an
# orphan nothing will ever collect -- and those are the big ones: a single
# zyphe-backend worktree target dir routinely passes 100 GB. This script keeps
# the two in step, and refuses to delete anything whose only copy is local.
#
# It also cleans up after git's own failure mode. A worktree that ran the docker
# stack owns a root-created bind-mount directory (docker/.mailpit-data), which
# the user cannot unlink. `git worktree remove` deletes files until it hits it,
# gives up with "Permission denied" -- and has by then already deregistered the
# worktree. The result is a directory git no longer knows about, holding a few
# hundred surviving files and still keyed to a target dir. So: such leftovers
# are found and removed as "stale", and the unremovable paths are detected
# before git is invoked, not after.
#
# Usage:
#   worktree-prune [options] [<worktree>...]
#
#   <worktree> is a name (the directory's basename, as used for the target-dir
#   key) or a path. With no arguments, --list is implied.
#
# Options:
#   -n, --dry-run     print the plan, change nothing (the default)
#   -y, --yes         actually remove
#   -f, --force       proceed despite local modifications, untracked files, or
#                     commits that exist nowhere else
#   -s, --sudo        use sudo for paths owned by another user (docker leftovers)
#   -q, --quiet       one summary line if there is anything to report, else
#                     nothing, and never a non-zero exit -- the mode hooks use
#       --keep-target remove the worktree, leave its target dir alone
#       --delete-branch  also delete the worktree's local branch
#       --list        show every worktree with its status and target-dir size
#       --stale       select every leftover directory git no longer registers
#       --orphans     also sweep target dirs under the base that no live
#                     worktree claims
#   -h, --help
#
# Run it from anywhere inside the repository whose worktrees you are pruning.
#
# Exit status: 1 if anything was refused or failed, 0 otherwise.

set -eu

BASE="${CARGO_TARGET_BASE_DIR:-/.cargo-targets/targets}"
ME=$(id -un)

dry=1; force=0; use_sudo=0; keep_target=0; del_branch=0; do_list=0; do_orphans=0; do_stale=0
quiet=0
selected=""
fails=0

ok()   { [ "$quiet" -eq 1 ] || printf '  ✅ %s\n' "$*"; }
warn() { [ "$quiet" -eq 1 ] || printf '  ⚠️  %s\n' "$*"; }
bad()  { fails=$((fails + 1)); [ "$quiet" -eq 1 ] || printf '  ❌ %s\n' "$*"; }
note() { [ "$quiet" -eq 1 ] || printf '      %s\n' "$*"; }
hdr()  { [ "$quiet" -eq 1 ] || printf '\n── %s ──────────────────────────────────────────\n' "$1"; }
say()  { [ "$quiet" -eq 1 ] || printf '%s\n' "$*"; }
die()  { printf 'worktree-prune: %s\n' "$*" >&2; exit 1; }

usage() { sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'; exit 0; }

while [ $# -gt 0 ]; do
    case $1 in
        -n|--dry-run)    dry=1 ;;
        -y|--yes)        dry=0 ;;
        -f|--force)      force=1 ;;
        -s|--sudo)       use_sudo=1 ;;
        -q|--quiet)      quiet=1 ;;
        --keep-target)   keep_target=1 ;;
        --delete-branch) del_branch=1 ;;
        --list)          do_list=1 ;;
        --stale)         do_stale=1 ;;
        --orphans)       do_orphans=1 ;;
        -h|--help)       usage ;;
        -*)              die "unknown option: $1" ;;
        *)               selected="$selected$1
" ;;
    esac
    shift
done
[ -n "$selected" ] || [ "$do_orphans" -eq 1 ] || [ "$do_stale" -eq 1 ] || do_list=1

git rev-parse --git-dir >/dev/null 2>&1 || die "not inside a git repository"

# Repo identity, keyed exactly as cargo-target-provision keys it: the shared
# .git is the same for every worktree, so the main checkout names the repo.
common_dir=$(git rev-parse --path-format=absolute --git-common-dir)
main_wt=$(dirname "$common_dir")
if [ "$(basename "$common_dir")" = ".git" ]; then
    repo=$(basename "$main_wt")
else
    repo=$(basename "$common_dir" .git)
fi

tmp=$(mktemp -d) || die "cannot create a temporary directory"
trap 'rm -rf "$tmp"' EXIT INT TERM

# ── inventory ────────────────────────────────────────────────────────────────
# One tab-separated record per worktree: path, branch, target dir. Tabs, not
# spaces: worktree paths are user-chosen and may contain spaces.

read_target_dir() {  # $1 = worktree path (registered or not)
    cfg="$1/.cargo/config.toml"
    [ -f "$cfg" ] || cfg="$1/.cargo/config"
    if [ -f "$cfg" ]; then
        td=$(grep -E '^[[:space:]]*target-dir[[:space:]]*=' "$cfg" 2>/dev/null |
             head -1 | sed -E 's/^[^=]*=[[:space:]]*"?([^"]*)"?[[:space:]]*$/\1/')
        if [ -n "$td" ]; then
            # A relative target-dir resolves against the dir holding .cargo/.
            case $td in
                /*) printf '%s\n' "$td" ;;
                *)  printf '%s\n' "$1/$td" ;;
            esac
            return 0
        fi
    fi
    # No config (never provisioned, or a half-deleted leftover): fall back to the
    # key cargo-target-provision would have used -- basename of the worktree.
    if [ "$1" = "$main_wt" ]; then printf '%s/%s/main/target\n' "$BASE" "$repo"
    else printf '%s/%s/%s/target\n' "$BASE" "$repo" "$(basename "$1")"; fi
}

git worktree list --porcelain | awk -v OFS='\t' '
    /^worktree /  { if (wt != "") print wt, br; wt = substr($0, 10); br = "" }
    /^branch /    { br = substr($0, 8); sub("refs/heads/", "", br) }
    /^detached/   { br = "(detached)" }
    END           { if (wt != "") print wt, br }
' > "$tmp/wts"

while IFS='	' read -r wt br; do
    printf '%s\t%s\t%s\n' "$wt" "$br" "$(read_target_dir "$wt")"
done < "$tmp/wts" > "$tmp/inv"

# ── stale leftovers ──────────────────────────────────────────────────────────
# Directories sitting where worktrees live that `git worktree list` does not
# report. Only the directories that actually hold worktrees are scanned -- the
# parents of the registered ones, plus the two conventional locations -- so a
# stray directory elsewhere in the repo is never a candidate.

: > "$tmp/parents"
while IFS='	' read -r wt _ _; do
    [ "$wt" = "$main_wt" ] && continue
    dirname "$wt"
done < "$tmp/inv" | sort -u > "$tmp/parents"
# Plus the conventional locations. These matter for the leftover case
# specifically: once git has deregistered a worktree it keeps no record of where
# it was, so a leftover outside both the parents of the surviving worktrees and
# this list cannot be found by directory at all -- only its orphaned target dir
# will be, which is the part that costs gigabytes.
for p in "$main_wt/.claude/worktrees" "$main_wt/.worktrees" "$main_wt/worktrees"; do
    [ -d "$p" ] && printf '%s\n' "$p"
done >> "$tmp/parents"
sort -u "$tmp/parents" -o "$tmp/parents"

# Being unregistered is not enough to be *ours*: worktrees often share a parent
# directory with unrelated clones, and mistaking one for a leftover would offer
# an independent repository up for deletion. So a candidate has to prove it
# belongs to this repository -- by its .git file, or by the target dir this repo
# would have keyed to it.
claims_this_repo() {
    d=$1
    if [ -e "$d/.git" ]; then
        # A .git *directory* is an independent clone, never a worktree.
        [ -f "$d/.git" ] || return 1
        gd=$(sed -n 's/^gitdir:[[:space:]]*//p' "$d/.git" 2>/dev/null | head -1)
        [ -n "$gd" ] || return 1
        case $gd in /*) ;; *) gd="$d/$gd" ;; esac
        gd=$(cd "$(dirname "$gd")" 2>/dev/null && pwd -P)/$(basename "$gd") 2>/dev/null || return 1
        case $gd in "$common_dir"/worktrees/*) return 0 ;; *) return 1 ;; esac
    fi
    # No .git at all: what a half-finished removal leaves. The tie to this repo
    # is then the cargo config it was provisioned with, or the target dir still
    # standing under this repo's key.
    td=$(read_target_dir "$d")
    case $td in "$BASE/$repo"/*) ;; *) return 1 ;; esac
    [ -f "$d/.cargo/config.toml" ] || [ -f "$d/.cargo/config" ] || [ -d "$td" ]
}

: > "$tmp/stale"
while IFS= read -r parent; do
    [ -d "$parent" ] || continue
    for d in "$parent"/*; do
        [ -d "$d" ] || continue
        cut -f1 "$tmp/inv" | grep -qxF "$d" && continue
        claims_this_repo "$d" || continue
        printf '%s\n' "$d" >> "$tmp/stale"
    done
done < "$tmp/parents"

# ── helpers ──────────────────────────────────────────────────────────────────

dir_bytes() {  # 0 for a missing directory, so callers can print unconditionally
    [ -d "$1" ] || { echo 0; return; }
    # du over a 200 GB target dir takes seconds, and --quiet reports counts only.
    [ "$quiet" -eq 1 ] && [ "$dry" -eq 1 ] && { echo 0; return; }
    du -sb "$1" 2>/dev/null | cut -f1 || du -sk "$1" 2>/dev/null | awk '{print $1 * 1024}'
}

human() { awk -v b="${1:-0}" 'BEGIN{ split("B KiB MiB GiB TiB", u, " ");
          i=1; while (b >= 1024 && i < 5) { b /= 1024; i++ }; printf "%.1f %s", b, u[i] }'; }

# Paths this user cannot unlink: owned by someone else, or sitting in a
# directory that is. docker bind-mounts are the usual source. Checked *before*
# git is asked to remove anything -- git deletes files until it hits one and
# then fails, having already deregistered the worktree.
alien_paths() {
    find "$1" -xdev \( ! -user "$ME" -o \( -type d ! -writable \) \) -print 2>/dev/null | head -5
}

# Contained in a long-lived branch (merged, however it was merged: this compares
# reachability, so a rebase-merge counts, a squash-merge does not) ...
is_contained() {
    for base in $(git for-each-ref --format='%(refname)' \
                    refs/remotes/origin/main refs/remotes/origin/master \
                    refs/remotes/origin/sprint-\* refs/heads/main refs/heads/master 2>/dev/null); do
        git merge-base --is-ancestor "$1" "$base" 2>/dev/null && return 0
    done
    return 1
}
# ... or at least present on a remote, so deleting the checkout loses nothing.
is_pushed()   { [ -n "$(git branch -r --contains "$1" 2>/dev/null | head -1)" ]; }
# ... or squash-merged, which only the forge can tell us about.
is_pr_merged() {
    [ -n "$1" ] && [ "$1" != "(detached)" ] || return 1
    command -v gh >/dev/null 2>&1 || return 1
    [ -n "$(gh pr list --head "$1" --state merged --limit 1 --json number \
              -q '.[].number' 2>/dev/null)" ]
}

safety_of() {  # prints a one-word verdict for a worktree's commits
    wt=$1; br=$2
    tip=$(git -C "$wt" rev-parse HEAD 2>/dev/null) || { echo unknown; return; }
    if is_contained "$tip";   then echo merged;  return; fi
    if is_pr_merged "$br";    then echo merged;  return; fi
    if is_pushed "$tip";      then echo pushed;  return; fi
    echo local-only
}

# The target dir is only ours to delete if it sits under the base and no other
# live worktree builds into it. Anything else is left alone and reported.
target_is_prunable() {  # $1 = target dir, $2 = the worktree being removed
    case $1 in "$BASE"/*) ;; *) return 1 ;; esac
    case $1 in "$BASE"/"$repo"/main/target) [ "$2" = "$main_wt" ] || return 1 ;; esac
    while IFS='	' read -r owt _ otd; do
        [ "$owt" = "$2" ] && continue
        [ "$otd" = "$1" ] && return 1
    done < "$tmp/inv"
    return 0
}

rm_tree() {  # $1 = directory; honours --sudo for foreign-owned leftovers
    if [ "$use_sudo" -eq 1 ] && [ -n "$(alien_paths "$1")" ]; then
        sudo rm -rf -- "$1"
    else
        rm -rf -- "$1"
    fi
}

# ── --list ───────────────────────────────────────────────────────────────────
if [ "$do_list" -eq 1 ]; then
    hdr "worktrees of $repo"
    printf '  %-30s %-38s %-11s %-9s %s\n' NAME BRANCH STATE DIRTY TARGET
    total=0
    while IFS='	' read -r wt br td; do
        if [ ! -d "$wt" ]; then state=missing
        elif [ "$wt" = "$main_wt" ]; then state=main
        else state=$(safety_of "$wt" "$br"); fi
        d=$(git -C "$wt" status --porcelain 2>/dev/null | wc -l | tr -d ' ')
        b=$(dir_bytes "$td"); total=$((total + b))
        printf '  %-30s %-38s %-11s %-9s %s\n' \
            "$(basename "$wt")" "$br" "$state" "$d" "$(human "$b")"
    done < "$tmp/inv"
    while IFS= read -r d; do
        [ -n "$d" ] || continue
        td=$(read_target_dir "$d"); b=$(dir_bytes "$td"); total=$((total + b))
        printf '  %-30s %-38s %-11s %-9s %s\n' \
            "$(basename "$d")" "(not registered)" stale \
            "$(find "$d" -xdev -type f 2>/dev/null | wc -l | tr -d ' ')" "$(human "$b")"
    done < "$tmp/stale"
    printf '\n  %s across %s worktrees' "$(human "$total")" "$(wc -l < "$tmp/inv" | tr -d ' ')"
    [ -s "$tmp/stale" ] && printf ' and %s stale leftover(s)' "$(wc -l < "$tmp/stale" | tr -d ' ')"
    printf '\n'
    [ -n "$selected" ] || [ "$do_orphans" -eq 1 ] || [ "$do_stale" -eq 1 ] || exit 0
fi

# ── plan ─────────────────────────────────────────────────────────────────────
# Resolve each argument, run every check, and only then touch anything: a
# half-applied sweep is the one outcome worse than not sweeping at all.
#
# Plan record: kind, prune-target flag, path, branch, target dir. The kind leads
# so no field can be empty at the front -- a record starting with an empty field
# loses it to IFS whitespace collapsing, and the whole line shifts by one.

: > "$tmp/plan"

resolve() {  # name or path -> worktree path, or "" if unknown
    want=$1
    [ -d "$want" ] && want=$(cd "$want" 2>/dev/null && pwd -P)
    while IFS='	' read -r wt _ _; do
        [ "$wt" = "$want" ] && { printf '%s\n' "$wt"; return 0; }
        [ "$(basename "$wt")" = "$1" ] && { printf '%s\n' "$wt"; return 0; }
    done < "$tmp/inv"
    return 1
}
resolve_stale() {
    want=$1
    [ -d "$want" ] && want=$(cd "$want" 2>/dev/null && pwd -P)
    while IFS= read -r d; do
        [ -n "$d" ] || continue
        [ "$d" = "$want" ] && { printf '%s\n' "$d"; return 0; }
        [ "$(basename "$d")" = "$1" ] && { printf '%s\n' "$d"; return 0; }
    done < "$tmp/stale"
    return 1
}

# A stale leftover: git no longer registers it, so its commits are safe by
# construction (they live in the branch, not the directory). What can still be
# lost is uncommitted work -- and git cannot tell us about it any more, so a
# leftover that still has its .git file is treated as a real worktree git merely
# forgot, and needs --force.
plan_stale() {
    d=$1
    td=$(read_target_dir "$d")
    [ "$quiet" -eq 1 ] || printf '\n  %s  (leftover, not registered)\n' "$(basename "$d")"
    note "$d"
    blocked=0
    n=$(find "$d" -xdev -type f 2>/dev/null | wc -l | tr -d ' ')
    if [ -e "$d/.git" ]; then
        if [ "$force" -eq 1 ]; then warn "still has a .git file -- a real worktree git forgot (--force)"
        else bad "still has a .git file; run 'git worktree prune' or pass --force"; blocked=1; fi
    else
        ok "no .git file -- what a half-finished removal left behind ($n file(s))"
    fi
    alien=$(alien_paths "$d")
    if [ -n "$alien" ]; then
        if [ "$use_sudo" -eq 1 ]; then warn "removing $(printf '%s' "$alien" | wc -l | tr -d ' ')+ foreign-owned path(s) with sudo"
        else bad "holds paths you cannot unlink; pass --sudo"; blocked=1
             [ "$quiet" -eq 1 ] || printf '%s\n' "$alien" | sed 's/^/      /'; fi
    fi
    prune_td=0
    if [ "$keep_target" -eq 1 ]; then note "target dir kept (--keep-target): $td"
    elif [ ! -d "$td" ]; then note "no target dir on disk ($td)"
    elif target_is_prunable "$td" "$d"; then prune_td=1
        ok "target dir: $td ($(human "$(dir_bytes "$td")"))"
    else warn "target dir left alone -- shared, or outside $BASE"; note "$td"; fi
    [ "$blocked" -eq 1 ] && return 0
    printf 'stale\t%s\t%s\t-\t%s\n' "$prune_td" "$d" "$td" >> "$tmp/plan"
}

[ -n "$selected" ] && hdr "plan"
# Fed from a file, not a pipe: a `while` on the right of a pipe runs in a
# subshell, and the refusal counter it increments would die with it -- the
# script would then exit 0 having refused everything.
printf '%s' "$selected" > "$tmp/args"
while IFS= read -r arg; do
    [ -n "$arg" ] || continue
    if wt=$(resolve "$arg"); then :
    elif d=$(resolve_stale "$arg"); then plan_stale "$d"; continue
    else bad "$arg: not a registered worktree of $repo, and no leftover by that name"; continue
    fi
    [ "$wt" = "$main_wt" ] && { bad "$arg: that is the main checkout"; continue; }

    br=$(awk -F'\t' -v w="$wt" '$1 == w {print $2}' "$tmp/inv")
    td=$(awk -F'\t' -v w="$wt" '$1 == w {print $3}' "$tmp/inv")
    blocked=0

    tracked=$(git -C "$wt" status --porcelain 2>/dev/null | grep -v '^??' | wc -l | tr -d ' ')
    untracked=$(git -C "$wt" status --porcelain 2>/dev/null | grep -c '^??' || true)
    state=$(safety_of "$wt" "$br")

    [ "$quiet" -eq 1 ] || printf '\n  %s  (%s)\n' "$(basename "$wt")" "$br"
    note "$wt"

    if [ "$tracked" -gt 0 ]; then
        if [ "$force" -eq 1 ]; then warn "$tracked uncommitted change(s) -- discarded (--force)"
        else bad "$tracked uncommitted change(s); commit them or pass --force"; blocked=1; fi
    fi
    if [ "$untracked" -gt 0 ]; then
        [ "$quiet" -eq 1 ] || git -C "$wt" status --porcelain | grep '^??' |
            sed 's/^?? /      untracked: /' | head -10
        if [ "$force" -eq 1 ]; then warn "$untracked untracked file(s) -- lost on removal (--force)"
        else bad "$untracked untracked file(s) would be lost; save them or pass --force"; blocked=1; fi
    fi
    case $state in
        merged) ok "commits are merged into a long-lived branch" ;;
        pushed) ok "commits exist on a remote branch" ;;
        *)      if [ "$force" -eq 1 ]; then warn "commits exist nowhere else -- kept only in the branch (--force)"
                else bad "commits exist on no remote and no long-lived branch; push them or pass --force"; blocked=1; fi ;;
    esac

    # Checked here rather than left to git: git deletes files until it reaches
    # such a path, fails, and by then has already deregistered the worktree.
    alien=$(alien_paths "$wt")
    if [ -n "$alien" ]; then
        [ "$quiet" -eq 1 ] || printf '%s\n' "$alien" | sed 's/^/      foreign-owned: /'
        if [ "$use_sudo" -eq 1 ]; then warn "foreign-owned path(s) -- removed with sudo after git gives up"
        else bad "git cannot unlink these (docker leftovers); pass --sudo"; blocked=1; fi
    fi

    prune_td=0
    if [ "$keep_target" -eq 1 ]; then
        note "target dir kept (--keep-target): $td"
    elif [ ! -d "$td" ]; then
        note "no target dir on disk ($td)"
    elif target_is_prunable "$td" "$wt"; then
        prune_td=1
        ok "target dir: $td ($(human "$(dir_bytes "$td")"))"
    else
        warn "target dir left alone -- shared, or outside $BASE"
        note "$td"
    fi

    [ "$blocked" -eq 1 ] && continue
    printf 'wt\t%s\t%s\t%s\t%s\n' "$prune_td" "$wt" "${br:--}" "$td" >> "$tmp/plan"
done < "$tmp/args"

# ── --stale ──────────────────────────────────────────────────────────────────
if [ "$do_stale" -eq 1 ]; then
    hdr "leftover directories git no longer registers"
    if [ ! -s "$tmp/stale" ]; then ok "none"; fi
    while IFS= read -r d; do
        [ -n "$d" ] || continue
        grep -qF "	$d	" "$tmp/plan" 2>/dev/null && continue   # already named explicitly
        plan_stale "$d"
    done < "$tmp/stale"
fi

# ── orphan target dirs ───────────────────────────────────────────────────────
if [ "$do_orphans" -eq 1 ]; then
    hdr "orphan target dirs under $BASE/$repo"
    found=0
    for d in "$BASE/$repo"/*; do
        [ -d "$d" ] || continue
        claimed=0
        while IFS='	' read -r _ _ otd; do
            case $otd in "$d"/*|"$d") claimed=1; break ;; esac
        done < "$tmp/inv"
        # A leftover still queued for removal owns its target dir until it goes.
        if [ "$claimed" -eq 0 ]; then
            while IFS= read -r s; do
                [ -n "$s" ] || continue
                case $(read_target_dir "$s") in "$d"/*|"$d") claimed=1; break ;; esac
            done < "$tmp/stale"
        fi
        [ "$claimed" -eq 1 ] && continue
        found=$((found + 1))
        [ "$quiet" -eq 1 ] || printf '  %-34s %s\n' "$(basename "$d")" "$(human "$(dir_bytes "$d")")"
        printf 'orphan\t1\t-\t-\t%s\n' "$d" >> "$tmp/plan"
    done
    [ "$found" -eq 0 ] && ok "none"
fi

if [ "$quiet" -eq 1 ] && [ "$dry" -eq 1 ]; then
    n_stale=0; [ -s "$tmp/stale" ] && n_stale=$(wc -l < "$tmp/stale" | tr -d ' ')
    [ "$do_stale" -eq 1 ] && [ "$n_stale" -gt 0 ] && printf \
        'worktree-prune: %s leftover worktree director(ies) git no longer registers -- worktree-prune --stale\n' "$n_stale"
    [ "$do_orphans" -eq 1 ] && [ "${found:-0}" -gt 0 ] && printf \
        'worktree-prune: %s orphan cargo target dir(s) under %s/%s -- worktree-prune --orphans\n' "$found" "$BASE" "$repo"
    exit 0
fi

[ -s "$tmp/plan" ] || { say ""; say "nothing to do."; exit $((fails > 0)); }

if [ "$dry" -eq 1 ]; then
    printf '\n  dry run -- nothing was removed. Re-run with --yes to apply.\n'
    exit $((fails > 0))
fi

# ── apply ────────────────────────────────────────────────────────────────────
hdr "removing"
freed=0

prune_target() {  # $1 = target dir, $2 = label
    b=$(dir_bytes "$1")
    if rm_tree "$1"; then freed=$((freed + b)); ok "$2 ($(human "$b"))"
    else bad "could not remove $1"; fi
    rmdir "$(dirname "$1")" 2>/dev/null || true
}

while IFS='	' read -r kind prune_td wt br td; do
    case $kind in
    orphan)
        b=$(dir_bytes "$td")
        if rm_tree "$td"; then freed=$((freed + b)); ok "orphan target dir $(basename "$td") ($(human "$b"))"
        else bad "could not remove $td"; fi
        ;;
    stale)
        if rm_tree "$wt"; then ok "leftover $(basename "$wt")"
        else bad "could not remove $wt -- target dir left in place"; continue; fi
        [ "$prune_td" = 1 ] && prune_target "$td" "target dir"
        ;;
    wt)
        if [ "$force" -eq 1 ]; then git worktree remove --force "$wt" || rc=$? ; rc=${rc:-0}
        else git worktree remove "$wt" || rc=$? ; rc=${rc:-0}; fi
        if [ "${rc:-0}" -ne 0 ]; then
            # git gives up on the first path it cannot unlink -- and by then has
            # already deregistered the worktree. Finish the job ourselves rather
            # than leaving a leftover behind; if it is still registered, git
            # refused for some other reason and the directory stays untouched.
            if git worktree list --porcelain | grep -qxF "worktree $wt"; then
                bad "git worktree remove failed for $wt -- nothing removed"
                unset rc; continue
            fi
            warn "git stopped partway (deregistered, files left) -- finishing the removal"
            if rm_tree "$wt"; then ok "worktree $(basename "$wt") (completed by hand)"
            else bad "could not remove $wt -- target dir left in place"; unset rc; continue; fi
        else
            ok "worktree $(basename "$wt")"
        fi
        unset rc
        [ "$prune_td" = 1 ] && prune_target "$td" "target dir"
        if [ "$del_branch" -eq 1 ] && [ -n "$br" ] && [ "$br" != "-" ] && [ "$br" != "(detached)" ]; then
            if [ "$force" -eq 1 ]; then git branch -D "$br" >/dev/null 2>&1 && ok "branch $br (forced)" \
                                         || bad "could not delete branch $br"
            elif git branch -d "$br" >/dev/null 2>&1; then ok "branch $br"
            else warn "branch $br kept -- git considers it unmerged; delete it with -f if you are sure"
            fi
        fi
        ;;
    esac
done < "$tmp/plan"

git worktree prune
if [ "$quiet" -eq 1 ]; then
    [ "$freed" -gt 0 ] && printf 'worktree-prune: freed %s\n' "$(human "$freed")"
    exit 0
fi
printf '\n  freed %s\n' "$(human "$freed")"
exit $((fails > 0))
