/**
 * Forkfile highlighter tests: `bun test tools/tests/forkfile-highlight.test.js`.
 *
 * The published fences are read from the real docs on disk, never copied into
 * the test. `enhance()` is exercised against a small in-memory DOM whose nodes
 * really implement `className`/`textContent`/`querySelectorAll`, so the
 * label-classification path runs for real rather than through a shim.
 */
import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import highlighter from "../../docs/javascripts/forkfile-highlight.js";

const { tokenize, toHtml, classify, enhance } = highlighter;

const DOC_FILES = [
  "../../docs/index.md",
  "../../docs/getting-started.md",
  "../../docs/forkfile.md",
  "../../docs/reference/forkfile.md",
  "../../README.md",
];

const TOKEN_CLASSES = [
  "keyword",
  "builtin",
  "string",
  "number",
  "comment",
  "operator",
  "reference",
  "ident",
];

function read(relative) {
  return readFileSync(new URL(relative, import.meta.url), "utf8");
}

function publishedFences() {
  const fences = [];
  for (const file of DOC_FILES) {
    const pattern = /^```text title="Forkfile"\n([\s\S]*?)^```$/gm;
    for (const match of read(file).matchAll(pattern)) {
      fences.push({ file, source: match[1] });
    }
  }
  return fences;
}

function plainText(html) {
  return html
    .replace(/<[^>]*>/g, "")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&amp;/g, "&");
}

/* ---------------------------------------------------------------------------
 * Minimal real DOM: elements own attrs, classes, children, and selector
 * queries; text nodes own `nodeValue`. Just enough for `enhance()`.
 * ------------------------------------------------------------------------- */

function parseCompound(token) {
  const [head, ...rest] = token.split(".");
  return { tag: head || null, classes: rest };
}

function parseSelector(selector) {
  return selector.split(",").map((part) => {
    const steps = [];
    for (const match of part.matchAll(/(>)?\s*([^\s>]+)/g)) {
      steps.push({ child: Boolean(match[1]), compound: parseCompound(match[2]) });
    }
    return steps;
  });
}

function matchesCompound(node, compound) {
  if (node.kind !== "element") return false;
  if (compound.tag && node.tag !== compound.tag) return false;
  return compound.classes.every((name) => node.classes.includes(name));
}

function matchesChain(node, steps) {
  if (!matchesCompound(node, steps[steps.length - 1].compound)) return false;
  let current = node;
  for (let i = steps.length - 2; i >= 0; i -= 1) {
    const step = steps[i];
    if (steps[i + 1].child) {
      current = current.parentElement;
      if (!current || !matchesCompound(current, step.compound)) return false;
    } else {
      let ancestor = current.parentElement;
      while (ancestor && !matchesCompound(ancestor, step.compound)) {
        ancestor = ancestor.parentElement;
      }
      if (!ancestor) return false;
      current = ancestor;
    }
  }
  return true;
}

function textNode(value) {
  return { kind: "text", nodeValue: value, parentElement: null, parentNode: null };
}

function element(tag, attrs = {}, children = []) {
  const node = {
    kind: "element",
    tag,
    attrs: { ...attrs },
    classes: String(attrs.class || "").split(/\s+/).filter(Boolean),
    children: [],
    parentElement: null,
    parentNode: null,
  };

  Object.defineProperty(node, "className", {
    get: () => node.classes.join(" "),
    set: (value) => {
      node.classes = String(value).split(/\s+/).filter(Boolean);
    },
  });

  Object.defineProperty(node, "textContent", {
    get: () =>
      node.children
        .map((child) => (child.kind === "text" ? child.nodeValue : child.textContent))
        .join(""),
    set: (value) => {
      node.children = [];
      node.appendChild(textNode(String(value)));
    },
  });

  node.classList = { contains: (name) => node.classes.includes(name) };
  node.hasAttribute = (name) => Object.hasOwn(node.attrs, name);
  node.getAttribute = (name) => (Object.hasOwn(node.attrs, name) ? node.attrs[name] : null);
  node.setAttribute = (name, value) => {
    node.attrs[name] = String(value);
  };
  node.appendChild = (child) => {
    child.parentElement = node;
    child.parentNode = node;
    node.children.push(child);
    return child;
  };
  node.replaceChild = (replacement, target) => {
    const index = node.children.indexOf(target);
    if (index === -1) throw new Error("replaceChild: target is not a child");
    const incoming = replacement.kind === "fragment" ? replacement.children : [replacement];
    for (const child of incoming) {
      child.parentElement = node;
      child.parentNode = node;
    }
    node.children.splice(index, 1, ...incoming);
    return target;
  };
  node.querySelectorAll = (selector) => {
    const chains = parseSelector(selector);
    const found = [];
    (function walk(current) {
      for (const child of current.children) {
        if (child.kind !== "element") continue;
        if (chains.some((steps) => matchesChain(child, steps))) found.push(child);
        walk(child);
      }
    })(node);
    return found;
  };
  node.querySelector = (selector) => node.querySelectorAll(selector)[0] || null;

  for (const child of children) node.appendChild(child);
  return node;
}

