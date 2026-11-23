#!/bin/sh
# Headless verification of the Forkfile Vim/Neovim runtime plugin.
# Usage: editors/vim/check.sh [path/to/Forkfile]
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo=$(CDPATH= cd -- "$here/../.." && pwd)
plugin="$here"

if ! command -v vim >/dev/null 2>&1; then
  echo "fail: vim not found (install Vim 8+ or Neovim to run this check)" >&2
  exit 1
fi

scratch="${TMPDIR:-/tmp}/opencode"
mkdir -p "$scratch"
work=$(mktemp -d "$scratch/forkfile-vim.XXXXXX")
trap 'rm -rf "$work"' EXIT

fixture="$work/forkfile-vim-check.txt"
script="$work/forkfile-vim-check.vim"

# Deliberately unindented, extension `.txt`, header-only detection on line 1,
# with `"${...}"` interpolation (including an escaped-quote form), a property,
# `} else {`, a tool prefix, `$env.KEY`, and paths that must NOT be route paths.
cat >"$fixture" <<'FORKFILE'
# forkfile: 0.1
image api . {
service web {
port 8000
route /api
route /
workdir /app
run: /bin/sh
run: @uv run app --port $port
on dev {
run: @uv run app --reload
} else {
run: @uv run app
}
}
}
env common {
URL = "${var(HOST) ?? self.url}"
T = "${secret(TOKEN)}"
}
PG = cache.name
PORT = $env.PORT
FORKFILE

# Fail closed on an explicit non-existent target; fall back to the fixture only
# when no argument was given at all.
if [ "$#" -ge 1 ]; then
  target=$1
  [ -f "$target" ] || { echo "fail: no such file: $target" >&2; exit 1; }
  strict=0
else
  target="$repo/support/demo/Forkfile"
  if [ -f "$target" ]; then
    strict=1
  else
    target="$fixture"
    strict=0
  fi
fi

cat >"$script" <<'VIMSCRIPT'
set nocompatible
let s:plugin = $FORKFILE_PLUGIN
let s:target = $FORKFILE_TARGET
let s:fixture = $FORKFILE_FIXTURE
let s:strict = $FORKFILE_STRICT ==# '1'
let s:name = fnamemodify(s:target, ':t')

execute 'set runtimepath^=' . fnameescape(s:plugin)
filetype plugin indent on
syntax enable
execute 'source ' . fnameescape(s:plugin . '/ftdetect/forkfile.vim')

let s:fail = []

function! s:Check(cond, msg) abort
  if !a:cond
    call add(s:fail, a:msg)
  endif
endfunction

function! s:HasGroup(lnum, group) abort
  for l:c in range(1, max([1, strlen(getline(a:lnum))]))
    if synIDattr(synID(a:lnum, l:c, 1), 'name') ==# a:group
      return 1
    endif
  endfor
  return 0
endfunction

function! s:AllGroup(lnum, col, len, group) abort
  for l:c in range(a:col, a:col + a:len - 1)
    if synIDattr(synID(a:lnum, l:c, 1), 'name') !=# a:group
      return 0
    endif
  endfor
  return 1
endfunction

function! s:FindLine(pat) abort
  let l:n = 1
  while l:n <= line('$')
    if getline(l:n) =~# a:pat
      return l:n
    endif
    let l:n += 1
  endwhile
  return 0
endfunction

" The repo demo must contain every construct; a user-supplied file only needs
" to highlight what it actually has.
function! s:Expect(lnum, label, group) abort
  if a:lnum <= 0
    call s:Check(!s:strict, 'no ' . a:label . ' in ' . s:name)
    return
  endif
  call s:Check(s:HasGroup(a:lnum, a:group), a:label . ' not highlighted as ' . a:group)
endfunction

