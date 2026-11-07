-- exports nothing: the feature plugs itself into the core when it loads.
-- the luau CLI runs a module that returns nil again on each require, so the
-- feature checks that it is not in the registry yet.
local registry = require("./registry")

if not table.find(registry.hooks, "hooks") then
	table.insert(registry.hooks, "hooks")
end

return nil
