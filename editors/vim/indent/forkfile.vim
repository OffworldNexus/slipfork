" Vim indent file: Forkfile (2-space, brace aware)
" Vim 8+ / Neovim compatible, no external dependencies.

if exists("b:did_indent")
  finish
endif
let b:did_indent = 1

setlocal indentexpr=GetForkfileIndent()
setlocal indentkeys=0{,0},!^F,o,O,0=else

let b:undo_indent = "setlocal indentexpr< indentkeys<"

if exists("*GetForkfileIndent")
  finish
endif

" Drop strings and comments so braces inside them never skew the depth.
function! s:ForkfileCode(text) abort
  let l:text = substitute(a:text, '"[^"]*"', '', 'g')
  let l:text = substitute(l:text, "'[^']*'", '', 'g')
  return substitute(l:text, '#.*$', '', '')
endfunction

function! s:ForkfileBraces(text) abort
  let l:opens = len(split(a:text, '{', 1)) - 1
  let l:closes = len(split(a:text, '}', 1)) - 1
  return l:opens - l:closes
endfunction

function! GetForkfileIndent() abort
  let l:prev = prevnonblank(v:lnum - 1)
  if l:prev == 0
    " Nothing above: leave blank/comment lines alone, code starts at 0.
    return getline(v:lnum) =~# '^\s*\%(#.*\)\=$' ? -1 : 0
  endif

  let l:depth = 0
  let l:lnum = 1
  while l:lnum <= l:prev
    let l:depth += s:ForkfileBraces(s:ForkfileCode(getline(l:lnum)))
    let l:lnum += 1
  endwhile
  if l:depth < 0
    let l:depth = 0
  endif

  let l:sw = shiftwidth()
  let l:line = getline(v:lnum)

  " A closing brace (including `} else {` / `} else on <expr> {`) lines up with
  " the line that opened the block; those same lines are net-zero for siblings.
  if l:line =~# '^\s*}' || l:line =~# '^\s*else\>'
    return max([0, (l:depth - 1) * l:sw])
  endif

  return l:depth * l:sw
endfunction

" vim: set fdm=marker :
