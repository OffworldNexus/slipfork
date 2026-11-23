" Vim ftplugin: Forkfile
" Vim 8+ / Neovim compatible, no external dependencies.

if exists("b:did_ftplugin")
  finish
endif
let b:did_ftplugin = 1

setlocal commentstring=#\ %s
setlocal comments=:#
setlocal formatoptions-=t formatoptions+=croql
setlocal shiftwidth=2 softtabstop=2 expandtab tabstop=2

let b:undo_ftplugin = "setlocal commentstring< comments< formatoptions< shiftwidth< softtabstop< expandtab< tabstop<"
