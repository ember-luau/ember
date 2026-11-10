#!/usr/bin/env bash
# Builds the plugin into Ember.rbxm. Studio resolves no @alias in a
# plugin, so the step after the compile points each @packages require at
# the Packages folder beside the scripts, by a relative path. The step is
# safe to run again on its own output.
#
# Usage: scripts/build.sh [--install]
# --install copies Ember.rbxm into the Studio plugins folder and replaces
# the old copy. Set PLUGINS_DIR to use a different folder.
set -euo pipefail
cd "$(dirname "$0")/.."

install=0
for arg in "$@"; do
	case "$arg" in
		--install) install=1 ;;
		*) echo "unknown flag: $arg" >&2; exit 2 ;;
	esac
done

alloy build
# Alloy writes a package require as an alias or as its place in the
# DataModel, @game/Packages/x. A plugin is not in the DataModel, so both
# must become a path to the Packages folder beside the modules.
sed -i -e "s#require('@packages/#require('./Packages/#g" \
	-e "s#require('@game/Packages/#require('./Packages/#g" build/*.luau
rojo build plugin.project.json -o Ember.rbxm
echo "built Ember.rbxm"

if [ "$install" = 1 ]; then
	# Vinegar (AUR) runs Studio in this Wine prefix.
	dir="${PLUGINS_DIR:-$HOME/.local/share/vinegar/prefixes/studio/drive_c/users/$USER/AppData/Local/Roblox/Plugins}"
	mkdir -p "$dir"
	cp -f Ember.rbxm "$dir/Ember.rbxm"
	echo "installed $dir/Ember.rbxm"
fi
