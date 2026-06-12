//! Deterministic frontend-design anti-pattern rules — no LLM.
//!
//! A faithful Rust port of Impeccable's text/regex engine
//! (`cli/engine/engines/regex/detect-text.mjs`): line-based matchers plus
//! whole-file analyzers over CSS/JSX/TSX/HTML source that flag the design
//! "tells" models default to. Same shape/contract as `diagnostics`: per-file,
//! deterministic, near zero false-positive (findings drive a cleanup rewrite).
//!
//! Rust's `regex` crate has no lookaround/backreferences, so the few upstream
//! patterns that used them are ported as a simple match + a code-side test
//! (exactly what their `test()` closures did).
//!
//! The four prose/copy analyzers (em-dash, marketing-buzzword, numbered
//! sections, aphoristic cadence) are gated to full HTML documents (`is_full_page`),
//! matching upstream — they can't fire on component fragments.
//!
//! This is a snapshot of Impeccable's ruleset; it does not track upstream.

use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// One design anti-pattern flagged in a file.
#[derive(Debug, Clone)]
pub(crate) struct DesignFinding {
    pub rule: &'static str,
    pub name: &'static str,
    pub guidance: &'static str,
    pub file: String,
    pub line: usize,
    pub snippet: String,
}

struct Rule {
    id: &'static str,
    name: &'static str,
    guidance: &'static str,
}

macro_rules! rule {
    ($const:ident, $id:literal, $name:literal, $guidance:literal) => {
        const $const: Rule = Rule {
            id: $id,
            name: $name,
            guidance: $guidance,
        };
    };
}

rule!(
    SIDE_TAB,
    "side-tab",
    "Side-tab accent border",
    "Thick colored border on one side of a card — the most recognizable tell of AI-generated UIs. Use a subtler accent or remove it."
);
rule!(
    BORDER_ACCENT,
    "border-accent-on-rounded",
    "Border accent on rounded element",
    "Thick accent border on a rounded card clashes with the rounded corners. Remove the border or the border-radius."
);
rule!(
    OVERUSED_FONT,
    "overused-font",
    "Overused font",
    "Inter, Roboto, Fraunces, Geist, Plus Jakarta Sans, Space Grotesk and friends are on so many AI UIs they no longer feel distinctive. Choose a face with personality."
);
rule!(
    GRADIENT_TEXT,
    "gradient-text",
    "Gradient text",
    "Gradient text is decorative rather than meaningful — a common AI tell, especially on headings/metrics. Use a solid color for text."
);
rule!(
    GRAY_ON_COLOR,
    "gray-on-color",
    "Gray text on colored background",
    "Gray text looks washed out on colored backgrounds. Use a darker shade of the background color, or white/near-white."
);
rule!(
    AI_PALETTE,
    "ai-color-palette",
    "AI color palette",
    "Purple/violet gradients and cyan-on-dark are recognizable tells of AI-generated UIs. Choose a distinctive, intentional palette."
);
rule!(
    BOUNCE_EASING,
    "bounce-easing",
    "Bounce or elastic easing",
    "Bounce/elastic easing feels dated. Real objects decelerate smoothly — use exponential easing (ease-out-quart/quint/expo)."
);
rule!(
    LAYOUT_TRANSITION,
    "layout-transition",
    "Layout property animation",
    "Animating width/height/padding/margin causes layout thrash. Animate transform and opacity instead (or grid-template-rows for height)."
);
rule!(
    BROKEN_IMAGE,
    "broken-image",
    "Broken or placeholder image",
    "<img> with empty/missing/placeholder src ships as a broken-image box. Use a real image, a generated asset, or remove the tag."
);
rule!(
    SINGLE_FONT,
    "single-font",
    "Single font for everything",
    "One font family for the whole page. Pair a distinctive display font with a refined body font to create hierarchy."
);
rule!(
    FLAT_TYPE,
    "flat-type-hierarchy",
    "Flat type hierarchy",
    "Font sizes are too close together — no clear hierarchy. Use fewer sizes with more contrast (aim for at least a 1.25 ratio)."
);
rule!(
    MONOTONOUS_SPACING,
    "monotonous-spacing",
    "Monotonous spacing",
    "The same spacing value used everywhere — no rhythm. Tighten related items and separate sections more generously."
);
rule!(
    DARK_GLOW,
    "dark-glow",
    "Dark mode with glowing accents",
    "Dark backgrounds with colored box-shadow glows are the default 'cool' AI look. Use subtle, purposeful lighting."
);
rule!(
    EM_DASH,
    "em-dash-overuse",
    "Em-dash overuse",
    "More than two em-dashes (— or --) in body copy is an AI cadence tell. Use commas, colons, periods, or parentheses."
);
rule!(
    MARKETING_BUZZWORD,
    "marketing-buzzword",
    "Marketing buzzword",
    "Generic SaaS phrases (streamline/empower/supercharge/world-class/enterprise-grade/…) are instant AI tells. Say what the product literally does."
);
rule!(
    NUMBERED_SECTIONS,
    "numbered-section-markers",
    "Numbered section markers",
    "Numbered display markers as section labels (01, 02, 03) are an AI editorial scaffold. Choose a different section cadence."
);
rule!(
    APHORISTIC,
    "aphoristic-cadence",
    "Aphoristic-cadence copy",
    "Repeated short-rebuttal ('X. No Y.') or manufactured-contrast ('Not a feature. A platform.') reads as AI cadence, not voice."
);

