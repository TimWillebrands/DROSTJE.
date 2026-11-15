local channel, generation = ...
local api = vim.api
local bridge = { sequence = 0, generation = generation, channel = channel }
_G.drostje = bridge

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
  notify('drostje_drain', bridge.sequence, vim.fn.mode(1))
end
function bridge.snapshot()
  local buf = api.nvim_get_current_buf()
  bridge.sequence = bridge.sequence + 1
  notify('drostje_snapshot', bridge.sequence, buf, api.nvim_buf_get_changedtick(buf),
    api.nvim_buf_get_lines(buf, 0, -1, true), cursor(buf), vim.fn.mode(1))
end
function bridge.attach(buf)
  vim.b[buf].drostje_draft = true
  for _, mode in ipairs({ 'n', 'i', 'x', 's' }) do
    vim.keymap.set(mode, '<F24>', '<Cmd>lua drostje.snapshot()<CR>',
      { buffer = buf, silent = true, nowait = true })
  end
end
for _, mode in ipairs({ 'n', 'i', 'x', 's', 'o' }) do
  vim.keymap.set(mode, '<F23>', '<Cmd>lua drostje.drain()<CR>', { silent = true, nowait = true })
end
vim.keymap.set('c', '<F23>', function()
  bridge.drain()
  return ''
end, { expr = true, silent = true, nowait = true })

