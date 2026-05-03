local M = {}

local binary = require("lean-ctx.binary")
local stats = require("lean-ctx.stats")

M.config = {
  statusline = true,
  refresh_interval_ms = 30000,
  auto_setup = true,
}

function M.setup(opts)
  M.config = vim.tbl_deep_extend("force", M.config, opts or {})

  local bin = binary.resolve()
  if not bin then
    vim.notify("lean-ctx: binary not found. Install: cargo install lean-ctx", vim.log.levels.WARN)
    return
  end

  M._create_commands()

  if M.config.statusline then
    stats.start_refresh(M.config.refresh_interval_ms)
  end
end

function M._create_commands()
  vim.api.nvim_create_user_command("LeanCtxSetup", function()
    binary.run({ "setup" }, function(output)
      vim.notify(output, vim.log.levels.INFO)
    end)
  end, { desc = "Run lean-ctx setup" })

  vim.api.nvim_create_user_command("LeanCtxDoctor", function()
    binary.run({ "doctor" }, function(output)
      vim.notify(output, vim.log.levels.INFO)
    end)
  end, { desc = "Run lean-ctx doctor" })

  vim.api.nvim_create_user_command("LeanCtxGain", function()
    binary.run({ "gain" }, function(output)
      vim.notify(output, vim.log.levels.INFO)
    end)
  end, { desc = "Show lean-ctx gain report" })

  vim.api.nvim_create_user_command("LeanCtxDashboard", function()
    binary.run({ "dashboard" }, function(_) end)
  end, { desc = "Open lean-ctx dashboard" })
end

function M.statusline()
  return stats.statusline_text()
end

return M
