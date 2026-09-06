-- Generic syntax canary for named functions, scopes and call forms.
-- Module loading is deliberately exercised as a shadowable ordinary call.
local M = {}
local first, second
local limit <const> = 4
local left, right = 1, 2
local alpha <const>, beta <const> = 3, 4
local first_fn, second_fn = function() return 1 end, function() return 2 end

--- Returns a stable transformed value.
local function transform(value)
  return value + limit
end

function M.map(value)
  return transform(value)
end

function M:run(value, ...)
  return self.map(value), ...
end

local callback = function(value) return transform(value) end
M.finish = function(value) return callback(value) end
local handlers = {
  close = function(value) return M.finish(value) end,
}

local require = function(name) return name end
local dependency = require "local.name"
local quoted = "function invented() missing_call() end"
local block = [=[function also_invented() bogus() end]=]
-- function comment_only() phantom() end
M:run(1)
handlers.close(2)
do
  local first = 3
  callback(first)
end
for index = 1, 2 do
  local item = transform(index)
end
for key, entry in pairs(handlers) do
  entry(key)
end
return M, dependency, quoted, block, first, second