function! s:Main() abort
  execute 'edit ' . fnameescape(s:target)
  call s:Check(&filetype ==# 'forkfile', 'filetype of ' . s:name . ' is "' . &filetype . '", want forkfile')
  call s:Check(&syntax ==# 'forkfile', 'syntax of ' . s:name . ' is "' . &syntax . '", want forkfile')
  call s:Check(&commentstring ==# '# %s', 'commentstring is "' . &commentstring . '", want "# %s"')
  call s:Check(&shiftwidth == 4 && &expandtab, 'indent options are not 4-space expandtab')
  call s:Check(s:HasGroup(1, 'forkfileHeader'), 'version header not highlighted as forkfileHeader')

  let l:img = s:FindLine('^\s*image\s')
  call s:Check(l:img > 0, 'no image declaration found in ' . s:name)
  if l:img > 0
    call s:Check(s:HasGroup(l:img, 'forkfileKeyword'), 'image keyword not highlighted as forkfileKeyword')
    call s:Check(s:HasGroup(l:img, 'forkfileType'), 'image id not highlighted as forkfileType')
  endif
  call s:Expect(s:FindLine('@\h'), '@tool', 'forkfileTool')
  call s:Expect(s:FindLine('^\s*route\s'), 'route line', 'forkfileRoute')
  call s:Expect(s:FindLine('\$port\>'), '$port reference', 'forkfilePort')
  call s:Expect(s:FindLine('\h\.\h'), 'dotted property', 'forkfileProperty')

  " `gg=G` must be a no-op: the shipped Forkfile is already 4-space canonical.
  let l:before = getline(1, '$')
  normal! gg=G
  call s:Check(getline(1, '$') ==# l:before, s:name . ' is not 4-space canonical (gg=G changed it)')
  setlocal nomodified

  execute 'edit! ' . fnameescape(s:fixture)
  call s:Check(&filetype ==# 'forkfile', 'header detection failed for .txt fixture ("' . &filetype . '")')

  let l:interp = s:FindLine('\${')
  call s:Check(l:interp > 0, 'fixture lost its "${...}" line')
  if l:interp > 0
    call s:Check(s:HasGroup(l:interp, 'forkfileInterp'), '"${...}" not highlighted as forkfileInterp')
    call s:Check(s:HasGroup(l:interp, 'forkfileBuiltin'), 'builtin not highlighted inside "${...}"')
    call s:Check(s:HasGroup(l:interp, 'forkfileOperator'), 'operator not highlighted inside "${...}"')
    call s:Check(s:HasGroup(l:interp, 'forkfileProperty'), 'property not highlighted inside "${...}"')
    call s:Check(s:HasGroup(l:interp, 'forkfileBase'), 'self base not highlighted inside "${...}"')
  endif

  " keepend: an interpolation containing a call must not swallow the closing
  " `}` and bleed forkfileInterp onto the next line.
  let l:t = s:FindLine('^T = ')
  call s:Check(l:t > 0, 'fixture lost its "${...}" call line')
  if l:t > 0
    call s:Check(s:HasGroup(l:t, 'forkfileInterp'), '"${...}" call not highlighted as forkfileInterp')
    call s:Check(!s:HasGroup(l:t + 1, 'forkfileInterp'), 'forkfileInterp leaked past `}` onto the next line (missing keepend)')
    call s:Check(s:HasGroup(l:t + 1, 'forkfileDelimiter'), '`}` after "${...}" lost forkfileDelimiter')
  endif
  let l:portline = s:FindLine('--port \$port')
  call s:Check(l:portline > 0, 'fixture lost its $port line')
  if l:portline > 0
    call s:Check(s:HasGroup(l:portline, 'forkfilePort'), '$port not highlighted as forkfilePort')
  endif

  let l:prop = s:FindLine('cache\.name')
  call s:Check(l:prop > 0, 'fixture lost its dotted property line')
  if l:prop > 0
    call s:Check(s:HasGroup(l:prop, 'forkfileProperty'), 'property after "." not highlighted as forkfileProperty')
    call s:Check(s:HasGroup(l:prop, 'forkfileBase'), 'component id before "." not highlighted as forkfileBase')
  endif

  let l:env = s:FindLine('\$env\.')
  call s:Check(l:env > 0, 'fixture lost its $env.KEY line')
  if l:env > 0
    let l:txt = getline(l:env)
    let l:col = match(l:txt, '\$env\.') + 1
    let l:len = len(matchstr(l:txt, '\$env\.\h\w*'))
    call s:Check(l:col > 0 && s:AllGroup(l:env, l:col, l:len, 'forkfileEnvRef'), '$env.KEY not fully highlighted as forkfileEnvRef')
  endif

  let l:else = s:FindLine('^\s*} else {')
  call s:Check(l:else > 0, 'fixture lost its `} else {` line')
  if l:else > 0
    call s:Check(s:HasGroup(l:else, 'forkfileKeyword'), '`else` not highlighted as forkfileKeyword')
  endif

  let l:wd = s:FindLine('^\s*workdir\s')
  call s:Check(l:wd > 0, 'fixture lost its workdir line')
  if l:wd > 0
    call s:Check(!s:HasGroup(l:wd, 'forkfileRoute'), 'workdir path wrongly highlighted as forkfileRoute')
  endif
  let l:sh = s:FindLine('^\s*run: /')
  call s:Check(l:sh > 0, 'fixture lost its `run: /bin/sh` line')
  if l:sh > 0
    call s:Check(!s:HasGroup(l:sh, 'forkfileRoute'), 'run: command wrongly highlighted as forkfileRoute')
  endif

  normal! gg=G
  let l:want = [
        \ '# forkfile: 0.1',
        \ 'image api . {',
        \ '    service web {',
        \ '        port 8000',
        \ '        route /api',
        \ '        route /',
        \ '        workdir /app',
        \ '        run: /bin/sh',
        \ '        run: @uv run app --port $port',
        \ '        on dev {',
        \ '            run: @uv run app --reload',
        \ '        } else {',
        \ '            run: @uv run app',
        \ '        }',
        \ '    }',
        \ '}',
        \ 'env common {',
        \ '    URL = "${var(HOST) ?? self.url}"',
        \ '    T = "${secret(TOKEN)}"',
        \ '}',
        \ 'PG = cache.name',
        \ 'PORT = $env.PORT',
        \ ]
  call s:Check(getline(1, '$') ==# l:want, 'indent mismatch after gg=G: ' . string(getline(1, '$')))
endfunction

try
  call s:Main()
catch
  call add(s:fail, 'exception: ' . v:exception . ' (' . v:throwpoint . ')')
endtry

if empty(s:fail)
  call writefile(['ok: Forkfile syntax + indent verified in ' . s:name], '/dev/stdout')
  qall!
else
  for s:m in s:fail
    echo 'fail: ' . s:m
    call writefile(['fail: ' . s:m], '/dev/stderr')
  endfor
  cquit 1
endif
VIMSCRIPT

set +e
if command -v timeout >/dev/null 2>&1; then
  FORKFILE_PLUGIN="$plugin" FORKFILE_TARGET="$target" FORKFILE_FIXTURE="$fixture" FORKFILE_STRICT="$strict" \
    timeout 30 vim -N -u NONE -n -i NONE -es -S "$script"
else
  FORKFILE_PLUGIN="$plugin" FORKFILE_TARGET="$target" FORKFILE_FIXTURE="$fixture" FORKFILE_STRICT="$strict" \
    vim -N -u NONE -n -i NONE -es -S "$script"
fi
status=$?
set -e

if [ "$status" -ne 0 ]; then
  echo "fail: headless vim exited $status (see messages above)" >&2
  exit 1
fi

exit 0
