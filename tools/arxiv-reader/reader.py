#!/usr/bin/env python3
"""
arxiv-reader prototype — Pandoc + KaTeX pipeline.

Usage:
  python3 reader.py <arxiv-id-or-url>
  python3 reader.py --local <path-to-tex-file>
"""

import json
import os
import re
import subprocess
import sys
import tarfile
import tempfile
import io
from pathlib import Path

try:
    from rich.console import Console
    from rich.rule import Rule
    from rich.padding import Padding
    from rich.table import Table as RichTable
    USE_RICH = True
except ImportError:
    USE_RICH = False

try:
    import requests
    HAS_REQUESTS = True
except ImportError:
    HAS_REQUESTS = False

SCRIPT_DIR = Path(__file__).parent
WIDTH      = 88


# ── Math rendering ────────────────────────────────────────────────────────────

_SYMBOLS = {
    # Greek lowercase
    'alpha': 'α', 'beta': 'β', 'gamma': 'γ', 'delta': 'δ',
    'epsilon': 'ε', 'varepsilon': 'ε', 'zeta': 'ζ', 'eta': 'η',
    'theta': 'θ', 'vartheta': 'ϑ', 'iota': 'ι', 'kappa': 'κ',
    'lambda': 'λ', 'mu': 'μ', 'nu': 'ν', 'xi': 'ξ',
    'pi': 'π', 'varpi': 'ϖ', 'rho': 'ρ', 'varrho': 'ϱ',
    'sigma': 'σ', 'varsigma': 'ς', 'tau': 'τ', 'upsilon': 'υ',
    'phi': 'φ', 'varphi': 'φ', 'chi': 'χ', 'psi': 'ψ', 'omega': 'ω',
    # Greek uppercase
    'Gamma': 'Γ', 'Delta': 'Δ', 'Theta': 'Θ', 'Lambda': 'Λ',
    'Xi': 'Ξ', 'Pi': 'Π', 'Sigma': 'Σ', 'Upsilon': 'Υ',
    'Phi': 'Φ', 'Psi': 'Ψ', 'Omega': 'Ω',
    # Operators
    'times': '×', 'div': '÷', 'pm': '±', 'mp': '∓',
    'cdot': '·', 'cdotp': '·', 'cdots': '⋯', 'ldots': '…',
    'vdots': '⋮', 'ddots': '⋱',
    # Relations
    'leq': '≤', 'le': '≤', 'geq': '≥', 'ge': '≥',
    'neq': '≠', 'ne': '≠', 'approx': '≈', 'equiv': '≡',
    'sim': '∼', 'simeq': '≃', 'cong': '≅', 'propto': '∝',
    'll': '≪', 'gg': '≫',
    # Sets
    'in': '∈', 'notin': '∉', 'subset': '⊂', 'supset': '⊃',
    'subseteq': '⊆', 'supseteq': '⊇', 'cup': '∪', 'cap': '∩',
    'emptyset': '∅', 'varnothing': '∅',
    # Logic & arrows
    'forall': '∀', 'exists': '∃', 'nexists': '∄',
    'neg': '¬', 'lnot': '¬', 'land': '∧', 'lor': '∨',
    'Rightarrow': '⇒', 'Leftarrow': '⇐', 'Leftrightarrow': '⇔',
    'rightarrow': '→', 'leftarrow': '←', 'leftrightarrow': '↔',
    'to': '→', 'gets': '←', 'mapsto': '↦',
    'uparrow': '↑', 'downarrow': '↓', 'updownarrow': '↕',
    'Uparrow': '⇑', 'Downarrow': '⇓',
    # Calculus
    'partial': '∂', 'nabla': '∇', 'infty': '∞',
    'int': '∫', 'iint': '∬', 'iiint': '∭',
    'oint': '∮', 'sum': '∑', 'prod': '∏',
    # Misc
    'circ': '∘', 'bullet': '•', 'star': '⋆',
    'dagger': '†', 'ddagger': '‡',
    'langle': '⟨', 'rangle': '⟩',
    'ell': 'ℓ', 'hbar': 'ℏ',
    'top': '⊤', 'bot': '⊥', 'perp': '⊥',
    'oplus': '⊕', 'ominus': '⊖', 'otimes': '⊗', 'oslash': '⊘',
    'mid': '∣', 'nmid': '∤',
    'lfloor': '⌊', 'rfloor': '⌋', 'lceil': '⌈', 'rceil': '⌉',
    'vert': '|', 'Vert': '‖',
    'because': '∵', 'therefore': '∴',
    # Operator names — strip backslash, keep the word
    'min': 'min', 'max': 'max', 'log': 'log', 'ln': 'ln', 'exp': 'exp',
    'sin': 'sin', 'cos': 'cos', 'tan': 'tan', 'arcsin': 'arcsin',
    'arccos': 'arccos', 'arctan': 'arctan', 'sinh': 'sinh', 'cosh': 'cosh',
    'tanh': 'tanh', 'det': 'det', 'tr': 'tr', 'arg': 'arg',
    'sup': 'sup', 'inf': 'inf', 'lim': 'lim', 'dim': 'dim',
    'ker': 'ker', 'rank': 'rank', 'softmax': 'softmax',
    # Punctuation
    'quad': ' ', 'qquad': '  ',
}