/// Design findings for one file. Empty for non-frontend files.
pub(crate) fn design_findings(path: &str, content: &str) -> Vec<DesignFinding> {
    if !is_frontend_file(path) {
        return Vec::new();
    }
    let mut out = Vec::new();

    // Line matchers.
    for (idx, line) in content.lines().enumerate() {
        let ln = idx + 1;
        for detect in LINE_RULES {
            if let Some((rule, snippet)) = detect(line) {
                out.push(finding(rule, path, ln, snippet));
            }
        }
    }

    // Whole-file analyzers (structural always; prose only on full HTML pages).
    let full_page = is_full_page(content);
    for analyze in ANALYZERS {
        if let Some((rule, line, snippet)) = analyze(content) {
            out.push(finding(rule, path, line, snippet));
        }
    }
    if full_page {
        for analyze in PROSE_ANALYZERS {
            if let Some((rule, line, snippet)) = analyze(content) {
                out.push(finding(rule, path, line, snippet));
            }
        }
    }
    out
}

type LineRule = fn(&str) -> Option<(&'static Rule, String)>;
const LINE_RULES: &[LineRule] = &[
    side_tab,
    border_accent,
    overused_font,
    gradient_text,
    gray_on_color,
    ai_palette,
    bounce_easing,
    layout_transition,
    broken_image,
];

type Analyzer = fn(&str) -> Option<(&'static Rule, usize, String)>;
const ANALYZERS: &[Analyzer] = &[
    single_font,
    flat_type_hierarchy,
    monotonous_spacing,
    dark_glow,
];
const PROSE_ANALYZERS: &[Analyzer] = &[
    em_dash_overuse,
    marketing_buzzword,
    numbered_section_markers,
    aphoristic_cadence,
];

fn finding(rule: &Rule, file: &str, line: usize, snippet: String) -> DesignFinding {
    DesignFinding {
        rule: rule.id,
        name: rule.name,
        guidance: rule.guidance,
        file: file.to_string(),
        line,
        snippet,
    }
}

fn is_frontend_file(path: &str) -> bool {
    let ext = match path.rsplit_once('.') {
        Some((_, ext)) if !ext.contains('/') => ext,
        _ => return false,
    };
    matches!(
        ext,
        "tsx"
            | "jsx"
            | "ts"
            | "js"
            | "mjs"
            | "cjs"
            | "css"
            | "scss"
            | "html"
            | "htm"
            | "astro"
            | "vue"
            | "svelte"
    )
}

/// A full HTML document (has a doctype/html/head), as opposed to a fragment.
fn is_full_page(content: &str) -> bool {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)<!doctype\s|<html[\s>]|<head[\s>]").unwrap());
    let stripped = strip_comments(content);
    RE.is_match(&stripped)
}

// ── shared line helpers ──────────────────────────────────────────────────────
fn has_rounded(line: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\brounded(?:-\w+)?\b").unwrap());
    RE.is_match(line)
}
fn has_border_radius(line: &str) -> bool {
    line.to_ascii_lowercase().contains("border-radius")
}
fn is_safe_element(line: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)<(?:blockquote|nav[\s>]|pre[\s>]|code[\s>]|a\s|input[\s>]|span[\s>])")
            .unwrap()
    });
    RE.is_match(line)
}
fn is_neutral_border_color(s: &str) -> bool {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)solid\s+(#[0-9a-f]{3,8}|rgba?\([^)]+\)|\w+)").unwrap());
    let Some(c) = RE.captures(s) else {
        return false;
    };
    let color = c[1].to_ascii_lowercase();
    if matches!(
        color.as_str(),
        "gray" | "grey" | "silver" | "white" | "black" | "transparent" | "currentcolor"
    ) {
        return true;
    }
    if let Some(hex) = color.strip_prefix('#') {
        let chan = |a: usize, b: usize| u8::from_str_radix(&hex[a..b], 16).ok();
        let rgb = match hex.len() {
            6 => (chan(0, 2), chan(2, 4), chan(4, 6)),
            3 => {
                let dup = |i: usize| u8::from_str_radix(&hex[i..i + 1].repeat(2), 16).ok();
                (dup(0), dup(1), dup(2))
            }
            _ => (None, None, None),
        };
        if let (Some(r), Some(g), Some(b)) = rgb {
            return (r.max(g).max(b) - r.min(g).min(b)) < 30;
        }
    }
    false
}