vim.g.clipboard = {
  name = 'DROSTJE. native clipboard',
  copy = {
    ['+'] = function(lines, regtype) notify('drostje_clipboard_set', lines, regtype) end,
    ['*'] = function(lines, regtype) notify('drostje_clipboard_set', lines, regtype) end,
  },
  paste = {
    ['+'] = function() return vim.rpcrequest(channel, 'drostje_clipboard_get') end,
    ['*'] = function() return vim.rpcrequest(channel, 'drostje_clipboard_get') end,
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
vim.opt.mouse = 'a'
vim.opt.termguicolors = true
vim.opt.swapfile = false
vim.opt.undofile = false
vim.opt.hidden = true
vim.opt.shortmess:append('I')
vim.opt.guicursor = 'n-v-c:block,i-ci-ve:ver25,r-cr:hor20,o:hor50,a:Cursor/lCursor-blinkwait700-blinkoff400-blinkon250'
local quits = { quit = true, wq = true, xit = true, exit = true }
local quit_all = { qall = true, quitall = true, wqall = true, xall = true, cquit = true }
local writes = { write = true, update = true, wall = true }
local unloads = { bdelete = true, bwipeout = true, bunload = true }
local function last_window()
  if vim.fn.tabpagenr('$') > 1 then return false end
  local windows = 0
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    if api.nvim_win_get_config(win).relative == '' then windows = windows + 1 end
  end
  return windows <= 1
end
local function claims(cmd)
  local draft = vim.b.drostje_draft and #cmd.args == 0
  return quit_all[cmd.cmd] or (quits[cmd.cmd] and (draft or last_window()))
    or ((writes[cmd.cmd] or unloads[cmd.cmd]) and draft)
end
local function report(message, group)
  api.nvim_echo({ { message, group } }, group == 'ErrorMsg', {})
end
function bridge.run(cmd)
  local name, draft = cmd.cmd, vim.b.drostje_draft
  if unloads[name] then
    return report('A draft stays open until its Thread closes', 'ErrorMsg')
  end
  if writes[name] then return report('Drafts save automatically') end
  local write = ({ wq = 'write', xit = 'update', exit = 'update', wqall = 'wall', xall = 'wall' })[name]
  if write and not (draft and not quit_all[name]) then
    local ok, error = pcall(vim.cmd, write)
    if not ok then return report((tostring(error):gsub('^Vim:', '')), 'ErrorMsg') end
  end
  if quit_all[name] or last_window() then
    notify('drostje_leave')
  else
    vim.cmd.close({ bang = cmd.bang })
  end
end
local function command_line_enter()
  if vim.fn.getcmdtype() ~= ':' then return '<CR>' end
  local line = vim.fn.getcmdline()
  local ok, cmd = pcall(api.nvim_parse_cmd, line, {})
  if not ok or not claims(cmd) then return '<CR>' end
  bridge.claimed = cmd
  vim.fn.histadd(':', line)
  return '<C-U><BS><Cmd>lua drostje.run(drostje.claimed)<CR>'
end
for _, key in ipairs({ '<CR>', '<NL>', '<kEnter>' }) do
  vim.keymap.set('c', key, command_line_enter, { expr = true, silent = true })
end
for keys, cmd in pairs({ ZZ = 'xit', ZQ = 'quit', ['<C-W>q'] = 'quit', ['<C-W><C-Q>'] = 'quit' }) do
  vim.keymap.set('n', keys, function()
    local claimed = { cmd = cmd, args = {}, bang = keys == 'ZQ' }
    if claims(claimed) then
      bridge.run(claimed)
    else
      vim.cmd({ cmd = cmd, bang = claimed.bang })
    end
  end, { silent = true })
end

function bridge.theme(c)
  local mode = c.dark and 'dark' or 'light'
  if vim.o.background ~= mode then vim.o.background = mode end
  local groups = {
    Normal = { fg = c.foreground, bg = c.background },
    NormalNC = { fg = c.foreground, bg = c.background },
    NormalFloat = { fg = c.popover_foreground, bg = c.popover },
    FloatBorder = { fg = c.border, bg = c.popover },
    FloatTitle = { fg = c.foreground, bg = c.popover, bold = true },
    MsgArea = { fg = c.foreground, bg = c.background },
    MsgSeparator = { fg = c.border, bg = c.background },
    Visual = { fg = c.foreground, bg = c.selection },
    VisualNOS = { fg = c.foreground, bg = c.selection },
    Cursor = { fg = c.background, bg = c.caret },
    lCursor = { fg = c.background, bg = c.caret },
    TermCursor = { fg = c.background, bg = c.caret },
    Search = { fg = c.background, bg = c.caret },
    IncSearch = { fg = c.background, bg = c.caret },
    CurSearch = { fg = c.background, bg = c.caret },
    Substitute = { fg = c.background, bg = c.caret },
    MatchParen = { bg = c.selection, bold = true },
    NonText = { fg = c.muted },
    EndOfBuffer = { fg = c.muted },
    Whitespace = { fg = c.muted },
    SpecialKey = { fg = c.muted },
    Conceal = { fg = c.muted },
    LineNr = { fg = c.muted },
    LineNrAbove = { fg = c.muted },
    LineNrBelow = { fg = c.muted },
    CursorLineNr = { fg = c.foreground, bold = true },
    SignColumn = { fg = c.muted, bg = c.background },
    FoldColumn = { fg = c.muted, bg = c.background },
    Folded = { fg = c.muted, bg = c.surface },
    CursorLine = { bg = c.surface },
    CursorColumn = { bg = c.surface },
    ColorColumn = { bg = c.surface },
    QuickFixLine = { bg = c.selection },
    StatusLine = { fg = c.foreground, bg = c.surface },
    StatusLineNC = { fg = c.muted, bg = c.surface },
    WinBar = { fg = c.foreground, bg = c.background, bold = true },
    WinBarNC = { fg = c.muted, bg = c.background },
    WinSeparator = { fg = c.border, bg = c.background },
    VertSplit = { fg = c.border, bg = c.background },
    TabLine = { fg = c.muted, bg = c.surface },
    TabLineFill = { bg = c.surface },
    TabLineSel = { fg = c.foreground, bg = c.background, bold = true },
    Pmenu = { fg = c.popover_foreground, bg = c.popover },
    PmenuSel = { fg = c.foreground, bg = c.selection },
    PmenuKind = { fg = c.muted, bg = c.popover },
    PmenuKindSel = { fg = c.muted, bg = c.selection },
    PmenuExtra = { fg = c.muted, bg = c.popover },
    PmenuExtraSel = { fg = c.muted, bg = c.selection },
    PmenuMatch = { fg = c.accent, bg = c.popover, bold = true },
    PmenuMatchSel = { fg = c.accent, bg = c.selection, bold = true },
    PmenuSbar = { bg = c.popover },
    PmenuThumb = { bg = c.border },
    WildMenu = { fg = c.foreground, bg = c.selection },
    ModeMsg = { fg = c.muted, bold = true },
    MoreMsg = { fg = c.success },
    Question = { fg = c.info },
    ErrorMsg = { fg = c.danger },
    WarningMsg = { fg = c.warning },
    OkMsg = { fg = c.success },
    Title = { fg = c.accent, bold = true },
    Directory = { fg = c.info },
    Comment = { fg = c.muted, italic = true },
    Constant = { fg = c.info },
    String = { fg = c.success },
    Identifier = { fg = c.foreground },
    Function = { fg = c.accent },
    Statement = { fg = c.accent, bold = true },
    PreProc = { fg = c.warning },
    Type = { fg = c.warning },
    Special = { fg = c.info },
    Delimiter = { fg = c.muted },
    Operator = { fg = c.foreground },
    Underlined = { fg = c.accent, underline = true },
    Error = { fg = c.danger },
    Todo = { fg = c.warning, bold = true },
    Added = { fg = c.success },
    Changed = { fg = c.warning },
    Removed = { fg = c.danger },
    DiffAdd = { fg = c.success, bg = c.surface },
    DiffChange = { fg = c.warning, bg = c.surface },
    DiffDelete = { fg = c.danger, bg = c.surface },
    DiffText = { fg = c.foreground, bg = c.selection },
    SpellBad = { sp = c.danger, undercurl = true },
    SpellCap = { sp = c.warning, undercurl = true },
    SpellRare = { sp = c.info, undercurl = true },
    SpellLocal = { sp = c.info, undercurl = true },
    DiagnosticError = { fg = c.danger },
    DiagnosticWarn = { fg = c.warning },
    DiagnosticInfo = { fg = c.info },
    DiagnosticHint = { fg = c.muted },
    DiagnosticOk = { fg = c.success },
    DiagnosticUnderlineError = { sp = c.danger, undercurl = true },
    DiagnosticUnderlineWarn = { sp = c.warning, undercurl = true },
    DiagnosticUnderlineInfo = { sp = c.info, undercurl = true },
    DiagnosticUnderlineHint = { sp = c.muted, undercurl = true },
    DiagnosticUnderlineOk = { sp = c.success, undercurl = true },
  }
  for group, spec in pairs(groups) do
    api.nvim_set_hl(0, group, spec)
  end
  return true
end
api.nvim_create_autocmd({ 'CursorMoved', 'CursorMovedI', 'BufEnter' }, {
  callback = function()
    local buf = api.nvim_get_current_buf()
    notify('drostje_cursor', buf, api.nvim_buf_get_changedtick(buf), cursor(buf))
  end,
})
return true
