--- Transform λ through an ordinary module-loading call.
local M = {}
local dependency = require("sample")
function M:run(value)
  return dependency(value)
end
return M
