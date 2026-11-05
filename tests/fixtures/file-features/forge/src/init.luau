export type World = { name: string }

local forge = {}

function forge.world(name: string): World
	return { name = name }
end

return forge
