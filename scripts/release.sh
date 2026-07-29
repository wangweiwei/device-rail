#!/usr/bin/env bash
# Local release driver (bash + git + curl + python3 + node + cargo only).
#
# Usage:
#   scripts/release.sh publish            # interactive: major / minor / patch
#   scripts/release.sh publish patch      # non-interactive (required when stdin is not a terminal)
#   scripts/release.sh prepare minor      # rewrite only, leave everything uncommitted
#   scripts/release.sh status [X.Y.Z]     # what each registry actually holds (defaults to current)
#
# publish: preflight (branch / clean tree / absent tag / no registry already
# carrying the version) -> rewrite every version touchpoint -> refresh
# Cargo.lock through cargo metadata -> run check-release-version.mjs ->
# commit -> push branch -> annotated tag -> push tag. The tag push starts
# .github/workflows/publish-packages.yml. Versions are always derived from a
# bump, never typed by hand; registry versions are immutable and burned
# numbers are never reused.
#
# Environment:
#   RELEASE_BRANCH   branch releases are allowed from (default main)
set -euo pipefail

cd "$(dirname "$0")/.."

BRANCH="${RELEASE_BRANCH:-main}"
NPM_PACKAGES=(@devicerail/protocol @devicerail/client @devicerail/live-visualizer
	@devicerail/tool-adapter @devicerail/recorder @devicerail/yaml-adapter)
RUST_CRATES=(devicerail-protocol devicerail-client)
PYTHON_PACKAGE=devicerail-client
MANIFESTS=(package.json packages/protocol/package.json packages/client/package.json
	packages/tool-adapter/package.json packages/recorder/package.json
	packages/live-visualizer/package.json packages/yaml-adapter/package.json
	packages/playwright-driver/package.json apps/live-visualizer/package.json)

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

current_version() {
	node -p 'JSON.parse(require("node:fs").readFileSync("package.json", "utf8")).version'
}

bump_version() { # bump_version <current> <major|minor|patch>
	local cur_major cur_minor cur_patch
	IFS='.' read -r cur_major cur_minor cur_patch <<<"$1"
	case "$2" in
	major) echo "$((cur_major + 1)).0.0" ;;
	minor) echo "$cur_major.$((cur_minor + 1)).0" ;;
	patch) echo "$cur_major.$cur_minor.$((cur_patch + 1))" ;;
	*) die "invalid bump kind: $2" ;;
	esac
}

# ---- registry lookups. A failed request counts as missing; the real gate is
# ---- the publish workflow, whose loops skip whatever a registry already has.
crates_has() { # crates_has <crate> <version>
	curl --silent --header 'User-Agent: DeviceRail release script' \
		"https://crates.io/api/v1/crates/$1" 2>/dev/null |
		python3 -c 'import sys,json
try: d=json.load(sys.stdin)
except Exception: sys.exit(1)
sys.exit(0 if sys.argv[1] in [v["num"] for v in d.get("versions",[])] else 1)' "$2" 2>/dev/null
}

npm_has() { # npm_has <package> <version>
	curl --silent "https://registry.npmjs.org/$1" 2>/dev/null |
		python3 -c 'import sys,json
try: d=json.load(sys.stdin)
except Exception: sys.exit(1)
sys.exit(0 if sys.argv[1] in d.get("versions",{}) else 1)' "$2" 2>/dev/null
}

pypi_has() { # pypi_has <package> <version>
	curl --silent "https://pypi.org/pypi/$1/json" 2>/dev/null |
		python3 -c 'import sys,json
try: d=json.load(sys.stdin)
except Exception: sys.exit(1)
sys.exit(0 if sys.argv[1] in d.get("releases",{}) else 1)' "$2" 2>/dev/null
}

print_status() { # print_status <version> ; last line is the missing count
	local ver="$1" missing=0 name
	for name in "${NPM_PACKAGES[@]}"; do
		if npm_has "$name" "$ver"; then
			printf '  published  npm        %s\n' "$name"
		else
			printf '  missing    npm        %s\n' "$name"
			missing=$((missing + 1))
		fi
	done
	if pypi_has "$PYTHON_PACKAGE" "$ver"; then
		printf '  published  PyPI       %s\n' "$PYTHON_PACKAGE"
	else
		printf '  missing    PyPI       %s\n' "$PYTHON_PACKAGE"
		missing=$((missing + 1))
	fi
	for name in "${RUST_CRATES[@]}"; do
		if crates_has "$name" "$ver"; then
			printf '  published  crates.io  %s\n' "$name"
		else
			printf '  missing    crates.io  %s\n' "$name"
			missing=$((missing + 1))
		fi
	done
	echo "$missing"
}

