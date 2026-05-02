/// The format of the math input.
pub enum MathInput<'a> {
  /// Raw LaTeX source, e.g. `\frac{x^2 + 1}{y}`.
  Latex(&'a str),
  /// MathML XML string, e.g. from an EPUB `<math>` block.
  MathMl(&'a str),
}

/// Render math to a Unicode string suitable for terminal display.
///
/// Pipeline:
///   1. preprocess() — expand symbol table, strip sizing hints
///   2. render_multiline() — split align/gather on \\ into per-line renders
///   3. tui_math::render_latex() — attempt Unicode typesetting
///   4. strip_latex() — fallback with frac/sqrt/super/subscript approximation
pub fn render(input: MathInput<'_>) -> String {
  match input {
    MathInput::Latex(src) => {
      let preprocessed = preprocess(src);
      // Multi-line environments (align, gather) must be split first.
      if let Some(multiline) = render_multiline(&preprocessed) {
        return multiline;
      }
      match tui_math::render_latex(&preprocessed) {
        // Any backslash in tui-math output means leaked LaTeX — fall back.
        Ok(s) if !s.contains("[PARSE ERROR:") && !s.contains('\\') => s,
        _ => strip_latex(&preprocessed),
      }
    }
    MathInput::MathMl(src) => src.to_string(),
  }
}

// ── Symbol table ──────────────────────────────────────────────────────────────

/// Shared symbol table used by both preprocess() and strip_latex().
/// Ordered longest-match first where needed (e.g. \varepsilon before \epsilon).
const SYMBOL_TABLE: &[(&str, &str)] = &[
  // ── Greek lowercase ──────────────────────────────────────────────────────
  ("\\varepsilon", "ε"), ("\\vartheta", "ϑ"), ("\\varpi", "ϖ"),
  ("\\varrho", "ϱ"), ("\\varsigma", "ς"), ("\\varphi", "φ"),
  ("\\alpha", "α"), ("\\beta", "β"), ("\\gamma", "γ"), ("\\delta", "δ"),
  ("\\epsilon", "ε"), ("\\zeta", "ζ"), ("\\eta", "η"), ("\\theta", "θ"),
  ("\\iota", "ι"), ("\\kappa", "κ"), ("\\lambda", "λ"), ("\\mu", "μ"),
  ("\\nu", "ν"), ("\\xi", "ξ"), ("\\pi", "π"), ("\\rho", "ρ"),
  ("\\sigma", "σ"), ("\\tau", "τ"), ("\\upsilon", "υ"), ("\\phi", "φ"),
  ("\\chi", "χ"), ("\\psi", "ψ"), ("\\omega", "ω"),

  // ── Greek uppercase ──────────────────────────────────────────────────────
  ("\\Gamma", "Γ"), ("\\Delta", "Δ"), ("\\Theta", "Θ"), ("\\Lambda", "Λ"),
  ("\\Xi", "Ξ"), ("\\Pi", "Π"), ("\\Sigma", "Σ"), ("\\Upsilon", "Υ"),
  ("\\Phi", "Φ"), ("\\Psi", "Ψ"), ("\\Omega", "Ω"),

  // ── Calculus and analysis ────────────────────────────────────────────────
  ("\\partial", "∂"), ("\\nabla", "∇"), ("\\infty", "∞"),
  ("\\ell", "ℓ"), ("\\hbar", "ℏ"),
  ("\\iint", "∬"), ("\\iiint", "∭"), ("\\oint", "∮"), ("\\int", "∫"),
  ("\\coprod", "∐"), ("\\prod", "Π"), ("\\sum", "Σ"),

  // ── Relations ────────────────────────────────────────────────────────────
  ("\\leq", "≤"), ("\\geq", "≥"), ("\\neq", "≠"), ("\\approx", "≈"),
  ("\\simeq", "≃"), ("\\cong", "≅"), ("\\equiv", "≡"), ("\\propto", "∝"),
  ("\\sim", "∼"), ("\\ll", "≪"), ("\\gg", "≫"),
  ("\\preceq", "⪯"), ("\\succeq", "⪰"), ("\\prec", "≺"), ("\\succ", "≻"),
  // \le/\ge/\ne omitted — they are aliases of \leq/\geq/\neq but cause
  // substring collisions: \le matches inside \left → "≤ft".

  // ── Set theory ───────────────────────────────────────────────────────────
  // Order matters: \notin before \in (longer first); \inf/\liminf/\limsup
  // before \in to prevent substring collision (\inf → ∈f).
  ("\\notin", "∉"),
  ("\\limsup", "lim sup"), ("\\liminf", "lim inf"), ("\\inf", "inf"),
  ("\\in", "∈"),
  ("\\subseteq", "⊆"), ("\\supseteq", "⊇"),
  ("\\subset", "⊂"), ("\\supset", "⊃"),
  ("\\cup", "∪"), ("\\cap", "∩"),
  ("\\varnothing", "∅"), ("\\emptyset", "∅"), ("\\setminus", "∖"),

  // ── Logic ────────────────────────────────────────────────────────────────
  ("\\forall", "∀"), ("\\nexists", "∄"), ("\\exists", "∃"), ("\\neg", "¬"),
  ("\\land", "∧"), ("\\lor", "∨"),
  ("\\implies", "⟹"), ("\\iff", "⟺"),

  // ── Arrows ───────────────────────────────────────────────────────────────
  ("\\Leftrightarrow", "⟺"), ("\\leftrightarrow", "↔"),
  ("\\Leftarrow", "⇐"), ("\\Rightarrow", "⇒"),
  ("\\leftarrow", "←"), ("\\rightarrow", "→"),
  ("\\gets", "←"), ("\\to", "→"), ("\\mapsto", "↦"),
  ("\\uparrow", "↑"), ("\\downarrow", "↓"),
  ("\\nearrow", "↗"), ("\\searrow", "↘"),

  // ── Algebra and operators ─────────────────────────────────────────────────
  ("\\oplus", "⊕"), ("\\ominus", "⊖"), ("\\otimes", "⊗"),
  ("\\oslash", "⊘"), ("\\odot", "⊙"),
  ("\\times", "×"), ("\\div", "÷"), ("\\pm", "±"), ("\\mp", "∓"),
  ("\\cdot", "·"), ("\\circ", "∘"), ("\\bullet", "•"),
  ("\\ast", "∗"), ("\\star", "⋆"),

  // ── Misc symbols ──────────────────────────────────────────────────────────
  ("\\dagger", "†"), ("\\ddagger", "‡"), ("\\wp", "℘"),
  ("\\Re", "ℜ"), ("\\Im", "ℑ"), ("\\aleph", "ℵ"),
  ("\\top", "⊤"), ("\\bot", "⊥"), ("\\angle", "∠"),
  ("\\perp", "⊥"), ("\\parallel", "∥"), ("\\triangle", "△"),
  ("\\square", "□"), ("\\diamond", "◇"), ("\\lozenge", "◊"),
  ("\\checkmark", "✓"), ("\\therefore", "∴"), ("\\because", "∵"),
  ("\\infty", "∞"), ("\\prime", "′"), ("\\backslash", "\\"),

  // ── Named operators (just strip the backslash) ────────────────────────────
  ("\\log", "log"), ("\\ln", "ln"), ("\\exp", "exp"),
  ("\\sin", "sin"), ("\\cos", "cos"), ("\\tan", "tan"),
  ("\\arcsin", "arcsin"), ("\\arccos", "arccos"), ("\\arctan", "arctan"),
  ("\\sinh", "sinh"), ("\\cosh", "cosh"), ("\\tanh", "tanh"),
  ("\\min", "min"), ("\\max", "max"), ("\\sup", "sup"),
  // \inf/\limsup/\liminf are placed in set-theory block above \in to
  // prevent the \in substring collision; they are not repeated here.
  ("\\lim", "lim"),
  ("\\det", "det"), ("\\arg", "arg"), ("\\dim", "dim"), ("\\ker", "ker"),
  ("\\deg", "deg"), ("\\gcd", "gcd"), ("\\Pr", "Pr"), ("\\tr", "tr"),
  ("\\rank", "rank"), ("\\span", "span"), ("\\diag", "diag"),

  // ── Delimiters / sizing hints (drop the command token) ─────────────────────
  ("\\left\\{", "{"), ("\\right\\}", "}"),
  ("\\left(", "("), ("\\right)",")" ),
  ("\\left[", "["), ("\\right]", "]"),
  ("\\left\\|", "‖"), ("\\right\\|", "‖"),
  ("\\left|", "|"), ("\\right|", "|"),
  ("\\left.", ""), ("\\right.", ""),
  ("\\biggr", ""), ("\\biggl", ""),
  ("\\Biggr", ""), ("\\Biggl", ""),
  ("\\Bigr", ""), ("\\Bigl", ""),
  ("\\bigr", ""), ("\\bigl", ""),
  ("\\big", ""),

  // ── Spacing ────────────────────────────────────────────────────────────────
  ("\\quad", " "), ("\\qquad", "  "),
  ("\\medspace", " "), ("\\thinspace", " "), ("\\thickspace", " "),
  ("\\negthinspace", ""), ("\\negmedspace", ""), ("\\negthickspace", ""),
  ("\\,", " "), ("\\;", " "), ("\\:", " "), ("\\!", ""),
  // Bare mu-spacing that tui-math can't parse.
  ("-1.5mu", ""), ("-2mu", ""), ("-3mu", ""), ("-4mu", ""), ("-6mu", ""),
  ("1mu", ""), ("2mu", ""), ("3mu", ""), ("4mu", ""), ("6mu", ""),

  // ── Misc punctuation and formatting ───────────────────────────────────────
  ("\\lvert", "|"), ("\\rvert", "|"), ("\\lVert", "‖"), ("\\rVert", "‖"),
  ("\\mid", "|"), ("\\colon", ":"),
  ("\\ldots", "…"), ("\\cdots", "…"), ("\\vdots", "⋮"), ("\\ddots", "⋱"),
  ("\\dots", "…"),

  // ── mathbb (already handled via replace_braced_cmd, but add common ones) ──
  // These are bare (no braces) fallbacks after replace_braced_cmd runs.
];

// ── Preprocessing ─────────────────────────────────────────────────────────────

fn preprocess(src: &str) -> String {
  let mut s = src.to_string();

  // 1. \mathbb{X} → Unicode double-struck equivalents.
  for (letter, sym) in MATHBB {
    s = s.replace(&format!("\\mathbb{{{letter}}}"), sym);
  }

  // 2. Commands where content should be kept, wrapper dropped.
  for cmd in &["mathcal", "mathbf", "mathit", "mathsf", "mathrm", "text",
               "mbox", "hbox", "boldsymbol", "bm", "operatorname",
               "underbrace", "overbrace", "overline", "underline",
               "widehat", "widetilde", "tilde", "hat", "bar", "vec",
               "dot", "ddot", "phantom"] {
    s = replace_braced_cmd(&s, cmd);
  }

  // 3. Commands where content should be silently dropped.
  for cmd in &["label", "tag", "nonumber", "notag", "color"] {
    s = remove_braced_cmd(&s, cmd);
  }

  // 4. Apply the full symbol table.
  for (from, to) in SYMBOL_TABLE {
    s = s.replace(from, to);
  }

  s
}

const MATHBB: &[(&str, &str)] = &[
  ("R", "ℝ"), ("N", "ℕ"), ("Z", "ℤ"), ("Q", "ℚ"), ("C", "ℂ"),
  ("P", "ℙ"), ("E", "𝔼"), ("F", "𝔽"), ("H", "ℍ"), ("1", "𝟙"),
];

// ── Multi-line rendering ──────────────────────────────────────────────────────

/// Detect `align`-style environments (contain `\\` line breaks) and render
/// each equation line independently, returning newline-joined Unicode.
/// Returns None for single-line math (let tui_math handle it).
fn render_multiline(src: &str) -> Option<String> {
  if !src.contains(r"\\") {
    return None;
  }
  let rendered_lines: Vec<String> = src
    .split(r"\\")
    .map(|fragment| {
      // Strip alignment markers; join columns with two spaces.
      let clean: String = fragment
        .split('&')
        .map(|col| col.trim())
        .filter(|col| !col.is_empty())
        .collect::<Vec<_>>()
        .join("  ");
      if clean.is_empty() {
        return String::new();
      }
      match tui_math::render_latex(&clean) {
        Ok(s) if !s.contains("[PARSE ERROR:") && !s.contains('\\') => s,
        _ => strip_latex(&clean),
      }
    })
    .filter(|l| !l.trim().is_empty())
    .collect();

  if rendered_lines.is_empty() {
    return None;
  }
  Some(rendered_lines.join("\n"))
}

// ── Fallback strip ────────────────────────────────────────────────────────────

/// Fallback renderer: strips LaTeX structure but approximates readability.
/// Handles \frac, \sqrt, superscripts, subscripts, and all symbols in SYMBOL_TABLE.
pub fn strip_latex(src: &str) -> String {
  // Apply symbol table first so Greek/operators are readable in the output.
  let mut src_clean = src.to_string();
  for (from, to) in SYMBOL_TABLE {
    src_clean = src_clean.replace(from, to);
  }

  let chars: Vec<char> = src_clean.chars().collect();
  let len = chars.len();
  let mut out = String::new();
  let mut i = 0;

  while i < len {
    let c = chars[i];

    // Skip comments.
    if c == '%' && (i == 0 || chars[i - 1] != '\\') {
      while i < len && chars[i] != '\n' { i += 1; }
      continue;
    }

    // Superscripts.
    if c == '^' {
      i += 1;
      let (exp, skip) = read_braced_or_char(&chars, i);
      i += skip;
      let clean = strip_latex_simple(&exp);
      out.push_str(&to_superscript(&clean));
      continue;
    }

    // Subscripts.
    if c == '_' {
      i += 1;
      let (sub, skip) = read_braced_or_char(&chars, i);
      i += skip;
      let clean = strip_latex_simple(&sub);
      out.push_str(&to_subscript(&clean));
      continue;
    }

    if c == '\\' && i + 1 < len {
      i += 1;
      // Read command name (alphabetic) or single non-alpha char.
      let start = i;
      if chars[i].is_alphabetic() {
        while i < len && chars[i].is_alphabetic() { i += 1; }
      } else {
        i += 1;
      }
      let cmd: String = chars[start..i].iter().collect();
      // Skip trailing spaces after alphabetic command.
      if chars[start].is_alphabetic() {
        while i < len && chars[i] == ' ' { i += 1; }
      }

      match cmd.as_str() {
        // \frac{num}{den} → num/den
        "frac" | "tfrac" | "dfrac" => {
          let (num, skip1) = read_braced(&chars, i); i += skip1;
          let (den, skip2) = read_braced(&chars, i); i += skip2;
          let rnum = strip_latex(&num);
          let rden = strip_latex(&den);
          if rden.is_empty() { out.push_str(&rnum); }
          else { out.push_str(&format!("{}/{}", rnum, rden)); }
        }
        // \sqrt{x} → √x
        "sqrt" => {
          // Optional [n] for nth root — skip it.
          if i < len && chars[i] == '[' {
            while i < len && chars[i] != ']' { i += 1; }
            if i < len { i += 1; }
          }
          let (inner, skip) = read_braced(&chars, i); i += skip;
          out.push_str(&format!("√{}", strip_latex(&inner)));
        }
        // Keep braced content.
        "mathbb" | "mathcal" | "mathbf" | "mathit" | "mathsf" | "mathrm"
        | "text" | "mbox" | "hbox" | "operatorname" | "boldsymbol" | "bm"
        | "tilde" | "hat" | "bar" | "vec" | "dot" | "ddot" | "widehat"
        | "widetilde" | "overline" | "underline" | "overbrace" | "underbrace" => {
          let (content, skip) = read_braced(&chars, i); i += skip;
          out.push_str(&strip_latex(&content));
        }
        // Drop braced content silently.
        "label" | "tag" | "nonumber" | "notag" | "vspace" | "hspace"
        | "phantom" | "color" => {
          if i < len && chars[i] == '{' {
            let (_, skip) = read_braced(&chars, i); i += skip;
          }
        }
        // Spacing → single space (already handled by SYMBOL_TABLE replacement
        // but catch any that remain).
        "quad" | "qquad" => out.push(' '),
        // LaTeX line break → newline.
        "\\" | "newline" | "cr" => out.push('\n'),
        // Alignment — treated as space.
        _ => {
          if i < len && chars[i] == '{' {
            let (content, skip) = read_braced(&chars, i); i += skip;
            out.push_str(&strip_latex(&content));
          }
        }
      }
      continue;
    }

    // Strip bare grouping braces.
    if c == '{' || c == '}' { i += 1; continue; }
    // Alignment char — space.
    if c == '&' { out.push(' '); i += 1; continue; }

    out.push(c);
    i += 1;
  }

  let result = out.trim().to_string();
  if result.is_empty() { "[equation]".to_string() } else { result }
}

/// Like strip_latex but returns "" for empty/unknown expressions (used inside
/// super/subscript handlers so we don't embed "[equation]" inside a script).
fn strip_latex_simple(src: &str) -> String {
  let result = strip_latex(src);
  if result == "[equation]" { String::new() } else { result }
}

// ── Super/subscript Unicode approximation ─────────────────────────────────────

fn to_superscript(s: &str) -> String {
  // For short simple expressions, map to Unicode combining superscripts.
  let mapped: Option<String> = s.chars().map(|c| superscript_char(c)).collect();
  match mapped {
    Some(m) => m,
    None => format!("^({})", s),
  }
}

fn to_subscript(s: &str) -> String {
  let mapped: Option<String> = s.chars().map(|c| subscript_char(c)).collect();
  match mapped {
    Some(m) => m,
    None => format!("_({})", s),
  }
}

fn superscript_char(c: char) -> Option<char> {
  Some(match c {
    '0' => '⁰', '1' => '¹', '2' => '²', '3' => '³', '4' => '⁴',
    '5' => '⁵', '6' => '⁶', '7' => '⁷', '8' => '⁸', '9' => '⁹',
    'a' => 'ᵃ', 'b' => 'ᵇ', 'c' => 'ᶜ', 'd' => 'ᵈ', 'e' => 'ᵉ',
    'f' => 'ᶠ', 'g' => 'ᵍ', 'h' => 'ʰ', 'i' => 'ⁱ', 'j' => 'ʲ',
    'k' => 'ᵏ', 'l' => 'ˡ', 'm' => 'ᵐ', 'n' => 'ⁿ', 'o' => 'ᵒ',
    'p' => 'ᵖ', 'r' => 'ʳ', 's' => 'ˢ', 't' => 'ᵗ', 'u' => 'ᵘ',
    'v' => 'ᵛ', 'w' => 'ʷ', 'x' => 'ˣ', 'y' => 'ʸ', 'z' => 'ᶻ',
    'A' => 'ᴬ', 'B' => 'ᴮ', 'D' => 'ᴰ', 'E' => 'ᴱ', 'G' => 'ᴳ',
    'H' => 'ᴴ', 'I' => 'ᴵ', 'J' => 'ᴶ', 'K' => 'ᴷ', 'L' => 'ᴸ',
    'M' => 'ᴹ', 'N' => 'ᴺ', 'O' => 'ᴼ', 'P' => 'ᴾ', 'R' => 'ᴿ',
    'T' => 'ᵀ', 'U' => 'ᵁ', 'V' => 'ᵛ', 'W' => 'ᵂ',
    '+' => '⁺', '-' => '⁻', '=' => '⁼', '(' => '⁽', ')' => '⁾',
    '*' => '⃰', ' ' => ' ',
    _ => return None,
  })
}

fn subscript_char(c: char) -> Option<char> {
  Some(match c {
    '0' => '₀', '1' => '₁', '2' => '₂', '3' => '₃', '4' => '₄',
    '5' => '₅', '6' => '₆', '7' => '₇', '8' => '₈', '9' => '₉',
    'a' => 'ₐ', 'e' => 'ₑ', 'h' => 'ₕ', 'i' => 'ᵢ', 'j' => 'ⱼ',
    'k' => 'ₖ', 'l' => 'ₗ', 'm' => 'ₘ', 'n' => 'ₙ', 'o' => 'ₒ',
    'p' => 'ₚ', 'r' => 'ᵣ', 's' => 'ₛ', 't' => 'ₜ', 'u' => 'ᵤ',
    'v' => 'ᵥ', 'x' => 'ₓ',
    '+' => '₊', '-' => '₋', '=' => '₌', '(' => '₍', ')' => '₎',
    ' ' => ' ',
    _ => return None,
  })
}

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Read a `{...}` braced group, returning (content, chars_consumed).
fn read_braced(chars: &[char], start: usize) -> (String, usize) {
  if start >= chars.len() || chars[start] != '{' {
    return (String::new(), 0);
  }
  let mut depth = 1usize;
  let mut content = String::new();
  let mut i = start + 1;
  while i < chars.len() {
    match chars[i] {
      '{' => { depth += 1; content.push('{'); }
      '}' => {
        depth -= 1;
        if depth == 0 { i += 1; break; }
        content.push('}');
      }
      c => content.push(c),
    }
    i += 1;
  }
  (content, i - start)
}

/// Read a `{...}` group OR a single char if no `{` follows.
fn read_braced_or_char(chars: &[char], start: usize) -> (String, usize) {
  if start >= chars.len() { return (String::new(), 0); }
  if chars[start] == '{' {
    read_braced(chars, start)
  } else {
    (chars[start].to_string(), 1)
  }
}

/// Replace `\cmd{content}` with just `content`.
fn replace_braced_cmd(src: &str, cmd: &str) -> String {
  let marker = format!("\\{cmd}{{");
  transform_braced_cmd(src, &marker, true)
}

/// Replace `\cmd{content}` with `""` (drop everything).
fn remove_braced_cmd(src: &str, cmd: &str) -> String {
  let marker = format!("\\{cmd}{{");
  transform_braced_cmd(src, &marker, false)
}

fn transform_braced_cmd(src: &str, marker: &str, keep_content: bool) -> String {
  let mut out = String::with_capacity(src.len());
  let mut rest = src;
  while let Some(pos) = rest.find(marker) {
    out.push_str(&rest[..pos]);
    rest = &rest[pos + marker.len()..];
    let mut depth = 1usize;
    let mut content = String::new();
    let mut consumed = 0usize;
    for c in rest.chars() {
      consumed += c.len_utf8();
      match c {
        '{' => { depth += 1; content.push(c); }
        '}' => {
          depth -= 1;
          if depth == 0 { break; }
          content.push(c);
        }
        _ => content.push(c),
      }
    }
    rest = &rest[consumed..];
    if keep_content { out.push_str(&content); }
  }
  out.push_str(rest);
  out
}