_BB = {'R': 'ℝ', 'Z': 'ℤ', 'N': 'ℕ', 'Q': 'ℚ', 'C': 'ℂ',
       'E': '𝔼', 'P': 'ℙ', 'F': '𝔽', 'H': 'ℍ'}

_SUP = {
    '0': '⁰', '1': '¹', '2': '²', '3': '³', '4': '⁴',
    '5': '⁵', '6': '⁶', '7': '⁷', '8': '⁸', '9': '⁹',
    '+': '⁺', '-': '⁻', '=': '⁼', '(': '⁽', ')': '⁾',
    'n': 'ⁿ', 'i': 'ⁱ', 'T': 'ᵀ',
}
_SUB = {
    '0': '₀', '1': '₁', '2': '₂', '3': '₃', '4': '₄',
    '5': '₅', '6': '₆', '7': '₇', '8': '₈', '9': '₉',
    '+': '₊', '-': '₋', '=': '₌', '(': '₍', ')': '₎',
    'a': 'ₐ', 'e': 'ₑ', 'o': 'ₒ', 'x': 'ₓ', 'n': 'ₙ',
    'i': 'ᵢ', 'j': 'ⱼ', 'k': 'ₖ', 'l': 'ₗ', 'm': 'ₘ',
    'p': 'ₚ', 'r': 'ᵣ', 's': 'ₛ', 't': 'ₜ', 'u': 'ᵤ',
    'v': 'ᵥ', 'h': 'ₕ',
}


def _to_sup(s: str) -> str:
    if all(c in _SUP for c in s):
        return ''.join(_SUP[c] for c in s)
    return f'^({s})' if len(s) > 1 else f'^{s}'


def _to_sub(s: str) -> str:
    if all(c in _SUB for c in s):
        return ''.join(_SUB[c] for c in s)
    return f'_({s})' if len(s) > 1 else f'_{s}'


# Unwrap-only font/style commands — keep inner text, discard the command
_UNWRAP = ('text', 'textrm', 'textsf', 'textit', 'textbf',
           'mathrm', 'mathit', 'mathbf', 'mathsf', 'mathtt',
           'boldsymbol', 'bm', 'operatorname', 'mathnormal',
           'mathcal', 'hat', 'tilde', 'bar', 'vec',
           'widehat', 'overline', 'underline', 'widetilde',
           'underbrace', 'overbrace')