require_unpublished() { # require_unpublished <version>
	local name
	for name in "${NPM_PACKAGES[@]}"; do
		npm_has "$name" "$1" && die "$1 is already on npm ($name); registry versions are immutable, pick another bump"
	done
	pypi_has "$PYTHON_PACKAGE" "$1" && die "$1 is already on PyPI; registry versions are immutable, pick another bump"
	for name in "${RUST_CRATES[@]}"; do
		crates_has "$name" "$1" && die "$1 is already on crates.io ($name); registry versions are immutable, pick another bump"
	done
	return 0
}

require_clean() {
	[ -z "$(git status --porcelain)" ] || die "the working tree must be clean; commit or stash first"
}

require_branch() {
	[ "$(git rev-parse --abbrev-ref HEAD)" = "$BRANCH" ] ||
		die "releases run from $BRANCH (set RELEASE_BRANCH to override)"
}

require_absent_tag() { # require_absent_tag <tag>
	[ -z "$(git tag --list "$1")" ] || die "$1 already exists locally; registry versions are immutable"
	[ -z "$(git ls-remote --tags origin "refs/tags/$1")" ] || die "$1 already exists on origin; registry versions are immutable"
}

# ---- touchpoint rewrite. Any failure rolls the whole rewrite back: the tree
# ---- was proven clean on entry, so every dirty file here is our own work.
apply_version() { # apply_version <new>
	if ! rewrite_all "$1"; then
		git checkout -- .
		die "version rewrite failed; every change was rolled back"
	fi
}

