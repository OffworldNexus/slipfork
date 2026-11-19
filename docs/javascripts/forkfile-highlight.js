/* Forkfile syntax highlighting for the Zensical docs site.
 *
 * Pygments has no `forkfile` lexer, so published snippets are written as
 * ```text title="Forkfile" and land in the page as an unstructured text block.
 * This script runs after Zensical's own highlighting, rewrites only those
 * blocks (never diagnostics or other text blocks), and leaves the copied text
 * byte-for-byte identical: every token is wrapped in a span whose textContent
 * is the original slice.
 *
 * Dependency-free, no eval, no network. Tokenizer is exported for tests.
 */
(function (global) {
  "use strict";

  const KEYWORDS = [
    "project", "features", "env", "service", "postgres", "redis", "s3", "domain",
    "task", "on", "run", "use", "from", "stage", "port", "route", "volume", "setup",
    "pre-deploy", "post-deploy", "failed-deploy", "database", "version", "dockerfile",
    "internal_ports", "size", "pool", "in",
  ];
  const BUILTINS = [
    "secret", "var", "random_hex", "random_base64", "random_string", "uuid4",
  ];
  const KEYWORD_SET = new Set(KEYWORDS);
  const BUILTIN_SET = new Set(BUILTINS);

  const CLASS = {
    keyword: "ff-keyword",
    builtin: "ff-builtin",
    string: "ff-string",
    number: "ff-number",
    comment: "ff-comment",
    operator: "ff-operator",
    reference: "ff-reference",
    ident: "ff-ident",
  };

  const IDENT_START = /[A-Za-z_]/;
  const IDENT_PART = /[A-Za-z0-9_-]/;
  const REFERENCE = /\$[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*/y;
  const NUMBER = /\d+(?:\.\d+)*/y;
  const WHITESPACE = /[ \t\r\n]/;

  /** Split Forkfile source into ordered tokens that rejoin to the exact source. */
  function tokenize(source) {
    const tokens = [];
    const n = source.length;
    let i = 0;

    while (i < n) {
      const c = source[i];

      if (WHITESPACE.test(c)) {
        let j = i + 1;
        while (j < n && WHITESPACE.test(source[j])) j += 1;
        tokens.push({ type: "plain", value: source.slice(i, j) });
        i = j;
        continue;
      }

      if (c === "#") {
        let j = i + 1;
        while (j < n && source[j] !== "\n") j += 1;
        tokens.push({ type: "comment", value: source.slice(i, j) });
        i = j;
        continue;
      }

      if (c === '"' || c === "'") {
        let j = i + 1;
        while (j < n && source[j] !== c && source[j] !== "\n") {
          j = Math.min(j + (source[j] === "\\" ? 2 : 1), n);
        }
        if (j < n && source[j] === c) j += 1;
        tokens.push({ type: "string", value: source.slice(i, j) });
        i = j;
        continue;
      }

      if (c === "$") {
        REFERENCE.lastIndex = i;
        const match = REFERENCE.exec(source);
        if (match) {
          tokens.push({ type: "reference", value: match[0] });
          i = REFERENCE.lastIndex;
          continue;
        }
      }

      if (c >= "0" && c <= "9") {
        NUMBER.lastIndex = i;
        const match = NUMBER.exec(source);
        tokens.push({ type: "number", value: match[0] });
        i = NUMBER.lastIndex;
        continue;
      }

      if (IDENT_START.test(c)) {
        let j = i + 1;
        while (j < n && IDENT_PART.test(source[j])) j += 1;
        let word = source.slice(i, j);
        const known = KEYWORD_SET.has(word) || BUILTIN_SET.has(word);
        if (!known && source[j] === "." && j + 1 < n && IDENT_START.test(source[j + 1])) {
          let k = j + 1;
          while (k < n && IDENT_PART.test(source[k])) k += 1;
          word = source.slice(i, k);
          tokens.push({ type: "reference", value: word });
          i = k;
          continue;
        }
        const type = KEYWORD_SET.has(word) ? "keyword" : BUILTIN_SET.has(word) ? "builtin" : "ident";
        tokens.push({ type: type, value: word });
        i = j;
        continue;
      }

      if (source.startsWith("??", i)) {
        tokens.push({ type: "operator", value: "??" });
        i += 2;
        continue;
      }
      if (c === "=" || c === "+" || c === "&" || c === "|" || c === "!") {
        tokens.push({ type: "operator", value: c });
        i += 1;
        continue;
      }

      tokens.push({ type: "plain", value: c });
      i += 1;
    }

    return tokens;
  }

  function escapeHtml(value) {
    return value
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;");
  }

  /** Render tokens to markup; unused text stays plain so nothing is rewritten. */
  function toHtml(tokens) {
    let html = "";
    for (const token of tokens) {
      if (token.type === "plain") {
        html += escapeHtml(token.value);
      } else {
        html += '<span class="' + CLASS[token.type] + '">' + escapeHtml(token.value) + "</span>";
      }
    }
    return html;
  }

  /** Render tokens into a DocumentFragment for in-place DOM rewriting. */
  function renderTokens(tokens, doc) {
    const fragment = doc.createDocumentFragment();
    for (const token of tokens) {
      if (token.type === "plain") {
        fragment.appendChild(doc.createTextNode(token.value));
      } else {
        const span = doc.createElement("span");
        span.className = CLASS[token.type];
        span.textContent = token.value;
        fragment.appendChild(span);
      }
    }
    return fragment;
  }

  /** Pure classification: is this code block labelled as a Forkfile snippet? */
  function classify(labelText) {
    return typeof labelText === "string" && labelText.trim().toLowerCase() === "forkfile";
  }

  function isForkfileCode(code) {
    if (!code || typeof code.querySelector !== "function") return false;
    // Numbered code blocks (line anchors) keep Zensical's own markup untouched.
    if (code.querySelector(".codelineno, .linenos")) return false;
    const holder = code.parentElement && code.parentElement.parentElement;
    if (!holder || !holder.classList || !holder.classList.contains("highlight")) return false;
    const label = holder.querySelector("span.filename");
    return classify(label ? label.textContent : "");
  }

  /**
   * Highlight every published Forkfile block under `root`.
   * Idempotent, and a no-op (returning 0) when no Forkfile block matches.
   */
  function enhance(root) {
    if (!root || typeof root.querySelectorAll !== "function") return 0;
    const doc = root.ownerDocument || root;
    const codes = root.querySelectorAll("pre > code");
    const targets = [];
    for (let i = 0; i < codes.length; i += 1) {
      const code = codes[i];
      if (!code.hasAttribute("data-forkfile-highlighted") && isForkfileCode(code)) {
        targets.push(code);
      }
    }

    let highlighted = 0;
    for (const code of targets) {
      code.setAttribute("data-forkfile-highlighted", "");
      const walker = doc.createTreeWalker(code, 4);
      const textNodes = [];
      let node = walker.nextNode();
      while (node) {
        textNodes.push(node);
        node = walker.nextNode();
      }

      let changed = false;
      for (const textNode of textNodes) {
        const tokens = tokenize(textNode.nodeValue);
        if (!tokens.some((token) => token.type !== "plain")) continue;
        textNode.parentNode.replaceChild(renderTokens(tokens, doc), textNode);
        changed = true;
      }
      if (changed) highlighted += 1;
    }
    return highlighted;
  }

  function boot(scope) {
    if (typeof document === "undefined") return;
    const run = () => {
      enhance(document);
    };
    if (document.readyState === "loading") {
      document.addEventListener("DOMContentLoaded", run, { once: true });
    } else {
      run();
    }
    // Material/Zensical instant navigation re-renders the article without a reload.
    if (scope && scope.document$ && typeof scope.document$.subscribe === "function") {
      scope.document$.subscribe(run);
    }
  }

  const api = { tokenize, toHtml, renderTokens, classify, isForkfileCode, enhance, CLASS };

  if (typeof module === "object" && module.exports) {
    module.exports = api;
    return;
  }
  if (global) global.ForkfileHighlight = api;
  boot(global);
})(typeof globalThis !== "undefined" ? globalThis : this);