def render_math(latex: str, display: bool = False) -> str:
    """Convert LaTeX math to readable Unicode using symbol table + pattern matching."""
    s = latex.strip()

    # Pre-processing: strip environment wrappers and normalise escapes
    s = re.sub(r'\\(?:begin|end)\s*\{[^{}]*\}', '', s)  # \begin{eq} / \end{eq}
    s = re.sub(r'\\_', '_', s)   # \_ → literal underscore (not a subscript command)
    s = re.sub(r'\\,|\\;|\\!|\\:', ' ', s)  # spacing commands → space

    for _ in range(10):
        prev = s

        # Size modifiers before brackets — discard
        s = re.sub(r'\\(?:left|right|[Bb]ig[lr]?)\s*', '', s)

        # \mathbb{X}
        s = re.sub(r'\\mathbb\s*\{([A-Z])\}', lambda m: _BB.get(m.group(1), m.group(1)), s)

        # Unwrap font/style commands
        for cmd in _UNWRAP:
            s = re.sub(r'\\' + cmd + r'\s*\{([^{}]*)\}', r'\1', s)

        # \frac{a}{b}
        s = re.sub(r'\\(?:tfrac|dfrac|frac)\s*\{([^{}]*)\}\s*\{([^{}]*)\}',
                   r'\1/\2', s)

        # \sqrt[n]{x}
        s = re.sub(r'\\sqrt\s*\[([^\]]*)\]\s*\{([^{}]*)\}',
                   lambda m: f'{_to_sup(m.group(1))}√{m.group(2)}', s)
        # \sqrt{x}
        s = re.sub(r'\\sqrt\s*\{([^{}]*)\}',
                   lambda m: (f'√{m.group(1)}' if len(m.group(1).strip()) == 1
                              else f'√({m.group(1)})'), s)

        # ^{...}  and  _{...}  (non-nested content)
        s = re.sub(r'\^\{([^{}]*)\}', lambda m: _to_sup(m.group(1).strip()), s)
        s = re.sub(r'_\{([^{}]*)\}',  lambda m: _to_sub(m.group(1).strip()), s)

        # ^x  and  _x  single char
        s = re.sub(r'\^([a-zA-Z0-9+\-])', lambda m: _to_sup(m.group(1)), s)
        s = re.sub(r'_([a-zA-Z0-9+\-])',  lambda m: _to_sub(m.group(1)), s)

        # \symbol → Unicode
        s = re.sub(r'\\([a-zA-Z]+)', lambda m: _SYMBOLS.get(m.group(1), m.group(0)), s)

        if s == prev:
            break

    # Strip any remaining bare braces after the loop converges
    s = s.replace('{', '').replace('}', '')
    return re.sub(r'\s+', ' ', s).strip()


# ── arXiv fetch ───────────────────────────────────────────────────────────────

def extract_id(arg: str) -> str | None:
    for prefix in ["arxiv.org/abs/", "arxiv.org/pdf/", "huggingface.co/papers/"]:
        if prefix in arg:
            rest = arg.split(prefix, 1)[1]
            cand = re.match(r"[\w.\-]+", rest)
            if cand:
                return strip_version(cand.group())
    cand = re.match(r"[\d]+\.[\d\w.\-]+", arg)
    if cand:
        return strip_version(cand.group())
    return None

def strip_version(id_: str) -> str:
    return re.sub(r"v\d+$", "", id_)

def fetch_source(arxiv_id: str) -> Path:
    if not HAS_REQUESTS:
        sys.exit("pip install requests")
    url = f"https://arxiv.org/e-print/{arxiv_id}"
    print(f"fetching {url} ...", file=sys.stderr)
    r = requests.get(url, timeout=30)
    r.raise_for_status()
    tmp = tempfile.mkdtemp(prefix=f"arxiv{arxiv_id.replace('.','')}_")
    try:
        with tarfile.open(fileobj=io.BytesIO(r.content)) as tf:
            tf.extractall(tmp)
    except tarfile.TarError:
        # plain gz .tex
        import gzip
        content = gzip.decompress(r.content).decode("utf-8", errors="replace")
        Path(tmp, "main.tex").write_text(content)
    return Path(tmp)

def find_root_tex(directory: Path) -> Path:
    for f in directory.rglob("*.tex"):
        if "\\begin{document}" in f.read_text(errors="replace"):
            return f
    # fallback: largest .tex file
    return max(directory.rglob("*.tex"), key=lambda f: f.stat().st_size)


