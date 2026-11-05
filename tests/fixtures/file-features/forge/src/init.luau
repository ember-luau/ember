local registry = require("@self/registry")

export type World = { name: string }

local forge = {}

function forge.world(name: string): World
	return { name = name }
end

function forge.hooked(): number
	return #registry.hooks
end

return forge
