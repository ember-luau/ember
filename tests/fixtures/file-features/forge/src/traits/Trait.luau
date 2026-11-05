export type Trait = { name: string }

return {
	new = function(name: string): Trait
		return { name = name }
	end,
}
