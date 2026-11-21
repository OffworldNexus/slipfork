/*
 * Forkfile highlighting for the documentation site.
 *
 * Why hand-written: Pygments has no `forkfile` lexer, the site builds offline
 * with `uvx --offline zensical build --strict`, and a plugin or bundled grammar
 * would add a dependency the docs gate cannot afford. This is one classic
 * script: no modules, no network, no dynamic code.
 *
 * It never rebuilds `code.innerHTML`. It splits Text nodes, so `code.textContent`
 * (what the copy button reads) and every element the build wrote stay intact.
 * Only `pre.forkfile > code` blocks — the custom `forkfile` fence — are touched;
 * a `console` or `json` block is left exactly as rendered.
 *
 * The same file is the test surface (`globalThis.ForkfileLexer`): `tokenize` and
 * `classify` are pure functions of the fence text, and `highlightCode` is the
 * only part that touches the document.
 */
(function (global) {
  "use strict";

  var MARK = "data-forkfile-highlighted";

  function lookup(list) {
    var map = {};
    for (var i = 0; i < list.length; i++) {
      map[list[i]] = true;
    }
    return map;
  }

  // Block openers take an identifier: `image api`, `service web`, `s3 uploads`.
  var BLOCK = lookup([
    "features", "domain", "image", "service", "env", "postgres", "redis",
    "s3", "cdn", "database"
  ]);
  var NAMED_BLOCK = lookup([
    "domain", "image", "service", "env", "postgres", "redis", "s3", "cdn",
    "database"
  ]);
  // Directives and property words inside a block.
  var DIRECTIVE = lookup([
    "use", "on", "arg", "stage", "dockerfile", "from", "port", "route", "run",
    "internal_ports", "pre-deploy", "post-deploy", "failed-deploy",
    "before-start", "when", "changed", "workdir", "version", "default",
    "strip-prefix", "rewrite", "exact"
  ]);
  // Expression builtins.
  var BUILTIN = lookup([
    "secret", "var", "random_hex", "random_base64", "random_alnum",
    "random_uuid"
  ]);

  // Classes styled by Zensical's own Pygments CSS and `stylesheets/extra.css`.
  var C = {
    keyword: "k",
    declaration: "kd",
    constant: "kc",
    type: "kt",
    name: "nf",
    variable: "nv",
    raw: "s1",
    string: "s2",
    escape: "se",
    comment: "c1",
    punctuation: "p",
    number: "mi",
    operator: "o"
  };
  var TOKEN_CLASSES = [
    C.keyword, C.declaration, C.constant, C.type, C.name, C.variable, C.raw,
    C.string, C.escape, C.comment, C.punctuation, C.number, C.operator
  ];

  // --- Is this fence a Forkfile at all? -----------------------------------
  // The first significant line must open with a block word. A grammar arrow or
  // an `a::=b` production is refused, so prose stays plain even next to code.
  function classify(text) {
    var lines = String(text).split("\n");
    for (var i = 0; i < lines.length; i++) {
      var line = lines[i].replace(/^\s+|\s+$/g, "");
      if (!line || line.charAt(0) === "#") {
        continue;
      }
      if (/^\S+:[0-9]+:[0-9]+:/.test(line) || line.indexOf("::=") >= 0) {
        return false;
      }
      var word = /^[A-Za-z_][A-Za-z0-9_-]*/.exec(line);
      return !!word && BLOCK[word[0]] === true;
    }
    return false;
  }

  function emit(out, cls, text) {
    if (text) {
      out.push({ text: text, cls: cls || null });
    }
  }

  function isSpace(ch) {
    return ch === " " || ch === "\t" || ch === "\n" || ch === "\r" ||
      ch === "\f" || ch === "\v";
  }

  function isWordStart(ch) {
    return (ch >= "A" && ch <= "Z") || (ch >= "a" && ch <= "z") || ch === "_";
  }

  function isWordChar(ch) {
    return isWordStart(ch) || (ch >= "0" && ch <= "9") || ch === "-";
  }

  function isDigit(ch) {
    return ch >= "0" && ch <= "9";
  }

  // A quoted string, with `\"` kept as its own escape token so it never ends
  // the run early. Line breaks inside a string are tolerated.
  function stringTokens(text, start, quoteClass) {
    var out = [];
    var i = start;
    var run = "";
    emit(out, quoteClass, text.charAt(i));
    i += 1;
    while (i < text.length) {
      var ch = text.charAt(i);
      if (ch === "\\") {
        if (run) {
          emit(out, quoteClass, run);
          run = "";
        }
        emit(out, C.escape, text.substr(i, 2));
        i += 2;
        continue;
      }
      if (ch === quoteClass.charAt(0)) {
        break;
      }
      run += ch;
      i += 1;
    }
    if (run) {
      emit(out, quoteClass, run);
    }
    if (i < text.length) {
      emit(out, quoteClass, text.charAt(i));
      i += 1;
    }
    return { tokens: out, end: i };
  }

  // `$port`, `$env.KEY`, `$port.name`.
  function variableAt(text, i) {
    var match = /^\$[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*/
      .exec(text.slice(i));
    return match ? match[0] : "";
  }

  // --- The lexer: one pass, no state beyond the last word ------------------
  function tokenize(text) {
    var source = String(text);
    var out = [];
    var i = 0;
    var n = source.length;
    var expectName = false;
    var lastWord = "";

    while (i < n) {
      var ch = source.charAt(i);

      if (ch === "#") {
        var stop = source.indexOf("\n", i);
        if (stop < 0) {
          stop = n;
        }
        emit(out, C.comment, source.slice(i, stop));
        i = stop;
        expectName = false;
        lastWord = "";
        continue;
      }

      if (ch === "'" || ch === '"') {
        var scanned = stringTokens(source, i, ch === '"' ? C.string : C.raw);
        for (var t = 0; t < scanned.tokens.length; t++) {
          emit(out, scanned.tokens[t].cls, scanned.tokens[t].text);
        }
        i = scanned.end;
        expectName = false;
        lastWord = "";
        continue;
      }

      if (isSpace(ch)) {
        var space = i;
        while (i < n && isSpace(source.charAt(i))) {
          i += 1;
        }
        emit(out, null, source.slice(space, i));
        continue;
      }

      if (ch === "$") {
        var variable = variableAt(source, i);
        if (variable) {
          emit(out, C.variable, variable);
          i += variable.length;
        } else {
          emit(out, null, ch);
          i += 1;
        }
        expectName = false;
        lastWord = "";
        continue;
      }

      if (ch === "{" || ch === "}" || ch === "(" || ch === ")" ||
          ch === "[" || ch === "]" || ch === ",") {
        emit(out, C.punctuation, ch);
        i += 1;
        expectName = false;
        continue;
      }

      if (ch === "?" && source.charAt(i + 1) === "?") {
        emit(out, C.operator, "??");
        i += 2;
        lastWord = "";
        continue;
      }

      if (ch === "=" || ch === ":" || ch === "|" || ch === "&") {
        emit(out, C.operator, ch);
        i += 1;
        lastWord = "";
        continue;
      }

      if (isDigit(ch) && !isWordChar(source.charAt(i - 1))) {
        var number = /^[0-9]+/.exec(source.slice(i))[0];
        emit(out, C.number, number);
        i += number.length;
        lastWord = "";
        continue;
      }

      if (isWordStart(ch)) {
        var word = /^[A-Za-z_][A-Za-z0-9_-]*/.exec(source.slice(i))[0];
        if (expectName) {
          emit(out, C.name, word);
          expectName = false;
        } else if (BLOCK[word] === true) {
          emit(out, C.keyword, word);
          if (NAMED_BLOCK[word] === true) {
            expectName = true;
          }
        } else if (DIRECTIVE[word] === true) {
          emit(out, C.declaration, word);
          if (word === "from") {
            expectName = true;
          }
        } else if (BUILTIN[word] === true) {
          emit(out, C.name, word);
        } else if (lastWord === "on") {
          emit(out, C.constant, word);
        } else {
          emit(out, null, word);
        }
        lastWord = word;
        i += word.length;
        continue;
      }

      emit(out, null, ch);
      i += 1;
      lastWord = "";
    }
    return out;
  }

  // --- The DOM walk: split Text nodes, never rebuild markup ---------------
  function textNodes(root) {
    var found = [];
    var stack = [root];
    while (stack.length) {
      var children = stack.pop().childNodes || [];
      for (var i = children.length - 1; i >= 0; i--) {
        var child = children[i];
        if (child.nodeType === 3) {
          found.push(child);
        } else if (child.nodeType === 1) {
          stack.push(child);
        }
      }
    }
    return found;
  }

  function wrap(cls, value) {
    var span = document.createElement("span");
    span.className = cls;
    span.appendChild(document.createTextNode(value));
    return span;
  }

  function highlightCode(code) {
    if (code.hasAttribute(MARK)) {
      return;
    }
    var text = code.textContent || "";
    if (!classify(text)) {
      return;
    }
    var segments = tokenize(text);
    var nodes = textNodes(code);
    var offset = 0;
    var segment = 0;
    var at = 0;

    for (var n = 0; n < nodes.length; n++) {
      var node = nodes[n];
      var data = node.nodeValue || "";
      var end = offset + data.length;
      var pieces = [];
      var cursor = offset;
      while (cursor < end) {
        while (segment < segments.length &&
               at + segments[segment].text.length <= cursor) {
          at += segments[segment].text.length;
          segment += 1;
        }
        if (segment >= segments.length) {
          pieces.push([data.slice(cursor - offset), null]);
          break;
        }
        var current = segments[segment];
        var stop = Math.min(end, at + current.text.length);
        pieces.push([data.slice(cursor - offset, stop - offset), current.cls]);
        cursor = stop;
      }
      var parent = node.parentNode;
      var changed = pieces.length !== 1 || pieces[0][1] !== null;
      if (changed && parent) {
        for (var p = 0; p < pieces.length; p++) {
          if (!pieces[p][0]) {
            continue;
          }
          var next = pieces[p][1]
            ? wrap(pieces[p][1], pieces[p][0])
            : document.createTextNode(pieces[p][0]);
          parent.insertBefore(next, node);
        }
        parent.removeChild(node);
      }
      offset = end;
    }
    code.setAttribute(MARK, "");
  }

  function highlight() {
    var blocks = document.querySelectorAll("pre.forkfile > code");
    for (var i = 0; i < blocks.length; i++) {
      highlightCode(blocks[i]);
    }
  }

  global.ForkfileLexer = {
    tokenize: tokenize,
    classify: classify,
    highlightCode: highlightCode,
    highlight: highlight,
    TOKEN_CLASSES: TOKEN_CLASSES,
    MARK: MARK
  };

  // `navigation.instant` swaps the document without a reload, so subscribe to
  // Material's observable when it exists and fall back to first load otherwise.
  if (typeof document$ !== "undefined" && document$.subscribe) {
    document$.subscribe(highlight);
  } else if (typeof document !== "undefined") {
    document.addEventListener("DOMContentLoaded", highlight);
  }
})(typeof globalThis !== "undefined" ? globalThis : this);