# ── Pandoc conversion ─────────────────────────────────────────────────────────

def tex_to_pandoc_ast(tex_path: Path) -> dict:
    # Run pandoc with cwd = tex file directory so \input{} resolves correctly.
    result = subprocess.run(
        ["pandoc", "-f", "latex", "-t", "json", "--quiet", tex_path.name],
        capture_output=True, text=True,
        cwd=str(tex_path.parent),
    )
    if result.returncode != 0:
        print(f"pandoc warning: {result.stderr[:200]}", file=sys.stderr)
    return json.loads(result.stdout)


# ── AST walker ────────────────────────────────────────────────────────────────

def walk_inlines(inlines) -> str:
    parts = []
    for node in inlines:
        t = node.get("t", "")
        c = node.get("c", "")
        if t == "Str":
            parts.append(c)
        elif t == "Space":
            parts.append(" ")
        elif t == "SoftBreak":
            parts.append(" ")
        elif t == "LineBreak":
            parts.append("\n")
        elif t in ("Emph", "Strong", "Underline", "Strikeout", "SmallCaps"):
            parts.append(walk_inlines(c))
        elif t == "Code":
            parts.append(c[1])  # c = [attr, text]
        elif t == "Math":
            kind, latex = c
            display = kind.get("t", "") == "DisplayMath"
            parts.append(render_math(latex, display=display))
        elif t == "Link":
            # c = [attr, inlines, target]
            parts.append(walk_inlines(c[1]))
        elif t == "Quoted":
            q, inner = c
            q_char = "“" if q.get("t") == "DoubleQuote" else "‘"
            q_end  = "”" if q.get("t") == "DoubleQuote" else "’"
            parts.append(q_char + walk_inlines(inner) + q_end)
        elif t == "Cite":
            # c = [citations, inlines]
            parts.append(walk_inlines(c[1]))
        elif t == "RawInline":
            # skip raw LaTeX/HTML remnants
            pass
        elif t == "Note":
            pass  # footnotes — skip for now
    return "".join(parts)


class Block:
    pass

class Heading(Block):
    def __init__(self, level, text): self.level = level; self.text = text

class Para(Block):
    def __init__(self, text): self.text = text

class CodeBlock(Block):
    def __init__(self, lang, code): self.lang = lang; self.code = code

class BulletItem(Block):
    def __init__(self, depth, text): self.depth = depth; self.text = text

class OrderedItem(Block):
    def __init__(self, depth, n, text): self.depth = depth; self.n = n; self.text = text

class HRule(Block):
    pass

class TableBlock(Block):
    def __init__(self, caption, head_rows, rows):
        self.caption   = caption
        self.head_rows = head_rows  # list of rows, each row a list of str (colspan expanded)
        self.rows      = rows

class RawBlock(Block):
    pass


def walk_blocks(blocks, depth=0) -> list[Block]:
    out = []
    for node in blocks:
        t = node.get("t", "")
        c = node.get("c", "")
        if t == "Header":
            level, _attr, inlines = c
            out.append(Heading(level, walk_inlines(inlines)))
        elif t == "Para":
            text = walk_inlines(c)
            if text.strip():
                out.append(Para(text))
        elif t == "Plain":
            text = walk_inlines(c)
            if text.strip():
                out.append(Para(text))
        elif t == "CodeBlock":
            _attr, code = c
            lang = (_attr[1] or [""])[0] if _attr[1] else ""
            out.append(CodeBlock(lang, code))
        elif t == "BulletList":
            for item in c:
                sub = walk_blocks(item, depth + 1)
                for b in sub:
                    if isinstance(b, Para):
                        out.append(BulletItem(depth, b.text))
                    else:
                        out.append(b)
        elif t == "OrderedList":
            _attrs, items = c
            for i, item in enumerate(items, 1):
                sub = walk_blocks(item, depth + 1)
                for b in sub:
                    if isinstance(b, Para):
                        out.append(OrderedItem(depth, i, b.text))
                    else:
                        out.append(b)
        elif t == "BlockQuote":
            out.extend(walk_blocks(c, depth))
        elif t == "HorizontalRule":
            out.append(HRule())
        elif t == "Div":
            _attr, inner = c
            out.extend(walk_blocks(inner, depth))
        elif t == "Table":
            out.append(parse_table(c))
        elif t == "RawBlock":
            pass  # skip raw LaTeX remnants
        elif t == "Figure":
            # Pandoc 3.x: c = [attr, caption, [blocks]]
            # caption: {"t": "Caption", "c": [short_or_null, [blocks]]}
            try:
                cap_struct = c[1] if isinstance(c, list) else {}
                cap_blocks = (cap_struct.get("c") or [None, []])[1] if isinstance(cap_struct, dict) else []
                cap = " ".join(
                    walk_inlines(b.get("c", []))
                    for b in (cap_blocks or [])
                    if b.get("t") in ("Para", "Plain")
                )
                if cap:
                    out.append(Para(f"[Figure: {cap}]"))
            except Exception:
                pass
    return out


