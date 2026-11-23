" Vim filetype detection for Forkfile (# forkfile: 0.1).
" Vim 8+ / Neovim compatible, no external dependencies.

augroup forkfile_detect
  au!
  " Explicit names: `Forkfile` (repo root) and any `*.forkfile`.
  au BufRead,BufNewFile Forkfile,*.forkfile setfiletype forkfile

  " Header sniffing: line 1 `# forkfile: <version>`.
  " Only when nothing (or plain text) claimed the buffer yet.
  au BufReadPost * if getline(1) =~# '^#\s*forkfile:'
        \ && &filetype !=# 'forkfile'
        \ && (!did_filetype() || &filetype ==# 'text')
        \ | execute 'setlocal filetype=forkfile'
        \ | endif
augroup END
