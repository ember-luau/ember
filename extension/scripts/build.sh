#!/usr/bin/env bash
# Builds the plugin into Ember.rbxm. Studio resolves no @alias in a
# plugin, so the step after the compile points each @packages require at
# the Packages folder beside the scripts, by a relative path. The step is
# safe to run again on its own output.
set -euo pipefail
cd "$(dirname "$0")/.."

alloy build
sed -i "s#require('@packages/#require('./Packages/#g" build/*.luau
rojo build plugin.project.json -o Ember.rbxm
echo "built Ember.rbxm"
