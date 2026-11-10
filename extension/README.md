# Ember Studio plugin

A Roblox Studio plugin to search Ember and install packages into a place.
Its manifest is an `ember.luau` ModuleScript in the place, which Studio can
type check. LuaRocks rocks install too, as `rocks/<name>`.

## Build

The plugin is an Alloy project. It uses the svelte-aly and Enamel ingots.

```sh
embr install
scripts/build.sh
```

`build.sh` runs `alloy build`, points the package requires at `Packages`,
and writes `Ember.rbxm` with Rojo. Copy `Ember.rbxm` into the Studio
plugins folder.

## Check

```sh
alloy flux
alloy test
```
