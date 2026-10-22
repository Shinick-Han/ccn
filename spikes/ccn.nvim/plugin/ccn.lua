-- ccn.nvim: register commands and the BufWritePost hook.
-- Keep this file dumb; all logic lives in lua/ccn/init.lua.

if vim.g.loaded_ccn_nvim == 1 then
  return
end
vim.g.loaded_ccn_nvim = 1

require("ccn").setup()
