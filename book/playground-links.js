// Links from the book's examples to the web playground, for every code block that names its
// sample graph:
//
//   ```rhai,graph=modern          (or empty, large, tree): every statement of the block gets a
//                                  run button in the left margin; a block with one statement
//                                  gets "Run in the playground" under it
//   ```rhai,graph=modern,script   the whole block is one script: one link under it
//
// A statement starts at a line without indentation that does not continue the previous one (it
// does not start with `.`, `)`, `]` or `}`, and every bracket before it is closed); comment lines
// before a statement belong to it. The test `book_playground_examples` of graphersal-session
// splits blocks the same way and runs every statement on its sample.
//
// (The marker is not `playground`: mdBook's book.js turns every `.playground` element into a
// Rust playground with a run button that posts to play.rust-lang.org.)
//
// A link opens https://play.graphersal.dev/ with the text as the query (`#/play?graph=..&q=..&run=1`,
// the format of playground/js/link.js). For a local preview, point it at another playground in the
// browser console: localStorage.setItem('graphersal.book.playground', 'http://localhost:8000/')
(function () {
  'use strict';
  var DEFAULT_BASE = 'https://play.graphersal.dev/';
  var GRAPHS = ['modern', 'empty', 'large', 'tree'];

  function base() {
    try {
      return localStorage.getItem('graphersal.book.playground') || DEFAULT_BASE;
    } catch (e) {
      return DEFAULT_BASE;
    }
  }

  function graphOf(code) {
    for (var i = 0; i < code.classList.length; i++) {
      var c = code.classList[i];
      if (c.indexOf('graph=') === 0) {
        var g = c.slice('graph='.length);
        return GRAPHS.indexOf(g) >= 0 ? g : null;
      }
    }
    return null;
  }

  // The GQL text of a statement that is exactly one `g.gql(<string literal>)` call (optionally
  // followed by `.to_list()`, `;` and a `//` comment), or null: a call with parameters, a computed
  // text, an interpolating backtick string or a longer traversal stays a Rhai script. Such a
  // statement opens as a GQL tab (`lang=gql`, playground/js/link.js); the playground's tab runs
  // the same call, so the result is the same.
  function gqlOf(text) {
    var m = /^\s*g\s*\.\s*gql\s*\(\s*/.exec(text);
    if (!m) return null;
    var i = m[0].length, q = text[i], value = '';
    if (q === '`') {
      var end = text.indexOf('`', i + 1);
      if (end < 0) return null;
      value = text.slice(i + 1, end);
      if (value.indexOf('${') >= 0) return null;
      i = end + 1;
    } else if (q === '"') {
      var closed = false;
      for (i++; i < text.length; i++) {
        var c = text[i];
        if (c === '"') { closed = true; i++; break; }
        if (c === '\\') {
          var e = text[++i], map = { n: '\n', t: '\t', r: '\r', '0': '\0', '\\': '\\', '"': '"', "'": "'" };
          if (!(e in map)) return null;
          value += map[e];
        } else value += c;
      }
      if (!closed) return null;
    } else return null;
    var rest = text.slice(i);
    if (!/^\s*\)(\s*\.\s*to_list\s*\(\s*\))?\s*;?\s*(\/\/[^\n]*)?\s*$/.test(rest)) return null;
    return value.trim() ? value : null;
  }

  function href(graph, script) {
    var gql = gqlOf(script);
    if (gql !== null) {
      return base() + '#/play?graph=' + encodeURIComponent(graph) +
        '&q=' + encodeURIComponent(gql) + '&lang=gql&run=1';
    }
    return base() + '#/play?graph=' + encodeURIComponent(graph) +
      '&q=' + encodeURIComponent(script) + '&run=1';
  }

  // How much a line opens minus what it closes, outside strings and comments.
  function depthDelta(line) {
    var depth = 0, quote = null;
    for (var i = 0; i < line.length; i++) {
      var c = line[i];
      if (quote) {
        if (c === '\\') i++;
        else if (c === quote) quote = null;
      } else if (c === '"' || c === '`') quote = c;
      else if (c === '/' && line[i + 1] === '/') break;
      else if (c === '(' || c === '[' || c === '{') depth++;
      else if (c === ')' || c === ']' || c === '}') depth--;
    }
    return depth;
  }

  function isComment(line) {
    return /^\s*\/\//.test(line);
  }

  // [{line: index of the first line, text}], like `statements` of the test.
  function statements(lines) {
    var out = [], depth = 0;
    for (var n = 0; n < lines.length; n++) {
      var line = lines[n];
      if (!line.trim()) continue;
      var continues = /^\s/.test(line) || /^\s*[.)\]}]/.test(line);
      var last = out[out.length - 1];
      var onlyComments = last && last.lines.every(isComment);
      if (!last || (depth === 0 && !continues && !onlyComments)) out.push({ line: n, lines: [] });
      out[out.length - 1].lines.push(line);
      depth += depthDelta(line);
    }
    return out
      .filter(function (s) { return !s.lines.every(isComment); })
      .map(function (s) { return { line: s.line, text: s.lines.join('\n') }; });
  }

  function linkUnder(pre, graph, script) {
    var p = document.createElement('p');
    p.className = 'playground-link';
    var a = document.createElement('a');
    a.href = href(graph, script);
    a.target = '_blank';
    a.rel = 'noopener';
    a.textContent = '▶ Run in the playground';
    a.title = 'Opens this query on the ' + graph + ' sample graph in the web playground';
    p.appendChild(a);
    pre.insertAdjacentElement('afterend', p);
  }

  // The top of line `n` of a code block, relative to its <pre>: measured with a Range at the
  // first character of that line, so fonts, padding and line height do not matter.
  function lineTops(pre, code, wanted) {
    var tops = {}, line = 0, need = wanted.slice().sort(function (a, b) { return a - b; });
    var walker = document.createTreeWalker(code, NodeFilter.SHOW_TEXT);
    var preTop = pre.getBoundingClientRect().top;
    var atLineStart = true, node;
    while ((node = walker.nextNode()) && need.length) {
      var t = node.nodeValue;
      for (var i = 0; i < t.length && need.length; i++) {
        if (atLineStart && line === need[0]) {
          var r = document.createRange();
          r.setStart(node, i);
          r.setEnd(node, Math.min(i + 1, t.length));
          var rect = r.getBoundingClientRect();
          tops[line] = { top: rect.top - preTop + pre.scrollTop, height: rect.height };
          need.shift();
        }
        atLineStart = t[i] === '\n';
        if (atLineStart) line++;
      }
    }
    return tops;
  }

  var placed = [];

  // Puts every margin button on the first line of its statement (again after the web fonts load
  // and on a resize: both change the line height).
  function place() {
    placed.forEach(function (p) {
      var tops = lineTops(p.pre, p.code, p.buttons.map(function (b) { return b.line; }));
      p.buttons.forEach(function (b) {
        var at = tops[b.line];
        if (!at) return;
        b.el.style.top = at.top + 'px';
        b.el.style.height = at.height + 'px';
        b.el.style.lineHeight = at.height + 'px';
      });
    });
  }

  function marginButtons(pre, code, graph, stmts) {
    pre.classList.add('playground-runs');
    var buttons = stmts.map(function (s, i) {
      var a = document.createElement('a');
      a.className = 'playground-run';
      a.href = href(graph, s.text);
      a.target = '_blank';
      a.rel = 'noopener';
      a.textContent = '▶';
      a.title = 'Run this query in the playground (' + graph + ' graph)';
      a.setAttribute('aria-label', 'Run query ' + (i + 1) + ' of this block in the playground');
      pre.appendChild(a);
      return { line: s.line, el: a };
    });
    placed.push({ pre: pre, code: code, buttons: buttons });
  }

  function addLinks() {
    var blocks = document.querySelectorAll('pre > code.language-rhai');
    for (var i = 0; i < blocks.length; i++) {
      var code = blocks[i];
      var graph = graphOf(code);
      if (!graph) continue;
      var pre = code.parentElement;
      if (pre.dataset.playground) continue;
      pre.dataset.playground = '1';
      var text = code.textContent.replace(/\s+$/, '');
      var stmts = code.classList.contains('script') ? [] : statements(text.split('\n'));
      if (stmts.length > 1) marginButtons(pre, code, graph, stmts);
      else linkUnder(pre, graph, stmts.length === 1 ? stmts[0].text : text);
    }
  }

  // Links written into a page (the generated step reference) point at the public playground;
  // a local preview redirects them like the generated ones.
  function retarget() {
    var b = base();
    if (b === DEFAULT_BASE) return;
    var links = document.querySelectorAll('a[href^="' + DEFAULT_BASE + '"]');
    for (var i = 0; i < links.length; i++) links[i].href = b + links[i].getAttribute('href').slice(DEFAULT_BASE.length);
  }

  function start() {
    retarget();
    addLinks();
    place();
    if (document.fonts && document.fonts.ready) document.fonts.ready.then(place);
    window.addEventListener('load', place);
    window.addEventListener('resize', place);
  }

  // For the Node test of playground/tests/book-links.test.mjs.
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = { gqlOf: gqlOf };
    return;
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start);
  else start();
})();
