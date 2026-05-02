#!/usr/bin/env node
// KaTeX math sidecar — reads JSON lines from stdin, writes rendered text to stdout.
// Each input line: {"id": N, "latex": "...", "display": bool}
// Each output line: {"id": N, "text": "..."}
//
// Stays alive for the whole session (no per-expression Node startup cost).

const katex = require("katex");
const readline = require("readline");

const rl = readline.createInterface({ input: process.stdin, terminal: false });

rl.on("line", (line) => {
  let req;
  try {
    req = JSON.parse(line);
  } catch {
    return;
  }

  let text;
  try {
    const html = katex.renderToString(req.latex, {
      displayMode: !!req.display,
      throwOnError: false,
      output: "html",
    });
    text = htmlToText(html);
  } catch {
    text = req.latex; // fallback: raw LaTeX
  }

  process.stdout.write(JSON.stringify({ id: req.id, text }) + "\n");
});

// Strip HTML tags but keep text content — KaTeX embeds Unicode math chars
// directly in the text nodes, so stripping tags gives readable output.
function htmlToText(html) {
  // Remove <annotation> (MathML source) and <svg> blocks entirely.
  html = html.replace(/<annotation[^>]*>[\s\S]*?<\/annotation>/g, "");
  html = html.replace(/<svg[^>]*>[\s\S]*?<\/svg>/g, "");
  // Strip all remaining tags.
  const text = html.replace(/<[^>]+>/g, "");
  // Decode common HTML entities.
  return text
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&nbsp;/g, " ")
    .replace(/&#x([0-9a-fA-F]+);/g, (_, h) =>
      String.fromCodePoint(parseInt(h, 16))
    )
    .replace(/&#(\d+);/g, (_, d) => String.fromCodePoint(parseInt(d, 10)))
    .replace(/\s+/g, " ")
    .trim();
}