rewrite_all() { # rewrite_all <new>
	local new="$1" m count

	for m in "${MANIFESTS[@]}"; do
		count="$(grep -cE '^  "version": "[^"]+",$' "$m" || true)"
		[ "$count" = "1" ] || { echo "$m: expected exactly one version line, found $count" >&2; return 1; }
		sed -i.bak -E "s/^  \"version\": \"[^\"]+\",$/  \"version\": \"$new\",/" "$m"
		rm -f "$m.bak"
	done

	count="$(grep -cE '^version = "[^"]+"$' Cargo.toml || true)"
	[ "$count" = "1" ] || { echo "Cargo.toml: expected exactly one workspace version, found $count" >&2; return 1; }
	sed -i.bak -E "s/^version = \"[^\"]+\"$/version = \"$new\"/" Cargo.toml
	rm -f Cargo.toml.bak

	# Both devicerail-protocol pins (dependencies and dev-dependencies) — the
	# release gate's regex only reaches the first one, so verify the count here.
	count="$(grep -cE '^devicerail-protocol = \{ path = "\.\./protocol", version = "' crates/client/Cargo.toml || true)"
	[ "$count" = "2" ] || { echo "crates/client/Cargo.toml: expected two protocol pins, found $count" >&2; return 1; }
	sed -i.bak -E "s|^(devicerail-protocol = \{ path = \"\.\./protocol\", version = )\"[^\"]+\"|\1\"$new\"|" crates/client/Cargo.toml
	rm -f crates/client/Cargo.toml.bak

	count="$(grep -cE '^version = "[^"]+"$' packages/python-client/pyproject.toml || true)"
	[ "$count" = "1" ] || { echo "pyproject.toml: expected exactly one version, found $count" >&2; return 1; }
	sed -i.bak -E "s/^version = \"[^\"]+\"$/version = \"$new\"/" packages/python-client/pyproject.toml
	rm -f packages/python-client/pyproject.toml.bak

	sed -i.bak -E "s/^__version__ = \"[^\"]+\"$/__version__ = \"$new\"/" \
		packages/python-client/src/devicerail/__init__.py
	rm -f packages/python-client/src/devicerail/__init__.py.bak
	grep -q "^__version__ = \"$new\"$" packages/python-client/src/devicerail/__init__.py ||
		{ echo "__init__.py: __version__ was not rewritten" >&2; return 1; }

	sed -i.bak -E "s/client_version: str = \"[^\"]+\"/client_version: str = \"$new\"/" \
		packages/python-client/src/devicerail/client.py
	rm -f packages/python-client/src/devicerail/client.py.bak
	grep -q "client_version: str = \"$new\"" packages/python-client/src/devicerail/client.py ||
		{ echo "client.py: client_version default was not rewritten" >&2; return 1; }

	NEW="$new" python3 - <<'PY' || return 1
import datetime, os, re
new = os.environ["NEW"]
date = datetime.date.today().isoformat()
s = open("CHANGELOG.md", encoding="utf-8").read()
assert len(re.findall(r"^## \[Unreleased\]$", s, re.M)) == 1, "CHANGELOG.md is missing its Unreleased section"
s = re.sub(
    r"^## \[Unreleased\]$",
    f"## [Unreleased]\n\n## [{new}] - {date}",
    s, count=1, flags=re.M,
)
open("CHANGELOG.md", "w", encoding="utf-8").write(s)
PY

	# Refresh the workspace member versions in Cargo.lock and prove the
	# workspace still resolves, then run the authoritative consistency gate.
	cargo metadata --format-version 1 --quiet >/dev/null || { echo "the workspace no longer resolves" >&2; return 1; }
	node scripts/check-release-version.mjs "v$new" || return 1
}

choose_release() { # choose_release <current> ; prints the chosen bump
	local current="$1" options=(major minor patch) o i=1 ans
	[ -t 0 ] || die "bump must be given explicitly (major / minor / patch) when stdin is not a terminal"
	printf 'current release %s\n\n' "$current" >&2
	for o in "${options[@]}"; do
		printf '  %d  %-5s  %s\n' "$i" "$o" "$(bump_version "$current" "$o")" >&2
		i=$((i + 1))
	done
	printf '\n' >&2
	read -r -p "release [1-3]: " ans || die "release selection was cancelled"
	[[ "$ans" =~ ^[0-9]+$ ]] && [ "$ans" -ge 1 ] && [ "$ans" -le 3 ] ||
		die "release must be 1, 2, or 3"
	echo "${options[$((ans - 1))]}"
}

cmd_publish() { # cmd_publish [bump]
	require_branch
	require_clean
	local current bump new tag
	current="$(current_version)"
	bump="${1:-$(choose_release "$current")}"
	case "$bump" in major | minor | patch) ;; *) die "bump must be major, minor, or patch" ;; esac
	new="$(bump_version "$current" "$bump")"
	tag="v$new"
	require_absent_tag "$tag"
	require_unpublished "$new"
	info "current $current  ->  target $new  (tag $tag)"
	apply_version "$new"
	git commit --all --quiet --message "release: $tag"
	info "version rewrite committed"
	git push origin "$BRANCH"
	git tag -a "$tag" -m "$tag"
	git push origin "$tag"
	info "pushed $tag; publish-packages.yml now needs an approval on each of the crates-io, npm, and pypi environments"
}

cmd_prepare() { # cmd_prepare [bump]
	require_branch
	require_clean
	local current bump new
	current="$(current_version)"
	bump="${1:-$(choose_release "$current")}"
	case "$bump" in major | minor | patch) ;; *) die "bump must be major, minor, or patch" ;; esac
	new="$(bump_version "$current" "$bump")"
	require_absent_tag "v$new"
	require_unpublished "$new"
	apply_version "$new"
	git diff --name-only | sed 's/^/  /'
	info "prepared $new (uncommitted): describe the release in CHANGELOG.md, commit, then push tag v$new"
}

cmd_status() { # cmd_status [version]
	local ver="${1:-$(current_version)}" rows missing
	[[ "$ver" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "status takes a version shaped like X.Y.Z"
	rows="$(print_status "$ver")"
	missing="$(printf '%s\n' "$rows" | tail -1)"
	printf '%s\n' "$rows" | sed '$d'
	if [ "$missing" = "0" ]; then
		info "every public package is published at $ver"
	else
		printf '\nretry the missing ecosystems against the release tag:\n' >&2
		printf '  gh workflow run publish-packages.yml --ref v%s -f release_tag=v%s \\\n' "$ver" "$ver" >&2
		printf '    -f publish_npm=<true|false> -f publish_pypi=<true|false> -f publish_crates=<true|false>\n' >&2
	fi
}

case "${1:-}" in
publish) cmd_publish "${2:-}" ;;
prepare) cmd_prepare "${2:-}" ;;
status)  cmd_status  "${2:-}" ;;
*) die "usage: $0 <publish|prepare|status> [major|minor|patch|X.Y.Z]" ;;
esac
