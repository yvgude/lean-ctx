local M = {}

local stats_path = vim.fn.expand("~/.lean-ctx/stats.json")
local timer = nil
local current_text = "⚡ lean-ctx"

local function format_tokens(n)
  if n >= 1000000 then
    return string.format("%.1fM", n / 1000000)
  elseif n >= 1000 then
    return string.format("%.1fK", n / 1000)
  else
    return tostring(n)
  end
end

local function read_stats()
  local f = io.open(stats_path, "r")
  if not f then
    return nil
  end
  local content = f:read("*a")
  f:close()

  local tokens = content:match('"total_input_tokens"%s*:%s*(%d+)')
  local commands = content:match('"total_commands"%s*:%s*(%d+)')

  if tokens then
    return {
      tokens_saved = tonumber(tokens) or 0,
      commands = tonumber(commands) or 0,
    }
  end
  return nil
end

local function update()
  local s = read_stats()
  if s and s.tokens_saved > 0 then
    current_text = "⚡ " .. format_tokens(s.tokens_saved) .. " saved"
  else
    current_text = "⚡ lean-ctx"
  end
end

function M.start_refresh(interval_ms)
  update()
  if timer then
    timer:stop()
  end
  timer = vim.uv.new_timer()
  timer:start(interval_ms, interval_ms, vim.schedule_wrap(update))
end

function M.statusline_text()
  return current_text
end

return M