function fragment() {
  const node = { kind: "fragment", children: [] };
  node.appendChild = (child) => {
    node.children.push(child);
    return child;
  };
  return node;
}

function documentFor(root) {
  const doc = {
    createElement: (tag) => element(tag),
    createTextNode: (value) => textNode(value),
    createDocumentFragment: () => fragment(),
    createTreeWalker: (scope) => {
      const text = [];
      (function collect(current) {
        for (const child of current.children) {
          if (child.kind === "text") text.push(child);
          else collect(child);
        }
      })(scope);
      let index = 0;
      return { nextNode: () => (index < text.length ? text[index++] : null) };
    },
  };
  root.ownerDocument = doc;
  return doc;
}

/** Zensical's rendered shape: div.highlight > span.filename + pre > code. */
function codeBlock(label, codeChildren, codeAttrs = {}) {
  const code = element("code", { class: "language-text", ...codeAttrs }, codeChildren);
  const pre = element("pre", {}, [code]);
  const parts = label === null ? [pre] : [element("span", { class: "filename" }, [textNode(label)]), pre];
  const holder = element("div", { class: "highlight" }, parts);
  return { holder, code };
}

describe("published Forkfile fences", () => {
  const fences = publishedFences();

  test("the docs actually declare Forkfile fences", () => {
    expect(fences.length).toBeGreaterThanOrEqual(8);
    expect(new Set(fences.map((fence) => fence.file)).size).toBe(DOC_FILES.length);
  });

  test("every fence tokenizes back to its exact source and gains spans", () => {
    const failures = fences
      .map(({ file, source }) => {
        const tokens = tokenize(source);
        const html = toHtml(tokens);
        return {
          file,
          rejoined: tokens.map((token) => token.value).join("") === source,
          textPreserved: plainText(html) === source,
          highlighted: html.includes('class="ff-'),
        };
      })
      .filter((result) => !result.rejoined || !result.textPreserved || !result.highlighted);
    expect(failures).toEqual([]);
  });
});

describe("tokenizer", () => {
  test("classifies statements, ids, values, and references", () => {
    const source = [
      "# top-level comment",
      "project demo",
      "env app {",
      "  TOKEN = secret(TOKEN) ?? random_base64(32)",
      '  URL   = "http://" + app_db.url',
      "  run sh -c 'echo hi' --port $port",
      "}",
      "",
    ].join("\n");

    const byType = new Map();
    for (const token of tokenize(source)) {
      const values = byType.get(token.type) || [];
      values.push(token.value);
      byType.set(token.type, values);
    }

    expect(byType.get("keyword")).toEqual(expect.arrayContaining(["project", "env", "run"]));
    expect(byType.get("builtin")).toEqual(expect.arrayContaining(["secret", "random_base64"]));
    expect(byType.get("ident")).toEqual(expect.arrayContaining(["demo", "app", "TOKEN"]));
    expect(byType.get("reference")).toEqual(
      expect.arrayContaining(["app_db.url", "$port"]),
    );
    expect(byType.get("string")).toEqual(expect.arrayContaining(['"http://"', "'echo hi'"]));
    expect(byType.get("number")).toContain("32");
    expect(byType.get("operator")).toEqual(expect.arrayContaining(["??", "=", "+"]));
    expect(byType.get("comment")).toEqual(["# top-level comment"]);
  });

  test("preserves awkward text byte-for-byte", () => {
    const tricky = [
      "a # trailing comment",
      'run "unterminated',
      "values = a ?? b & c | d ! e = f + g",
      "path ./nested/dir and mail.* route",
      "",
    ].join("\n");
    expect(tokenize(tricky).map((token) => token.value).join("")).toBe(tricky);
  });

  test("handles empty and comment-only sources", () => {
    expect(tokenize("")).toEqual([]);
    expect(toHtml([])).toBe("");
    expect(tokenize("# just a comment")).toEqual([
      { type: "comment", value: "# just a comment" },
    ]);
  });
});

describe("classify", () => {
  test("accepts only the Forkfile title label", () => {
    expect(classify("Forkfile")).toBe(true);
    expect(classify(" forkfile ")).toBe(true);
    expect(classify("bash")).toBe(false);
    expect(classify("error: unknown feature")).toBe(false);
    expect(classify("")).toBe(false);
    expect(classify(null)).toBe(false);
  });
});