// ── overused-font ────────────────────────────────────────────────────────────
fn overused_font(line: &str) -> Option<(&'static Rule, String)> {
    static FAMILY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)font-family\s*:\s*['"]?(Inter|Roboto|Open Sans|Lato|Montserrat|Arial|Helvetica|Fraunces|Geist Sans|Geist Mono|Geist|Mona Sans|Plus Jakarta Sans|Space Grotesk|Recoleta|Instrument Sans|Instrument Serif)\b"#).unwrap()
    });
    static GF: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?i)fonts\.googleapis\.com/css2?\?family=(Inter|Roboto|Open\+Sans|Lato|Montserrat|Fraunces|Plus\+Jakarta\+Sans|Space\+Grotesk|Instrument\+Sans|Instrument\+Serif|Mona\+Sans|Geist)\b"#).unwrap()
    });
    if let Some(m) = FAMILY.find(line) {
        return Some((&OVERUSED_FONT, m.as_str().to_string()));
    }
    if let Some(c) = GF.captures(line) {
        return Some((
            &OVERUSED_FONT,
            format!("Google Fonts: {}", c[1].replace('+', " ")),
        ));
    }
    None
}

// ── gradient-text ──────────────────────────────────────────────────────────
fn gradient_text(line: &str) -> Option<(&'static Rule, String)> {
    static BG_CLIP: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)(?:-webkit-)?background-clip\s*:\s*text").unwrap());
    if BG_CLIP.is_match(line) && line.to_ascii_lowercase().contains("gradient") {
        return Some((
            &GRADIENT_TEXT,
            "background-clip: text + gradient".to_string(),
        ));
    }
    if line.contains("bg-clip-text") && line.contains("bg-gradient-to-") {
        return Some((&GRADIENT_TEXT, "bg-clip-text + bg-gradient".to_string()));
    }
    None
}

// ── gray-on-color ──────────────────────────────────────────────────────────
fn gray_on_color(line: &str) -> Option<(&'static Rule, String)> {
    static GRAY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\btext-(?:gray|slate|zinc|neutral|stone)-\d+\b").unwrap());
    static BG: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\bbg-(?:red|orange|amber|yellow|lime|green|emerald|teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose)-\d+\b").unwrap()
    });
    let gray = GRAY.find(line)?;
    let bg = BG.find(line)?;
    Some((
        &GRAY_ON_COLOR,
        format!("{} on {}", gray.as_str(), bg.as_str()),
    ))
}

// ── side-tab ──────────────────────────────────────────────────────────────
fn side_tab(line: &str) -> Option<(&'static Rule, String)> {
    static TW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bborder-[lrse]-(\d+)\b").unwrap());
    static LR_SOLID: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)border-(?:left|right)\s*:\s*(\d+)px\s+solid[^;]*").unwrap()
    });
    static LR_WIDTH: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)border-(?:left|right)-width\s*:\s*(\d+)px").unwrap());
    static INLINE_SOLID: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)border-inline-(?:start|end)\s*:\s*(\d+)px\s+solid").unwrap()
    });
    static INLINE_WIDTH: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)border-inline-(?:start|end)-width\s*:\s*(\d+)px").unwrap()
    });
    static JSX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"border(?:Left|Right)\s*[:=]\s*["'`](\d+)px\s+solid"#).unwrap()
    });

    if let Some(c) = TW.captures(line) {
        let n: i64 = c[1].parse().unwrap_or(0);
        let trip = if has_rounded(line) { n >= 1 } else { n >= 4 };
        if trip {
            return Some((&SIDE_TAB, c[0].to_string()));
        }
    }
    if let Some(c) = LR_SOLID.captures(line) {
        let n: i64 = c[1].parse().unwrap_or(0);
        let trip = !is_safe_element(line)
            && !is_neutral_border_color(&c[0])
            && (if has_border_radius(line) {
                n >= 1
            } else {
                n >= 3
            });
        if trip {
            return Some((&SIDE_TAB, c[0].trim_end_matches(';').trim().to_string()));
        }
    }
    for re in [&*LR_WIDTH, &*INLINE_SOLID, &*INLINE_WIDTH] {
        if let Some(c) = re.captures(line) {
            let n: i64 = c[1].parse().unwrap_or(0);
            if !is_safe_element(line) && n >= 3 {
                return Some((&SIDE_TAB, c[0].to_string()));
            }
        }
    }
    if let Some(c) = JSX.captures(line) {
        let n: i64 = c[1].parse().unwrap_or(0);
        if n >= 3 {
            return Some((&SIDE_TAB, c[0].to_string()));
        }
    }
    None
}

