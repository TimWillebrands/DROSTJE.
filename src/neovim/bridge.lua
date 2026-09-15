local channel, generation = ...
local api = vim.api
local bridge = { sequence = 0, generation = generation, channel = channel }
_G.drost = bridge

local function notify(name, ...)
  vim.rpcnotify(channel, name, generation, ...)
end
local function cursor(buf)
  if api.nvim_get_current_buf() == buf then return api.nvim_win_get_cursor(0) end
  local mark = api.nvim_buf_get_mark(buf, '"')
  return { math.max(1, mark[1]), mark[2] }
end
function bridge.drain()
  bridge.sequence = bridge.sequence + 1
  notify('drost_drain', bridge.sequence, vim.fn.mode(1))
end
function bridge.snapshot()
  local buf = api.nvim_get_current_buf()
  bridge.sequence = bridge.sequence + 1
  notify('drost_snapshot', bridge.sequence, buf, api.nvim_buf_get_changedtick(buf),
    api.nvim_buf_get_lines(buf, 0, -1, true), cursor(buf), vim.fn.mode(1))
end
function bridge.attach(buf)
  for _, mode in ipairs({ 'n', 'i', 'x', 's' }) do
    vim.keymap.set(mode, '<F24>', '<Cmd>lua drost.snapshot()<CR>',
      { buffer = buf, silent = true, nowait = true })
  end
  for key, direction in pairs({ h = 'left', j = 'down', k = 'up', l = 'right' }) do
    local function focus_pane()
      notify('drost_focus_pane', api.nvim_get_current_buf(), direction)
    end
    for _, chord in ipairs({ '<C-w>' .. key, '<C-w><C-' .. key .. '>' }) do
      vim.keymap.set('n', chord, focus_pane,
        { buffer = buf, silent = true, nowait = true })
    end
  end
end
for _, mode in ipairs({ 'n', 'i', 'x', 's', 'o' }) do
  vim.keymap.set(mode, '<F23>', '<Cmd>lua drost.drain()<CR>', { silent = true, nowait = true })
end
vim.keymap.set('c', '<F23>', function()
  bridge.drain()
  return ''
end, { expr = true, silent = true, nowait = true })

-- RPC requests are deliberately uncached. The host services these on GPUI's
-- thread while its MessagePack reader remains free to handle nested requests.
vim.g.clipboard = {
  name = 'Drost native clipboard',
  copy = {
    ['+'] = function(lines, regtype) notify('drost_clipboard_set', lines, regtype) end,
    ['*'] = function(lines, regtype) notify('drost_clipboard_set', lines, regtype) end,
  },
  paste = {
    ['+'] = function() return vim.rpcrequest(channel, 'drost_clipboard_get') end,
    ['*'] = function() return vim.rpcrequest(channel, 'drost_clipboard_get') end,
  },
  cache_enabled = 0,
}
vim.opt.clipboard = 'unnamedplus'
vim.opt.wrap = true
vim.opt.linebreak = true
vim.opt.number = false
vim.opt.relativenumber = false
vim.opt.showtabline = 0
vim.opt.laststatus = 0
vim.opt.showmode = false
vim.opt.ruler = false
vim.opt.fillchars:append({ eob = ' ' })
vim.opt.mouse = 'a'
vim.opt.termguicolors = true
vim.opt.swapfile = false
vim.opt.undofile = false
vim.opt.hidden = true
vim.opt.shortmess:append('I')
vim.opt.guicursor = 'n-v-c:block,i-ci-ve:ver25,r-cr:hor20,o:hor50,a:Cursor-blinkwait700-blinkoff400-blinkon250'
function bridge.apply_theme(theme)
  vim.opt.background = theme.dark and 'dark' or 'light'
  api.nvim_set_hl(0, 'Normal', { fg = theme.foreground, bg = theme.background })
  api.nvim_set_hl(0, 'NormalFloat', { fg = theme.popover_foreground, bg = theme.popover })
  api.nvim_set_hl(0, 'Visual', { bg = theme.selection })
  api.nvim_set_hl(0, 'Cursor', { fg = theme.background, bg = theme.caret })
  api.nvim_set_hl(0, 'Search', { fg = theme.primary_foreground, bg = theme.primary })
end
api.nvim_create_autocmd({ 'CursorMoved', 'CursorMovedI', 'BufEnter' }, {
  callback = function()
    local buf = api.nvim_get_current_buf()
    notify('drost_cursor', buf, api.nvim_buf_get_changedtick(buf), cursor(buf))
  end,
})
return true