describe("enhance", () => {
  test("rewrites the Forkfile-labelled block only, then stays idempotent", () => {
    const snippet = "project demo\nenv app {\n  TOKEN = secret(TOKEN) ?? uuid4()\n}\n";
    const diagnostic = "error: unknown feature `nope`\n";

    const forkfile = codeBlock("Forkfile", [textNode(snippet)]);
    const untitled = codeBlock(null, [textNode(diagnostic)]);
    const numbered = codeBlock("Forkfile", [
      element("span", { class: "codelineno" }, [textNode("1")]),
      textNode(snippet),
    ]);

    const article = element("article", {}, [
      forkfile.holder,
      untitled.holder,
      numbered.holder,
    ]);
    const doc = documentFor(article);

    // The classifier picks the titled block and never invents an error label.
    expect(classify(forkfile.holder.querySelector("span.filename").textContent)).toBe(true);
    expect(untitled.holder.querySelector("span.filename")).toBeNull();
    expect(doc).toBe(article.ownerDocument);

    expect(enhance(article)).toBe(1);

    expect(forkfile.code.querySelectorAll(".ff-keyword").map((span) => span.textContent)).toEqual([
      "project",
      "env",
    ]);
    expect(forkfile.code.querySelectorAll(".ff-builtin").map((span) => span.textContent)).toEqual([
      "secret",
      "uuid4",
    ]);
    expect(forkfile.code.textContent).toBe(snippet);
    expect(forkfile.code.className).toBe("language-text");
    expect(forkfile.code.hasAttribute("data-forkfile-highlighted")).toBe(true);

    expect(untitled.code.querySelectorAll(".ff-keyword")).toEqual([]);
    expect(untitled.code.textContent).toBe(diagnostic);
    expect(untitled.code.hasAttribute("data-forkfile-highlighted")).toBe(false);

    // Line-numbered blocks keep Zensical's anchors, so they are never rewritten.
    expect(numbered.code.querySelectorAll(".ff-keyword")).toEqual([]);
    expect(numbered.code.hasAttribute("data-forkfile-highlighted")).toBe(false);

    expect(enhance(article)).toBe(0);
  });

  test("ignores pages without a code block", () => {
    expect(enhance(null)).toBe(0);
    const empty = element("article", {});
    documentFor(empty);
    expect(enhance(empty)).toBe(0);
  });
});

describe("no-match no-op", () => {
  test("published docs never use an unlabelable ```forkfile fence", () => {
    const offenders = DOC_FILES.filter((file) => /^```forkfile$/m.test(read(file)));
    expect(offenders).toEqual([]);
  });

  test("diagnostic error blocks stay untitled, so they are never rewritten", () => {
    const fences = [
      ...read("../../docs/forkfile.md").matchAll(
        /^```text(?: title="Forkfile")?\n([\s\S]*?)^```$/gm,
      ),
    ];
    const diagnostics = fences.filter((match) => match[1].startsWith("error:"));
    expect(diagnostics.length).toBeGreaterThan(0);
    expect(diagnostics.filter((match) => match[0].includes('title="Forkfile"'))).toEqual([]);
  });
});

describe("stylesheet", () => {
  const css = read("../../docs/stylesheets/extra.css");

  function schemeVars(scheme) {
    const block = new RegExp(
      `body\\[data-md-color-scheme="${scheme}"\\]\\s*\\{([^}]*)\\}`,
    ).exec(css);
    expect(block).not.toBeNull();
    const vars = {};
    for (const match of block[1].matchAll(/--ff-([a-z]+):\s*(#[0-9a-fA-F]{6})/g)) {
      vars[match[1]] = match[2];
    }
    return vars;
  }

  /** Resolve the background the stylesheet actually paints on the code block. */
  function paintedBackground(scheme) {
    const rule = new RegExp(
      `body\\[data-md-color-scheme="${scheme}"\\]\\s+\\.md-typeset\\s+pre\\s*>\\s*code\\s*\\{([^}]*)\\}`,
    ).exec(css);
    expect(rule).not.toBeNull();
    expect(rule[1]).toMatch(/background-color:\s*var\(--ff-bg\)/);
    const bg = schemeVars(scheme).bg;
    expect(bg).toBeDefined();
    return bg;
  }

  function luminance(hex) {
    const channels = [1, 3, 5].map((offset) => {
      const value = parseInt(hex.slice(offset, offset + 2), 16) / 255;
      return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
    });
    return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2];
  }

  function contrast(a, b) {
    const first = luminance(a);
    const second = luminance(b);
    const [high, low] = first > second ? [first, second] : [second, first];
    return (high + 0.05) / (low + 0.05);
  }

  test("every token class meets WCAG AA against the painted background", () => {
    const failures = [];
    for (const scheme of ["default", "slate"]) {
      const vars = schemeVars(scheme);
      const bg = paintedBackground(scheme);
      expect(bg).toBe(vars.bg);
      for (const name of TOKEN_CLASSES) {
        expect(vars[name]).toBeDefined();
        const ratio = contrast(vars[name], bg);
        if (ratio < 4.5) failures.push({ scheme, name, ratio: Number(ratio.toFixed(2)) });
      }
    }
    expect(failures).toEqual([]);
  });

  test("every token class carries a weight, style, or underline cue", () => {
    const missing = TOKEN_CLASSES.filter((name) => {
      const rule = new RegExp(`\\.md-typeset \\.ff-${name} \\{([^}]*)\\}`).exec(css);
      return !rule || !/font-weight|font-style|text-decoration/.test(rule[1]);
    });
    expect(missing).toEqual([]);
  });
});
