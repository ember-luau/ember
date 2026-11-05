local forge = require("./packages/luau/forge")
local kit = require("./packages/luau/kit")

local world: forge.World = forge.world("one")
local trait: forge.Trait = forge.trait("two")
print(world.name, trait.name, forge.run(), kit.forge.run(), forge.start)
