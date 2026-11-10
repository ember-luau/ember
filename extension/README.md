# Ember Studio plugin

A Roblox Studio plugin to search Ember and install packages into a place.
Its manifest is an `ember.luau` ModuleScript in the place, which Studio can
type check. LuaRocks rocks install too, as `rocks/<name>`.

## Scripts

`ember.luau` takes a `scripts` table, the Luau form of `[scripts]` in
`ember.toml`. The Project tab runs each one, and its output goes to the
Output window. A command is Luau code, a ModuleScript by its path, or a
function. A list runs its commands at once, and each line names its
command, as `embr run` does:

```luau
scripts = {
	hello = 'print("hello from", game.Name)',
	seed = "@game/ServerStorage/Tools/seed",
	both = { "@game/ServerStorage/Tools/seed", function(print) print("done") end },
},
```

A command gets its own `print` and `warn`. A module at a path runs when
it is required, and when it returns a function, that function runs with
the same two.

## Build

The plugin is an Alloy project. It uses the svelte-aly and Enamel ingots.

```sh
embr install
scripts/build.sh
```

`build.sh` runs `alloy build`, points the package requires at `Packages`,
and writes `Ember.rbxm` with Rojo. `scripts/build.sh --install` also
copies it into the Studio plugins folder and replaces the old copy.

## Check

```sh
alloy flux
alloy test
```
