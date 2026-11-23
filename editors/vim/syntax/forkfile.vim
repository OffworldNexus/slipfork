" Vim syntax file
" Language:    Forkfile (# forkfile: 0.1)
" Maintainer:  Slipfork
" Last Change: 2026-11-23
" Vim 8+ / Neovim compatible, no external dependencies.

if exists("b:current_syntax")
  finish
endif

syn case match

" Comments -------------------------------------------------------------------
syn keyword forkfileTodo contained TODO FIXME NOTE XXX
syn match   forkfileComment /#.*$/ contains=forkfileTodo

" `# forkfile: 0.1` version header. Defined after the comment so it wins at
" the same start column; the version token is part of the PreProc span.
syn match forkfileHeader /^#\s*forkfile:\s*\S\+/

" Directives and block keywords (hyphenated ones need explicit alternation) --
syn match forkfileKeyword /\<\%(features\|default\|image\|from\|dockerfile\|stage\|workdir\|arg\|use\|env\|before-start\|pre-deploy\|post-deploy\|failed-deploy\|run\|when\|changed\|service\|port\|internal_ports\|route\|strip-prefix\|rewrite\|exact\|domain\|postgres\|database\|version\|redis\|s3\|cdn\|policy\|on\|else\)\>/

" Identifiers inside `"${...}"` (declared before builtins/properties so the
" more specific groups win at the same start column).
syn match forkfileIdent /\<\l\w*\>/ contained

" Declared component name after a declaring keyword. Anchored to the line start
" so a `run:` command such as `... image build` never colours `build`.
syn match forkfileType /\%(^\s*\%(image\|service\|postgres\|database\|redis\|s3\|cdn\|domain\)\s\+\)\@<=\<\h[a-zA-Z0-9_-]*\>/

" Component/base identifier before a dot: `cache` in `cache.url`.
syn match forkfileBase /\<\h[a-zA-Z0-9_-]*\ze\./

" @uv / @pnpm / @cargo / ... -------------------------------------------------
syn match forkfileTool /@\h\w*/

" $port and $VAR references ---------------------------------------------------
syn match forkfileEnvRef /\$\w\+/
syn match forkfileEnvRef /\$env\.\h\w*/
syn match forkfilePort /\$port\>/

" Property after a dot: app_db.url, uploads.endpoint, .name, ... -------------
syn match forkfileProperty /\.\@<=\h[a-zA-Z0-9_-]*/

" `secret(KEY, <generator>)` / `var(KEY, <generator>)` builtins and their generators.
syn match forkfileBuiltin /\<\%(var\|secret\|random_string\|random_hex\|random_base64\|random_uuid\)\>/

" Expression operators --------------------------------------------------------
syn match forkfileOperator /??\|[+|&!:=]/

" Route paths, anchored to their owning keyword (`route /api`, `route /`,
" `rewrite /old`). Bare `workdir /app` / `run: /bin/sh` never match. -----------
syn match forkfileRoute /\%(^\s*\%(route\|rewrite\|exact\)\>\s\+\)\@<=\/\S*/
syn match forkfileRoute /\%(^\s*\%(route\|rewrite\|exact\)\>\s\+\)\@<=\/\ze\%(\s\|$\)/

" Literals and punctuation ----------------------------------------------------
syn match forkfileBoolean /\<\%(true\|false\)\>/
syn match forkfileNumber /-\?\<\d\+\>/
syn match forkfileDelimiter /[][{}]/

" `"${...}"` interpolation region inside a double-quoted string. `keepend` stops
" a nested escaped-quote string from swallowing the `}` and leaking the region
" onto following lines.
syn region forkfileInterp start=/\${/ end=/}/ keepend contains=forkfileBuiltin,forkfileBase,forkfileProperty,forkfileOperator,forkfileString,forkfileNumber,forkfileBoolean,forkfileIdent

" Strings; interpolation is highlighted inside double-quoted strings only.
syn region forkfileString start=/"/ skip=/\\\\\|\\"/ end=/"/ contains=forkfileInterp
syn region forkfileString start=/'/ skip=/\\\\\|\\'/ end=/'/

hi def link forkfileHeader    PreProc
hi def link forkfileComment   Comment
hi def link forkfileTodo      Todo
hi def link forkfileKeyword   Keyword
hi def link forkfileType      Type
hi def link forkfileBase      Identifier
hi def link forkfileTool      Function
hi def link forkfileIdent     Identifier
hi def link forkfileBuiltin   Function
hi def link forkfileProperty  Identifier
hi def link forkfileOperator  Operator
hi def link forkfilePort      Special
hi def link forkfileEnvRef    Identifier
hi def link forkfileString    String
hi def link forkfileBoolean   Boolean
hi def link forkfileNumber    Number
hi def link forkfileRoute     Special
hi def link forkfileDelimiter Delimiter

let b:current_syntax = "forkfile"