def cell_text(cell) -> str:
    """Extract plain text from a Pandoc 3.x cell: [attr, alignment, rowspan, colspan, [blocks]]."""
    if not isinstance(cell, list) or len(cell) < 5:
        return ""
    return " ".join(
        walk_inlines(b.get("c", []))
        for b in (cell[4] or [])
        if isinstance(b, dict) and b.get("t") in ("Para", "Plain")
    )

def cell_colspan(cell) -> int:
    """Return the colspan of a Pandoc cell (position 3 in the cell list)."""
    if isinstance(cell, list) and len(cell) >= 4 and isinstance(cell[3], int):
        return max(1, cell[3])
    return 1

def expand_row(raw_row) -> list[str]:
    """Turn a Pandoc [row_attr, [cells]] into a flat list of strings, colspan expanded."""
    cells = raw_row[1] if isinstance(raw_row, list) and len(raw_row) >= 2 else []
    out = []
    for cell in cells:
        text = cell_text(cell)
        span = cell_colspan(cell)
        out.append(text)
        out.extend([""] * (span - 1))  # blank placeholders for spanned columns
    return out

def parse_table(c) -> TableBlock:
    # Pandoc 3.x: c = [attr, caption, colspec, head, [body, ...], foot]
    try:
        cap_raw  = c[1]
        cap_blks = (cap_raw[1] if isinstance(cap_raw, list) and len(cap_raw) > 1 else []) or []
        caption  = " ".join(
            walk_inlines(b.get("c", []))
            for b in cap_blks
            if isinstance(b, dict) and b.get("t") in ("Para", "Plain")
        )

        # head = [attr, [row, ...]] — multiple header rows possible
        thead     = c[3]
        raw_hrows = (thead[1] if isinstance(thead, list) and len(thead) >= 2 else []) or []
        head_rows = [expand_row(r) for r in raw_hrows]

        # bodies = [[attr, row_head_cols, head_rows, body_rows], ...]
        data_rows = []
        for body in (c[4] or []):
            if isinstance(body, list) and len(body) >= 4:
                for row in (body[3] or []):
                    data_rows.append(expand_row(row))

        return TableBlock(caption, head_rows, data_rows)
    except Exception as e:
        return TableBlock(f"[table error: {e}]", [], [])


# ── Renderer ──────────────────────────────────────────────────────────────────