// ── border-accent-on-rounded ────────────────────────────────────────────────
fn border_accent(line: &str) -> Option<(&'static Rule, String)> {
    static TW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bborder-[tb]-(\d+)\b").unwrap());
    static SOLID: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)border-(?:top|bottom)\s*:\s*(\d+)px\s+solid").unwrap());
    if let Some(c) = TW.captures(line) {
        let n: i64 = c[1].parse().unwrap_or(0);
        if has_rounded(line) && n >= 1 {
            return Some((&BORDER_ACCENT, c[0].to_string()));
        }
    }
    if let Some(c) = SOLID.captures(line) {
        let n: i64 = c[1].parse().unwrap_or(0);
        if n >= 3 && has_border_radius(line) {
            return Some((&BORDER_ACCENT, c[0].to_string()));
        }
    }
    None
}

// ── ai-color-palette ─────────────────────────────────────────────────────────
fn ai_palette(line: &str) -> Option<(&'static Rule, String)> {
    static TEXT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\btext-(?:purple|violet|indigo)-\d+\b").unwrap());
    static HEADINGY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\btext-[2-9]xl\b|<h[1-3]").unwrap());
    static FROM: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\bfrom-(?:purple|violet|indigo)-\d+\b").unwrap());
    static TO: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\bto-(?:purple|violet|indigo|blue|cyan|pink|fuchsia)-\d+\b").unwrap()
    });
    if let Some(m) = TEXT.find(line) {
        if HEADINGY.is_match(line) {
            return Some((&AI_PALETTE, format!("{} on heading", m.as_str())));
        }
    }
    if let Some(m) = FROM.find(line) {
        if TO.is_match(line) {
            return Some((&AI_PALETTE, format!("{} gradient", m.as_str())));
        }
    }
    None
}

// ── bounce-easing ────────────────────────────────────────────────────────────
fn bounce_easing(line: &str) -> Option<(&'static Rule, String)> {
    static ANIM: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)animation(?:-name)?\s*:\s*[^;]*\b(?:bounce|elastic|wobble|jiggle|spring)\b",
        )
        .unwrap()
    });
    static CUBIC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"cubic-bezier\(\s*([\d.-]+)\s*,\s*([\d.-]+)\s*,\s*([\d.-]+)\s*,\s*([\d.-]+)\s*\)",
        )
        .unwrap()
    });
    if line.contains("animate-bounce") {
        return Some((&BOUNCE_EASING, "animate-bounce (Tailwind)".to_string()));
    }
    if let Some(m) = ANIM.find(line) {
        return Some((&BOUNCE_EASING, m.as_str().to_string()));
    }
    if let Some(c) = CUBIC.captures(line) {
        let y1: f64 = c[2].parse().unwrap_or(0.0);
        let y2: f64 = c[4].parse().unwrap_or(0.0);
        if !(-0.1..=1.1).contains(&y1) || !(-0.1..=1.1).contains(&y2) {
            return Some((&BOUNCE_EASING, c[0].to_string()));
        }
    }
    None
}

// ── layout-transition ─────────────────────────────────────────────────────────
fn layout_transition(line: &str) -> Option<(&'static Rule, String)> {
    static TRANS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)transition(?:-property)?\s*:\s*([^;{}]+)").unwrap());
    static LAYOUT_PROP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(?:(?:max|min)-)?(?:width|height)\b|\bpadding(?:-(?:top|right|bottom|left))?\b|\bmargin(?:-(?:top|right|bottom|left))?\b").unwrap()
    });
    let c = TRANS.captures(line)?;
    let val = c[1].to_ascii_lowercase();
    if val.split_whitespace().any(|t| t == "all") {
        return None;
    }
    let found: Vec<String> = LAYOUT_PROP
        .find_iter(&c[1])
        .map(|m| m.as_str().to_string())
        .collect();
    if found.is_empty() {
        return None;
    }
    Some((
        &LAYOUT_TRANSITION,
        format!("transition: {}", found.join(", ")),
    ))
}

