local M = {}

local cached_path = nil

local candidates = {
  "lean-ctx",
  vim.fn.expand("~/.cargo/bin/lean-ctx"),
  "/usr/local/bin/lean-ctx",
  "/opt/homebrew/bin/lean-ctx",
  vim.fn.expand("~/.local/bin/lean-ctx"),
}

function M.resolve()
  if cached_path then
    return cached_path
  end

  for _, candidate in ipairs(candidates) do
    if vim.fn.executable(candidate) == 1 then
      cached_path = candidate
      return candidate
    end
  end

  return nil
end

function M.run(args, callback)
  local bin = M.resolve()
  if not bin then
    callback("lean-ctx binary not found")
    return
  end

  local cmd = vim.list_extend({ bin }, args)
  local env = {
    LEAN_CTX_ACTIVE = "0",
    NO_COLOR = "1",
    PATH = vim.env.PATH,
    HOME = vim.env.HOME,
  }

  vim.system(cmd, {
    env = env,
    text = true,
  }, function(result)
    vim.schedule(function()
      local output = result.stdout or ""
      if output == "" then
        output = result.stderr or ""
      end
      callback(output)
    end)
  end)
end

return M