def render(blocks: list[Block], console):
    import textwrap

    for block in blocks:
        if isinstance(block, Heading):
            prefix = "#" * block.level + "  "
            if USE_RICH:
                if block.level == 1:
                    console.print(Rule(block.text, style="bold cyan"))
                elif block.level == 2:
                    console.print(f"\n[bold blue]{prefix}{block.text}[/bold blue]")
                else:
                    console.print(f"\n[bold]{prefix}{block.text}[/bold]")
            else:
                print(f"\n{prefix}{block.text}")

        elif isinstance(block, Para):
            wrapped = textwrap.fill(block.text, width=WIDTH)
            if USE_RICH:
                console.print(Padding(wrapped, (0, 0, 0, 0)))
            else:
                print(wrapped)
            print()

        elif isinstance(block, CodeBlock):
            if USE_RICH:
                from rich.syntax import Syntax
                console.print(Syntax(block.code, block.lang or "text", theme="monokai"))
            else:
                for line in block.code.splitlines():
                    print("    " + line)
            print()

        elif isinstance(block, BulletItem):
            indent = "  " * block.depth
            wrapped = textwrap.fill(block.text, width=WIDTH - len(indent) - 2,
                                    subsequent_indent=indent + "    ")
            if USE_RICH:
                console.print(f"{indent}• {wrapped}")
            else:
                print(f"{indent}• {wrapped}")

        elif isinstance(block, OrderedItem):
            indent = "  " * block.depth
            wrapped = textwrap.fill(block.text, width=WIDTH - len(indent) - 4,
                                    subsequent_indent=indent + "    ")
            if USE_RICH:
                console.print(f"{indent}{block.n}. {wrapped}")
            else:
                print(f"{indent}{block.n}. {wrapped}")

        elif isinstance(block, HRule):
            if USE_RICH:
                console.print(Rule(style="dim"))
            else:
                print("─" * WIDTH)

        elif isinstance(block, TableBlock):
            render_table(block, console)

def render_table(t: TableBlock, console):
    if not t.head_rows and not t.rows:
        return

    all_rows  = t.head_rows + t.rows
    ncols     = max((len(r) for r in all_rows), default=0)
    if ncols == 0:
        return

    def pad(row): return row + [""] * (ncols - len(row))

    if USE_RICH:
        from rich import box as rich_box
        # Build Rich Table — first header row becomes column names.
        col_names = pad(t.head_rows[0]) if t.head_rows else [""] * ncols
        tbl = RichTable(
            caption=t.caption or None,
            caption_style="dim italic",
            show_header=True,
            header_style="bold",
            box=rich_box.SIMPLE,
            show_lines=False,
            expand=False,
            padding=(0, 1),
        )
        for name in col_names:
            tbl.add_column(name, overflow="fold", no_wrap=False, min_width=4)

        # Extra header rows (e.g. sub-column labels like EN-DE / EN-FR).
        for hrow in t.head_rows[1:]:
            tbl.add_row(*pad(hrow), style="bold dim")

        for row in t.rows:
            tbl.add_row(*pad(row))

        console.print(tbl)
        console.print()
    else:
        # Plain-text fallback.
        col_widths = [min(max(len(pad(r)[c]) for r in all_rows), 28) for c in range(ncols)]
        sep = "─" * (sum(col_widths) + 3 * ncols + 1)
        def fmt(row):
            return "│ " + " │ ".join(str(pad(row)[c])[:col_widths[c]].ljust(col_widths[c])
                                      for c in range(ncols)) + " │"
        print(sep)
        for i, row in enumerate(t.head_rows):
            print(fmt(row))
        if t.head_rows:
            print(sep)
        for row in t.rows:
            print(fmt(row))
        print(sep)
        if t.caption:
            print(f"[Table: {t.caption}]")
        print()


# ── Entry point ───────────────────────────────────────────────────────────────

def main():
    args = sys.argv[1:]
    if not args:
        print("usage: reader.py <arxiv-id>  |  reader.py --local <file.tex>")
        sys.exit(1)

    console = Console(width=WIDTH) if USE_RICH else None

    if args[0] == "--local":
        tex_path = Path(args[1])
    else:
        arxiv_id = extract_id(args[0])
        if not arxiv_id:
            sys.exit(f"could not parse arXiv ID from: {args[0]}")
        src_dir = fetch_source(arxiv_id)
        tex_path = find_root_tex(src_dir)

    print(f"parsing {tex_path} with pandoc...", file=sys.stderr)
    ast = tex_to_pandoc_ast(tex_path)

    blocks = walk_blocks(ast.get("blocks", []))
    render(blocks, console)

if __name__ == "__main__":
    main()