// ── broken-image ──────────────────────────────────────────────────────────
fn broken_image(line: &str) -> Option<(&'static Rule, String)> {
    static EMPTY_SRC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r##"(?i)<img\b[^>]*?\bsrc\s*=\s*(?:""|''|"\s+"|'\s+'|"#"|'#')"##).unwrap()
    });
    static ANY_IMG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<img\b[^>]*>").unwrap());
    static HAS_SRC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bsrc\s*=").unwrap());
    if let Some(m) = EMPTY_SRC.find(line) {
        return Some((&BROKEN_IMAGE, truncate(m.as_str(), 100)));
    }
    // Lookahead-free port: an <img> tag with no `src=` at all.
    if let Some(m) = ANY_IMG.find(line) {
        if !HAS_SRC.is_match(m.as_str()) {
            return Some((&BROKEN_IMAGE, truncate(m.as_str(), 100)));
        }
    }
    None
}

// ── single-font (whole file) ────────────────────────────────────────────────
fn single_font(content: &str) -> Option<(&'static Rule, usize, String)> {
    static GENERIC: &[&str] = &[
        "serif",
        "sans-serif",
        "monospace",
        "cursive",
        "fantasy",
        "system-ui",
        "ui-serif",
        "ui-sans-serif",
        "ui-monospace",
        "ui-rounded",
        "-apple-system",
        "blinkmacsystemfont",
        "segoe ui",
        "inherit",
        "initial",
        "unset",
        "revert",
    ];
    static FAMILY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)font-family\s*:\s*([^;}]+)").unwrap());
    static GF: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)fonts\.googleapis\.com/css2?\?family=([^&\x22'\s]+)").unwrap()
    });
    let mut fonts: HashSet<String> = HashSet::new();
    for c in FAMILY.captures_iter(content) {
        for f in c[1].split(',') {
            let f = f
                .trim()
                .trim_matches(|ch| ch == '\'' || ch == '"')
                .to_ascii_lowercase();
            if !f.is_empty() && !GENERIC.contains(&f.as_str()) {
                fonts.insert(f);
            }
        }
    }
    for c in GF.captures_iter(content) {
        for f in c[1].split('|') {
            let name = f
                .split(':')
                .next()
                .unwrap_or("")
                .replace('+', " ")
                .to_ascii_lowercase();
            if !name.is_empty() {
                fonts.insert(name);
            }
        }
    }
    if fonts.len() != 1 || content.lines().count() < 20 {
        return None;
    }
    let name = fonts.into_iter().next().unwrap();
    let line = content
        .lines()
        .position(|l| l.to_ascii_lowercase().contains(&name))
        .map(|i| i + 1)
        .unwrap_or(1);
    Some((&SINGLE_FONT, line, format!("only font used is {name}")))
}

// ── flat-type-hierarchy (whole file) ──────────────────────────────────────────
fn flat_type_hierarchy(content: &str) -> Option<(&'static Rule, usize, String)> {
    const REM: f64 = 16.0;
    static SIZE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)font-size\s*:\s*([\d.]+)(px|rem|em)\b").unwrap());
    static CLAMP: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)font-size\s*:\s*clamp\(\s*([\d.]+)(px|rem|em)\s*,\s*[^,]+,\s*([\d.]+)(px|rem|em)\s*\)").unwrap()
    });
    let mut sizes: HashSet<i64> = HashSet::new();
    let to_px = |v: f64, unit: &str| {
        if unit.eq_ignore_ascii_case("px") {
            v
        } else {
            v * REM
        }
    };
    let key = |px: f64| (px * 10.0).round() as i64;
    for c in SIZE.captures_iter(content) {
        let px = to_px(c[1].parse().unwrap_or(0.0), &c[2]);
        if px > 0.0 && px < 200.0 {
            sizes.insert(key(px));
        }
    }
    for c in CLAMP.captures_iter(content) {
        sizes.insert(key(to_px(c[1].parse().unwrap_or(0.0), &c[2])));
        sizes.insert(key(to_px(c[3].parse().unwrap_or(0.0), &c[4])));
    }
    let tw: [(&str, f64); 13] = [
        ("text-xs", 12.0),
        ("text-sm", 14.0),
        ("text-base", 16.0),
        ("text-lg", 18.0),
        ("text-xl", 20.0),
        ("text-2xl", 24.0),
        ("text-3xl", 30.0),
        ("text-4xl", 36.0),
        ("text-5xl", 48.0),
        ("text-6xl", 60.0),
        ("text-7xl", 72.0),
        ("text-8xl", 96.0),
        ("text-9xl", 128.0),
    ];
    for (cls, px) in tw {
        if Regex::new(&format!(r"\b{cls}\b"))
            .unwrap()
            .is_match(content)
        {
            sizes.insert(key(px));
        }
    }
    if sizes.len() < 3 {
        return None;
    }
    let mut sorted: Vec<i64> = sizes.into_iter().collect();
    sorted.sort_unstable();
    let ratio = *sorted.last().unwrap() as f64 / *sorted.first().unwrap() as f64;
    if ratio >= 2.0 {
        return None;
    }
    let px_list: Vec<String> = sorted
        .iter()
        .map(|k| format!("{}px", *k as f64 / 10.0))
        .collect();
    Some((
        &FLAT_TYPE,
        1,
        format!("Sizes: {} (ratio {:.1}:1)", px_list.join(", "), ratio),
    ))
}

// ── monotonous-spacing (whole file) ────────────────────────────────────────────
fn monotonous_spacing(content: &str) -> Option<(&'static Rule, usize, String)> {
    static PX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(?:padding|margin)(?:-(?:top|right|bottom|left))?\s*:\s*(\d+)px").unwrap()
    });
    static REMRE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(?:padding|margin)(?:-(?:top|right|bottom|left))?\s*:\s*([\d.]+)rem")
            .unwrap()
    });
    static GAP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)gap\s*:\s*(\d+)px").unwrap());
    static TW: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\b(?:p|px|py|pt|pb|pl|pr|m|mx|my|mt|mb|ml|mr|gap)-(\d+)\b").unwrap()
    });
    let mut vals: Vec<i64> = Vec::new();
    for c in PX.captures_iter(content) {
        let v: i64 = c[1].parse().unwrap_or(0);
        if v > 0 && v < 200 {
            vals.push(v);
        }
    }
    for c in REMRE.captures_iter(content) {
        let v = (c[1].parse::<f64>().unwrap_or(0.0) * 16.0).round() as i64;
        if v > 0 && v < 200 {
            vals.push(v);
        }
    }
    for c in GAP.captures_iter(content) {
        vals.push(c[1].parse().unwrap_or(0));
    }
    for c in TW.captures_iter(content) {
        vals.push(c[1].parse::<i64>().unwrap_or(0) * 4);
    }
    let rounded: Vec<i64> = vals
        .iter()
        .map(|v| (*v as f64 / 4.0).round() as i64 * 4)
        .collect();
    if rounded.len() < 10 {
        return None;
    }
    let mut counts: HashMap<i64, usize> = HashMap::new();
    for v in &rounded {
        *counts.entry(*v).or_default() += 1;
    }
    let max_count = *counts.values().max().unwrap();
    let pct = max_count as f64 / rounded.len() as f64;
    let unique = counts.keys().filter(|v| **v > 0).count();
    if pct <= 0.6 || unique > 3 {
        return None;
    }
    let dominant = counts
        .iter()
        .max_by_key(|(_, n)| **n)
        .map(|(v, _)| *v)
        .unwrap();
    Some((
        &MONOTONOUS_SPACING,
        1,
        format!(
            "~{dominant}px used {max_count}/{} times ({}%)",
            rounded.len(),
            (pct * 100.0).round()
        ),
    ))
}

// ── dark-glow (whole file) ────────────────────────────────────────────────────
fn dark_glow(content: &str) -> Option<(&'static Rule, usize, String)> {
    static DARK_BG: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)background(?:-color)?\s*:\s*(?:#(?:0[0-9a-f]|1[0-9a-f]|2[0-3])[0-9a-f]{4}\b|#(?:0|1)[0-9a-f]{2}\b|rgb\(\s*\d{1,2}\s*,\s*\d{1,2}\s*,\s*\d{1,2}\s*\))").unwrap()
    });
    static TW_DARK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\bbg-(?:gray|slate|zinc|neutral|stone)-(?:9\d{2}|800)\b").unwrap()
    });
    static SHADOW: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)box-shadow\s*:\s*([^;{}]+)").unwrap());
    static RGB: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)").unwrap());
    static PXVAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+)px").unwrap());

    if !DARK_BG.is_match(content) && !TW_DARK.is_match(content) {
        return None;
    }
    for c in SHADOW.captures_iter(content) {
        let val = &c[1];
        let Some(col) = RGB.captures(val) else {
            continue;
        };
        let (r, g, b): (i64, i64, i64) = (
            col[1].parse().unwrap_or(0),
            col[2].parse().unwrap_or(0),
            col[3].parse().unwrap_or(0),
        );
        if (r.max(g).max(b) - r.min(g).min(b)) < 30 {
            continue; // gray glow — skip
        }
        let px: Vec<i64> = PXVAL
            .captures_iter(val)
            .map(|p| p[1].parse().unwrap_or(0))
            .collect();
        if px.len() >= 3 && px[2] > 4 {
            let line = content[..c.get(0).unwrap().start()].lines().count();
            return Some((
                &DARK_GLOW,
                line,
                format!("Colored glow (rgb({r},{g},{b})) on dark page"),
            ));
        }
    }
    None
}

// ── prose analyzers (full HTML pages only) ────────────────────────────────────
fn strip_comments(content: &str) -> String {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<!--[\s\S]*?-->").unwrap());
    RE.replace_all(content, " ").into_owned()
}
fn strip_html_to_text(html: &str) -> String {
    static SCRIPT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<script\b[^>]*>[\s\S]*?</script>").unwrap());
    static STYLE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<style\b[^>]*>[\s\S]*?</style>").unwrap());
    static COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<!--[\s\S]*?-->").unwrap());
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());
    static WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
    let s = SCRIPT.replace_all(html, " ");
    let s = STYLE.replace_all(&s, " ");
    let s = COMMENT.replace_all(&s, " ");
    let s = TAG.replace_all(&s, " ");
    WS.replace_all(&s, " ").into_owned()
}

fn em_dash_overuse(content: &str) -> Option<(&'static Rule, usize, String)> {
    let text = strip_html_to_text(content);
    let bytes = text.as_bytes();
    let mut count = 0;
    // em-dash char.
    count += text.matches('—').count();
    // "--" followed by a non-space (lookahead-free).
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == b'-' && bytes[i + 1] == b'-' && !bytes[i + 2].is_ascii_whitespace() {
            count += 1;
        }
        i += 1;
    }
    if count < 5 {
        return None;
    }
    Some((&EM_DASH, 1, format!("{count} em-dashes in body text")))
}

fn marketing_buzzword(content: &str) -> Option<(&'static Rule, usize, String)> {
    const BUZZ: &[&str] = &[
        "streamline your",
        "empower your",
        "supercharge your",
        "unleash your",
        "unleash the power",
        "leverage the power",
        "built for the modern",
        "trusted by leading",
        "trusted by the world",
        "best-in-class",
        "industry-leading",
        "world-class",
        "enterprise-grade",
        "next-generation",
        "cutting-edge",
        "transform your business",
        "revolutionize",
        "game-changer",
        "game changing",
        "mission-critical",
        "best of breed",
        "future-proof",
        "future proof",
        "seamless experience",
        "seamlessly integrate",
        "drive engagement",
        "drive growth",
        "drive results",
        "harness the power",
    ];
    let text = strip_html_to_text(content);
    let lower = text.to_ascii_lowercase();
    let mut count = 0;
    let mut first = String::new();
    for phrase in BUZZ {
        let mut from = 0;
        while let Some(rel) = lower[from..].find(phrase) {
            let idx = from + rel;
            count += 1;
            if first.is_empty() {
                let lo = idx.saturating_sub(12);
                let hi = (idx + phrase.len() + 12).min(text.len());
                first = text.get(lo..hi).unwrap_or(phrase).trim().to_string();
            }
            from = idx + phrase.len();
        }
    }
    if count == 0 {
        return None;
    }
    let plural = if count == 1 { "" } else { "s" };
    Some((
        &MARKETING_BUZZWORD,
        1,
        format!("{count} buzzword phrase{plural}: \"{first}\""),
    ))
}

fn numbered_section_markers(content: &str) -> Option<(&'static Rule, usize, String)> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(0[1-9]|1[0-2])\b").unwrap());
    let text = strip_html_to_text(content);
    let mut seen: HashSet<String> = HashSet::new();
    for c in RE.captures_iter(&text) {
        seen.insert(c[1].to_string());
    }
    if seen.len() < 3 {
        return None;
    }
    let mut sorted: Vec<String> = seen.into_iter().collect();
    sorted.sort();
    let sequential = sorted
        .windows(2)
        .filter(|w| w[1].parse::<i64>().unwrap_or(0) == w[0].parse::<i64>().unwrap_or(0) + 1)
        .count();
    if sequential < 2 {
        return None;
    }
    let preview: Vec<String> = sorted.iter().take(6).cloned().collect();
    Some((
        &NUMBERED_SECTIONS,
        1,
        format!("Sequence: {}", preview.join(", ")),
    ))
}

fn aphoristic_cadence(content: &str) -> Option<(&'static Rule, usize, String)> {
    static NOT_A: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\bNot an? [a-z][^.!?]{1,40}[.!]\s+[A-Z][^.!?]{1,60}[.!]").unwrap()
    });
    static REBUTTAL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\b[A-Z][^.!?]{4,80}[.!]\s+(?:No|Just)\s+[a-z][^.!?]{2,60}[.!]").unwrap()
    });
    let text = strip_html_to_text(content);
    let mut count = 0;
    let mut first = String::new();
    for re in [&*NOT_A, &*REBUTTAL] {
        for m in re.find_iter(&text) {
            count += 1;
            if first.is_empty() {
                first = truncate(m.as_str().trim(), 80);
            }
        }
    }
    if count < 3 {
        return None;
    }
    Some((
        &APHORISTIC,
        1,
        format!("{count} aphoristic constructions: \"{first}\""),
    ))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(path: &str, content: &str) -> Vec<&'static str> {
        design_findings(path, content)
            .into_iter()
            .map(|f| f.rule)
            .collect()
    }

    #[test]
    fn overused_font_css_and_google() {
        assert_eq!(
            ids("a.css", "body { font-family: 'Inter', sans-serif; }"),
            ["overused-font"]
        );
        assert!(
            ids("i.html", "fonts.googleapis.com/css2?family=Roboto").contains(&"overused-font")
        );
        assert!(ids("a.css", "body { font-family: 'Söhne'; }").is_empty());
    }

    #[test]
    fn gradient_text_and_gray_on_color() {
        assert!(
            ids(
                "h.css",
                ".t{ background: linear-gradient(#f00,#00f); -webkit-background-clip: text; }"
            )
            .contains(&"gradient-text")
        );
        assert!(
            ids("C.tsx", "<div className=\"bg-blue-600 text-gray-400\">")
                .contains(&"gray-on-color")
        );
        assert!(ids("C.tsx", "<div className=\"bg-white text-gray-400\">").is_empty());
    }

    #[test]
    fn side_tab_and_border_accent() {
        assert!(
            ids(
                "C.tsx",
                "<div className=\"rounded-lg border-l-4 border-red-500\">"
            )
            .contains(&"side-tab")
        );
        assert!(
            ids(
                "c.css",
                ".card{ border-radius: 8px; border-left: 4px solid #e11; }"
            )
            .contains(&"side-tab")
        );
        // neutral border color is not a side-tab
        assert!(!ids("c.css", ".x{ border-left: 4px solid #ccc; }").contains(&"side-tab"));
        assert!(
            ids("C.tsx", "<div className=\"rounded-xl border-t-4\">")
                .contains(&"border-accent-on-rounded")
        );
    }

    #[test]
    fn ai_palette_bounce_layout_broken_image() {
        assert!(
            ids("H.tsx", "<h1 className=\"text-purple-500 text-4xl\">")
                .contains(&"ai-color-palette")
        );
        assert!(ids("c.css", ".x{ animation: bounce 1s; }").contains(&"bounce-easing"));
        assert!(ids("c.css", ".x{ transition: width 0.3s ease; }").contains(&"layout-transition"));
        assert!(!ids("c.css", ".x{ transition: all 0.3s ease; }").contains(&"layout-transition"));
        assert!(ids("i.html", "<img src=\"\">").contains(&"broken-image"));
        assert!(ids("i.html", "<img alt=\"logo\">").contains(&"broken-image"));
    }

    #[test]
    fn cubic_bezier_overshoot_only() {
        assert!(
            ids(
                "c.css",
                ".x{ transition-timing-function: cubic-bezier(0.2, 1.6, 0.3, 1); }"
            )
            .contains(&"bounce-easing")
        );
        assert!(
            !ids(
                "c.css",
                ".x{ transition-timing-function: cubic-bezier(0.2, 0.8, 0.3, 1); }"
            )
            .contains(&"bounce-easing")
        );
    }

    #[test]
    fn flat_type_hierarchy_fires_on_close_sizes() {
        let css = ".a{font-size:16px}.b{font-size:18px}.c{font-size:20px}";
        assert!(ids("t.css", css).contains(&"flat-type-hierarchy"));
        let wide = ".a{font-size:14px}.b{font-size:24px}.c{font-size:48px}";
        assert!(!ids("t.css", wide).contains(&"flat-type-hierarchy"));
    }

    #[test]
    fn prose_rules_only_on_full_pages() {
        let copy = "Streamline your workflow. Supercharge your team. Empower your business.";
        // Bare fragment: no prose findings.
        assert!(ids("frag.html", copy).is_empty());
        // Full document: marketing-buzzword fires.
        let page = format!("<!doctype html><html><body><p>{copy}</p></body></html>");
        assert!(ids("page.html", &page).contains(&"marketing-buzzword"));
    }

    #[test]
    fn non_frontend_files_skipped() {
        assert!(design_findings("main.rs", "// font-family: Inter").is_empty());
    }
}
