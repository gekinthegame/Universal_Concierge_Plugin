//! Deterministic design-quality auditor — flags the "AI-slop" tells in staged
//! HTML/CSS (overused fonts, gradient text, AI palette, side-tab borders,
//! gray-on-color, flat type hierarchy, monotonous spacing, bounce easing,
//! marketing buzzwords, em-dash overuse, …) with **no LLM and no API key**.
//!
//! This is a faithful Rust port of the **Impeccable** deterministic auditor —
//! specifically its regex rule-engine (`cli/engine/engines/regex/detect-text.mjs`),
//! anti-pattern catalog (`cli/engine/registry/antipatterns.mjs`, descriptions
//! reproduced verbatim) and constants (`shared/constants.mjs`).
//!
//! Impeccable — Copyright 2025-2026 Paul Bakaus — Apache License 2.0
//! <https://github.com/HonestFreak/impeccable> (itself building on Anthropic's
//! frontend-design skill, Apache-2.0, and ehmo's typecraft-guide-skill). Used
//! here under the terms of the Apache License 2.0; see CREDITS.md / NOTICE.
//!
//! It is **advisory**: findings are reported, never enforced. The Concierge's own
//! brand (magenta/cyan-on-dark, gradient headings) intentionally trips
//! `ai-color-palette`/`gradient-text`; the AI/user decides what to act on. The
//! deep browser CSS-cascade path of upstream is intentionally NOT ported — this
//! is the text/HTML-string matcher subset that runs on raw source.

use regex::Regex;
use std::sync::OnceLock;

/// One flagged anti-pattern, mirroring Impeccable's `finding()` shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Stable anti-pattern id (e.g. `overused-font`).
    pub antipattern: String,
    /// Human title.
    pub name: String,
    /// What it is + how to fix it (verbatim from the Impeccable catalog).
    pub description: String,
    /// `warning` | `advisory`.
    pub severity: String,
    /// `slop` | `quality`.
    pub category: String,
    /// 1-based source line.
    pub line: usize,
    /// The matched snippet / a short explanation.
    pub snippet: String,
}

/// Catalog metadata for a ported anti-pattern id: (name, description, severity,
/// category). Descriptions are reproduced verbatim from Impeccable's
/// `registry/antipatterns.mjs` (Apache-2.0).
fn meta(id: &str) -> (&'static str, &'static str, &'static str, &'static str) {
    match id {
        "side-tab" => ("Side-tab accent border",
            "Thick colored border on one side of a card — the most recognizable tell of AI-generated UIs. Use a subtler accent or remove it entirely.",
            "warning", "slop"),
        "border-accent-on-rounded" => ("Border accent on rounded element",
            "Thick accent border on a rounded card — the border clashes with the rounded corners. Remove the border or the border-radius.",
            "warning", "slop"),
        "overused-font" => ("Overused font",
            "Inter, Roboto, Fraunces, Geist, Plus Jakarta Sans, and Space Grotesk are used on so many sites they no longer feel distinctive. Each new wave of AI-generated UIs converges on the same handful of faces. Choose a face that gives your interface personality.",
            "warning", "slop"),
        "single-font" => ("Single font for everything",
            "Only one font family is used for the entire page. Pair a distinctive display font with a refined body font to create typographic hierarchy.",
            "warning", "slop"),
        "flat-type-hierarchy" => ("Flat type hierarchy",
            "Font sizes are too close together — no clear visual hierarchy. Use fewer sizes with more contrast (aim for at least a 1.25 ratio between steps).",
            "warning", "slop"),
        "gradient-text" => ("Gradient text",
            "Gradient text is decorative rather than meaningful — a common AI tell, especially on headings and metrics. Use solid colors for text.",
            "warning", "slop"),
        "ai-color-palette" => ("AI color palette",
            "Purple/violet gradients and cyan-on-dark are the most recognizable tells of AI-generated UIs. Choose a distinctive, intentional palette.",
            "warning", "slop"),
        "monotonous-spacing" => ("Monotonous spacing",
            "The same spacing value used everywhere — no rhythm, no variation. Use tight groupings for related items and generous separations between sections.",
            "warning", "slop"),
        "bounce-easing" => ("Bounce or elastic easing",
            "Bounce and elastic easing feel dated and tacky. Real objects decelerate smoothly — use exponential easing (ease-out-quart/quint/expo) instead.",
            "warning", "slop"),
        "dark-glow" => ("Dark mode with glowing accents",
            "Dark backgrounds with colored box-shadow glows are the default \"cool\" look of AI-generated UIs. Use subtle, purposeful lighting instead — or skip the dark theme entirely.",
            "warning", "slop"),
        "numbered-section-markers" => ("Numbered section markers (01 / 02 / 03)",
            "Numbered display markers as section labels (01, 02, 03) are the AI editorial scaffold one tier deeper than tracked eyebrow chips. If you find yourself reaching for them, choose a different section cadence.",
            "advisory", "slop"),
        "em-dash-overuse" => ("Em-dash overuse",
            "More than two em-dashes (— or --) in body copy is an AI cadence tell. Use commas, colons, periods, or parentheses instead.",
            "warning", "slop"),
        "marketing-buzzword" => ("Marketing buzzword",
            "Generic SaaS phrases (streamline / empower / supercharge / world-class / enterprise-grade / next-generation / cutting-edge / etc) are instant AI tells. Pick a specific verb and noun that says what the product literally does.",
            "warning", "slop"),
        "aphoristic-cadence" => ("Aphoristic-cadence copy",
            "Three or more sections landing on a short rebuttal sentence (\"X. No Y.\" / \"X. Just Y.\") or a manufactured-contrast aphorism (\"Not a feature. A platform.\") reads as AI cadence, not voice. Once is fine; the pattern is the tell.",
            "warning", "slop"),
        "broken-image" => ("Broken or placeholder image",
            "<img> tags with empty src, missing src, or placeholder values ship as broken-image boxes. Use real images, generated assets, or remove the tag.",
            "warning", "quality"),
        "gray-on-color" => ("Gray text on colored background",
            "Gray text looks washed out on colored backgrounds. Use a darker shade of the background color instead, or white/near-white for contrast.",
            "warning", "quality"),
        "layout-transition" => ("Layout property animation",
            "Animating width, height, padding, or margin causes layout thrash and janky performance. Use transform and opacity instead, or grid-template-rows for height animations.",
            "warning", "quality"),
        _ => ("Design anti-pattern", "", "warning", "slop"),
    }
}

fn make(id: &str, line: usize, snippet: String) -> Finding {
    let (name, description, severity, category) = meta(id);
    Finding {
        antipattern: id.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        severity: severity.to_string(),
        category: category.to_string(),
        line,
        snippet,
    }
}

// ── compiled regexes (built once) ──────────────────────────────────────────
struct Re {
    border_tw: Regex, // tailwind border-l-4 etc.
    border_lr: Regex, // border-left: Npx solid …
    border_lr_width: Regex,
    border_inline: Regex,
    border_inline_width: Regex,
    border_jsx: Regex,
    border_tb_tw: Regex, // border-t-4 etc (accent on rounded)
    border_tb: Regex,
    font_family: Regex, // overused font-family
    google_font: Regex, // overused google fonts url
    gradient_clip: Regex,
    gray_on_color: Regex,
    gray_bg: Regex,
    ai_text: Regex, // tailwind text-purple-… on heading
    ai_from: Regex, // from-purple-… gradient
    ai_to: Regex,
    bounce_kw: Regex,
    cubic: Regex,
    transition: Regex,
    transition_prop: Regex,
    img_empty_src: Regex,
    img_tag: Regex,
    rounded: Regex,
    border_radius: Regex,
    safe_el: Regex,
    has_heading_or_big: Regex,
    has_bg_gradient_to: Regex,
    // analyzers
    font_family_any: Regex,
    google_font_any: Regex,
    font_size: Regex,
    clamp_size: Regex,
    pad_margin_px: Regex,
    pad_margin_rem: Regex,
    gap_px: Regex,
    tw_spacing: Regex,
    box_shadow: Regex,
    rgb_dark_bg: Regex,
    tw_dark_bg: Regex,
}

fn res() -> &'static Re {
    static R: OnceLock<Re> = OnceLock::new();
    R.get_or_init(|| Re {
        border_tw: Regex::new(r"(?i)\bborder-[lrse]-(\d+)\b").unwrap(),
        border_lr: Regex::new(r"(?i)border-(?:left|right)\s*:\s*(\d+)px\s+solid[^;]*").unwrap(),
        border_lr_width: Regex::new(r"(?i)border-(?:left|right)-width\s*:\s*(\d+)px").unwrap(),
        border_inline: Regex::new(r"(?i)border-inline-(?:start|end)\s*:\s*(\d+)px\s+solid").unwrap(),
        border_inline_width: Regex::new(r"(?i)border-inline-(?:start|end)-width\s*:\s*(\d+)px").unwrap(),
        border_jsx: Regex::new(r#"(?i)border(?:Left|Right)\s*[:=]\s*["'`](\d+)px\s+solid"#).unwrap(),
        border_tb_tw: Regex::new(r"(?i)\bborder-[tb]-(\d+)\b").unwrap(),
        border_tb: Regex::new(r"(?i)border-(?:top|bottom)\s*:\s*(\d+)px\s+solid").unwrap(),
        font_family: Regex::new(r#"(?i)font-family\s*:\s*['"]?(Inter|Roboto|Open Sans|Lato|Montserrat|Arial|Helvetica|Fraunces|Geist Sans|Geist Mono|Geist|Mona Sans|Plus Jakarta Sans|Space Grotesk|Recoleta|Instrument Sans|Instrument Serif)\b"#).unwrap(),
        google_font: Regex::new(r"(?i)fonts\.googleapis\.com/css2?\?family=(Inter|Roboto|Open\+Sans|Lato|Montserrat|Fraunces|Plus\+Jakarta\+Sans|Space\+Grotesk|Instrument\+Sans|Instrument\+Serif|Mona\+Sans|Geist)\b").unwrap(),
        gradient_clip: Regex::new(r"(?i)background-clip\s*:\s*text|-webkit-background-clip\s*:\s*text").unwrap(),
        gray_on_color: Regex::new(r"\btext-(?:gray|slate|zinc|neutral|stone)-(\d+)\b").unwrap(),
        gray_bg: Regex::new(r"\bbg-(?:red|orange|amber|yellow|lime|green|emerald|teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose)-\d+\b").unwrap(),
        ai_text: Regex::new(r"\btext-(?:purple|violet|indigo)-(\d+)\b").unwrap(),
        ai_from: Regex::new(r"\bfrom-(?:purple|violet|indigo)-(\d+)\b").unwrap(),
        ai_to: Regex::new(r"\bto-(?:purple|violet|indigo|blue|cyan|pink|fuchsia)-\d+\b").unwrap(),
        bounce_kw: Regex::new(r"(?i)animation(?:-name)?\s*:\s*[^;]*\b(bounce|elastic|wobble|jiggle|spring)\b").unwrap(),
        cubic: Regex::new(r"cubic-bezier\(\s*([\d.\-]+)\s*,\s*([\d.\-]+)\s*,\s*([\d.\-]+)\s*,\s*([\d.\-]+)\s*\)").unwrap(),
        transition: Regex::new(r"(?i)transition\s*:\s*([^;{}]+)").unwrap(),
        transition_prop: Regex::new(r"(?i)transition-property\s*:\s*([^;{}]+)").unwrap(),
        img_empty_src: Regex::new(r##"(?i)<img\b[^>]*?\bsrc\s*=\s*(?:""|''|"\s+"|'\s+'|"#"|'#')"##).unwrap(),
        img_tag: Regex::new(r"(?i)<img\b[^>]*>").unwrap(),
        rounded: Regex::new(r"\brounded(?:-\w+)?\b").unwrap(),
        border_radius: Regex::new(r"(?i)border-radius").unwrap(),
        safe_el: Regex::new(r"(?i)<(?:blockquote|nav[\s>]|pre[\s>]|code[\s>]|a\s|input[\s>]|span[\s>])").unwrap(),
        has_heading_or_big: Regex::new(r"(?i)\btext-(?:[2-9]xl|[3-9]xl)\b|<h[1-3]").unwrap(),
        has_bg_gradient_to: Regex::new(r"(?i)\bbg-gradient-to-").unwrap(),
        font_family_any: Regex::new(r"(?i)font-family\s*:\s*([^;}]+)").unwrap(),
        google_font_any: Regex::new(r#"(?i)fonts\.googleapis\.com/css2?\?family=([^&"'\s]+)"#).unwrap(),
        font_size: Regex::new(r"(?i)font-size\s*:\s*([\d.]+)(px|rem|em)\b").unwrap(),
        clamp_size: Regex::new(r"(?i)font-size\s*:\s*clamp\(\s*([\d.]+)(px|rem|em)\s*,\s*[^,]+,\s*([\d.]+)(px|rem|em)\s*\)").unwrap(),
        pad_margin_px: Regex::new(r"(?i)(?:padding|margin)(?:-(?:top|right|bottom|left))?\s*:\s*(\d+)px").unwrap(),
        pad_margin_rem: Regex::new(r"(?i)(?:padding|margin)(?:-(?:top|right|bottom|left))?\s*:\s*([\d.]+)rem").unwrap(),
        gap_px: Regex::new(r"(?i)gap\s*:\s*(\d+)px").unwrap(),
        tw_spacing: Regex::new(r"\b(?:p|px|py|pt|pb|pl|pr|m|mx|my|mt|mb|ml|mr|gap)-(\d+)\b").unwrap(),
        box_shadow: Regex::new(r"(?i)box-shadow\s*:\s*([^;{}]+)").unwrap(),
        rgb_dark_bg: Regex::new(r"(?i)background(?:-color)?\s*:\s*rgb\(\s*(\d{1,2})\s*,\s*(\d{1,2})\s*,\s*(\d{1,2})\s*\)").unwrap(),
        tw_dark_bg: Regex::new(r"\bbg-(?:gray|slate|zinc|neutral|stone)-(?:9\d{2}|800)\b").unwrap(),
    })
}

// ── small predicates (ported from detect-text.mjs) ─────────────────────────
fn is_neutral_border_color(s: &str) -> bool {
    // solid <color> — gray-ish (channel spread < 30) or a named neutral.
    static RE: OnceLock<Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"(?i)solid\s+(#[0-9a-f]{3,8}|rgba?\([^)]+\)|\w+)").unwrap());
    let Some(c) = re.captures(s).and_then(|c| c.get(1)) else {
        return false;
    };
    let c = c.as_str().to_lowercase();
    if matches!(
        c.as_str(),
        "gray" | "grey" | "silver" | "white" | "black" | "transparent" | "currentcolor"
    ) {
        return true;
    }
    let parse = |h: &str| u8::from_str_radix(h, 16).unwrap_or(0) as i32;
    if c.len() == 7 && c.starts_with('#') {
        let (r, g, b) = (parse(&c[1..3]), parse(&c[3..5]), parse(&c[5..7]));
        return r.max(g).max(b) - r.min(g).min(b) < 30;
    }
    if c.len() == 4 && c.starts_with('#') {
        let dup = |i: usize| {
            let d = &c[i..i + 1];
            parse(&format!("{d}{d}"))
        };
        let (r, g, b) = (dup(1), dup(2), dup(3));
        return r.max(g).max(b) - r.min(g).min(b) < 30;
    }
    false
}

/// Whether the document looks like a full page (not a partial/component).
fn is_full_page(content: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    static STRIP: OnceLock<Regex> = OnceLock::new();
    let strip = STRIP.get_or_init(|| Regex::new(r"(?s)<!--.*?-->").unwrap());
    let re = RE.get_or_init(|| Regex::new(r"(?i)<!doctype\s|<html[\s>]|<head[\s>]").unwrap());
    re.is_match(&strip.replace_all(content, ""))
}

fn strip_html_to_text(html: &str) -> String {
    static SCRIPT: OnceLock<Regex> = OnceLock::new();
    static STYLE: OnceLock<Regex> = OnceLock::new();
    static COMMENT: OnceLock<Regex> = OnceLock::new();
    static TAG: OnceLock<Regex> = OnceLock::new();
    static WS: OnceLock<Regex> = OnceLock::new();
    let s = SCRIPT.get_or_init(|| Regex::new(r"(?is)<script\b[^>]*>.*?</script>").unwrap());
    let st = STYLE.get_or_init(|| Regex::new(r"(?is)<style\b[^>]*>.*?</style>").unwrap());
    let c = COMMENT.get_or_init(|| Regex::new(r"(?s)<!--.*?-->").unwrap());
    let t = TAG.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    let w = WS.get_or_init(|| Regex::new(r"\s+").unwrap());
    let out = s.replace_all(html, " ");
    let out = st.replace_all(&out, " ");
    let out = c.replace_all(&out, " ");
    let out = t.replace_all(&out, " ");
    w.replace_all(&out, " ").into_owned()
}

const GENERIC_FONTS: &[&str] = &[
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

/// Audit one HTML/CSS source string and return the design anti-patterns found.
/// Pure + deterministic; safe to call per request.
pub fn audit(content: &str) -> Vec<Finding> {
    let re = res();
    let mut out: Vec<Finding> = Vec::new();

    // ── per-line matchers ──
    for (idx, line) in content.lines().enumerate() {
        let no = idx + 1;
        let rounded = re.rounded.is_match(line);
        let has_radius = re.border_radius.is_match(line);
        let safe = re.safe_el.is_match(line);

        // side-tab (tailwind border-l-N)
        if let Some(c) = re.border_tw.captures(line) {
            let n: i32 = c[1].parse().unwrap_or(0);
            if (rounded && n >= 1) || (!rounded && n >= 4) {
                out.push(make("side-tab", no, c[0].to_string()));
            }
        }
        if let Some(c) = re.border_lr.captures(line) {
            let m0 = &c[0];
            let n: i32 = c[1].parse().unwrap_or(0);
            if !safe
                && !is_neutral_border_color(m0)
                && ((has_radius && n >= 1) || (!has_radius && n >= 3))
            {
                out.push(make(
                    "side-tab",
                    no,
                    m0.trim_end_matches([';', ' ']).to_string(),
                ));
            }
        }
        for re2 in [
            &re.border_lr_width,
            &re.border_inline,
            &re.border_inline_width,
        ] {
            if let Some(c) = re2.captures(line) {
                let n: i32 = c[1].parse().unwrap_or(0);
                if !safe && n >= 3 {
                    out.push(make("side-tab", no, c[0].to_string()));
                }
            }
        }
        if let Some(c) = re.border_jsx.captures(line) {
            if c[1].parse::<i32>().unwrap_or(0) >= 3 {
                out.push(make("side-tab", no, c[0].to_string()));
            }
        }

        // border-accent-on-rounded
        if let Some(c) = re.border_tb_tw.captures(line) {
            if rounded && c[1].parse::<i32>().unwrap_or(0) >= 1 {
                out.push(make("border-accent-on-rounded", no, c[0].to_string()));
            }
        }
        if let Some(c) = re.border_tb.captures(line) {
            if has_radius && c[1].parse::<i32>().unwrap_or(0) >= 3 {
                out.push(make("border-accent-on-rounded", no, c[0].to_string()));
            }
        }

        // overused fonts
        if let Some(c) = re.font_family.captures(line) {
            out.push(make("overused-font", no, c[0].to_string()));
        }
        if let Some(c) = re.google_font.captures(line) {
            out.push(make(
                "overused-font",
                no,
                format!("Google Fonts: {}", c[1].replace('+', " ")),
            ));
        }

        // gradient text
        if re.gradient_clip.is_match(line) && line.to_lowercase().contains("gradient") {
            out.push(make(
                "gradient-text",
                no,
                "background-clip: text + gradient".into(),
            ));
        }
        if line.contains("bg-clip-text") && re.has_bg_gradient_to.is_match(line) {
            out.push(make(
                "gradient-text",
                no,
                "bg-clip-text + bg-gradient".into(),
            ));
        }

        // gray text on colored bg (tailwind)
        if let Some(c) = re.gray_on_color.captures(line) {
            if let Some(bg) = re.gray_bg.find(line) {
                out.push(make(
                    "gray-on-color",
                    no,
                    format!("{} on {}", &c[0], bg.as_str()),
                ));
            }
        }

        // AI palette
        if let Some(c) = re.ai_text.captures(line) {
            if re.has_heading_or_big.is_match(line) {
                out.push(make(
                    "ai-color-palette",
                    no,
                    format!("{} on heading", &c[0]),
                ));
            }
        }
        if let Some(c) = re.ai_from.captures(line) {
            if re.ai_to.is_match(line) {
                out.push(make("ai-color-palette", no, format!("{} gradient", &c[0])));
            }
        }

        // bounce / elastic easing
        if line.contains("animate-bounce") {
            out.push(make(
                "bounce-easing",
                no,
                "animate-bounce (Tailwind)".into(),
            ));
        }
        if let Some(c) = re.bounce_kw.captures(line) {
            out.push(make("bounce-easing", no, c[0].to_string()));
        }
        if let Some(c) = re.cubic.captures(line) {
            let y1: f64 = c[2].parse().unwrap_or(0.0);
            let y2: f64 = c[4].parse().unwrap_or(0.0);
            if !(-0.1..=1.1).contains(&y1) || !(-0.1..=1.1).contains(&y2) {
                out.push(make(
                    "bounce-easing",
                    no,
                    format!("cubic-bezier({}, {}, {}, {})", &c[1], &c[2], &c[3], &c[4]),
                ));
            }
        }

        // layout-property transitions
        for re2 in [&re.transition, &re.transition_prop] {
            if let Some(c) = re2.captures(line) {
                let val = c[1].to_lowercase();
                if !val.contains("all") && layout_prop(&val) {
                    out.push(make(
                        "layout-transition",
                        no,
                        format!("transition: {}", c[1].trim()),
                    ));
                }
            }
        }

        // broken images
        if re.img_empty_src.is_match(line) {
            let m = re.img_empty_src.find(line).unwrap();
            out.push(make("broken-image", no, snippet100(m.as_str())));
        }
        for m in re.img_tag.find_iter(line) {
            let tag = m.as_str();
            if !tag.to_lowercase().contains("src") || !contains_src_attr(tag) {
                out.push(make("broken-image", no, snippet100(tag)));
            }
        }
    }

    // dedup line matchers (same id + snippet within 2 lines)
    dedup(&mut out);

    // ── page-level analyzers (full pages only) ──
    if is_full_page(content) {
        if let Some(f) = analyze_single_font(content) {
            out.push(f);
        }
        if let Some(f) = analyze_flat_hierarchy(content) {
            out.push(f);
        }
        if let Some(f) = analyze_monotonous_spacing(content) {
            out.push(f);
        }
        if let Some(f) = analyze_em_dash(content) {
            out.push(f);
        }
        if let Some(f) = analyze_buzzwords(content) {
            out.push(f);
        }
        if let Some(f) = analyze_numbered_markers(content) {
            out.push(f);
        }
        if let Some(f) = analyze_dark_glow(content) {
            out.push(f);
        }
    }
    out
}

fn layout_prop(val: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(?:(?:max|min)-)?(?:width|height)\b|\bpadding\b|\bmargin\b").unwrap()
    })
    .is_match(val)
}

fn contains_src_attr(tag: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\bsrc\s*=").unwrap())
        .is_match(tag)
}

fn snippet100(s: &str) -> String {
    s.chars().take(100).collect()
}

fn dedup(findings: &mut Vec<Finding>) {
    let mut kept: Vec<Finding> = Vec::with_capacity(findings.len());
    for f in findings.drain(..) {
        let dupe = kept.iter().any(|d| {
            d.antipattern == f.antipattern
                && d.snippet == f.snippet
                && (d.line as i64 - f.line as i64).abs() <= 2
        });
        if !dupe {
            kept.push(f);
        }
    }
    *findings = kept;
}

// ── page analyzers (ported from REGEX_ANALYZERS) ───────────────────────────
const REM: f64 = 16.0;

fn analyze_single_font(content: &str) -> Option<Finding> {
    let re = res();
    let mut fonts: Vec<String> = Vec::new();
    let mut add = |f: String| {
        let f = f.trim().trim_matches(['\'', '"']).to_lowercase();
        if !f.is_empty() && !GENERIC_FONTS.contains(&f.as_str()) && !fonts.contains(&f) {
            fonts.push(f);
        }
    };
    for c in re.font_family_any.captures_iter(content) {
        for f in c[1].split(',') {
            add(f.to_string());
        }
    }
    for c in re.google_font_any.captures_iter(content) {
        for f in c[1].split('|') {
            add(f.split(':').next().unwrap_or("").replace('+', " "));
        }
    }
    if fonts.len() != 1 || content.lines().count() < 20 {
        return None;
    }
    let name = &fonts[0];
    let line = content
        .lines()
        .position(|l| l.to_lowercase().contains(name))
        .map(|i| i + 1)
        .unwrap_or(1);
    Some(make(
        "single-font",
        line,
        format!("only font used is {name}"),
    ))
}

fn analyze_flat_hierarchy(content: &str) -> Option<Finding> {
    let re = res();
    let mut sizes: Vec<f64> = Vec::new();
    let mut add = |px: f64| {
        let r = (px * 10.0).round() / 10.0;
        if px > 0.0 && px < 200.0 && !sizes.contains(&r) {
            sizes.push(r);
        }
    };
    for c in re.font_size.captures_iter(content) {
        let v: f64 = c[1].parse().unwrap_or(0.0);
        add(if &c[2] == "px" { v } else { v * REM });
    }
    for c in re.clamp_size.captures_iter(content) {
        let a: f64 = c[1].parse().unwrap_or(0.0);
        let b: f64 = c[3].parse().unwrap_or(0.0);
        add(if &c[2] == "px" { a } else { a * REM });
        add(if &c[4] == "px" { b } else { b * REM });
    }
    let tw: &[(&str, f64)] = &[
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
        if content
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-'))
            .any(|token| token == *cls)
        {
            add(*px);
        }
    }
    if sizes.len() < 3 {
        return None;
    }
    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let ratio = sizes[sizes.len() - 1] / sizes[0];
    if ratio >= 2.0 {
        return None;
    }
    let labels: Vec<String> = sizes.iter().map(|s| format!("{s}px")).collect();
    Some(make(
        "flat-type-hierarchy",
        1,
        format!("Sizes: {} (ratio {:.1}:1)", labels.join(", "), ratio),
    ))
}

fn analyze_monotonous_spacing(content: &str) -> Option<Finding> {
    let re = res();
    let mut vals: Vec<i64> = Vec::new();
    for c in re.pad_margin_px.captures_iter(content) {
        let v: i64 = c[1].parse().unwrap_or(0);
        if v > 0 && v < 200 {
            vals.push(v);
        }
    }
    for c in re.pad_margin_rem.captures_iter(content) {
        let v = (c[1].parse::<f64>().unwrap_or(0.0) * 16.0).round() as i64;
        if v > 0 && v < 200 {
            vals.push(v);
        }
    }
    for c in re.gap_px.captures_iter(content) {
        vals.push(c[1].parse().unwrap_or(0));
    }
    for c in re.tw_spacing.captures_iter(content) {
        vals.push(c[1].parse::<i64>().unwrap_or(0) * 4);
    }
    let rounded: Vec<i64> = vals
        .iter()
        .map(|v| (*v as f64 / 4.0).round() as i64 * 4)
        .collect();
    if rounded.len() < 10 {
        return None;
    }
    let mut counts: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    for v in &rounded {
        *counts.entry(*v).or_default() += 1;
    }
    let max_count = *counts.values().max().unwrap_or(&0);
    let pct = max_count as f64 / rounded.len() as f64;
    let unique: Vec<i64> = {
        let mut u: Vec<i64> = counts.keys().copied().filter(|v| *v > 0).collect();
        u.sort();
        u
    };
    if pct <= 0.6 || unique.len() > 3 {
        return None;
    }
    let dominant = counts
        .iter()
        .max_by_key(|(_, c)| **c)
        .map(|(v, _)| *v)
        .unwrap_or(0);
    Some(make(
        "monotonous-spacing",
        1,
        format!(
            "~{dominant}px used {max_count}/{} times ({}%)",
            rounded.len(),
            (pct * 100.0).round()
        ),
    ))
}

fn analyze_em_dash(content: &str) -> Option<Finding> {
    let text = strip_html_to_text(content);
    // count em-dash chars + "--" followed by a non-space (the JS `--(?=\S)`).
    let mut count = 0usize;
    let bytes: Vec<char> = text.chars().collect();
    for ch in &bytes {
        if *ch == '—' {
            count += 1;
        }
    }
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == '-' && bytes[i + 1] == '-' {
            let after = bytes.get(i + 2);
            if after.is_some_and(|c| !c.is_whitespace()) {
                count += 1;
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    if count < 5 {
        return None;
    }
    Some(make(
        "em-dash-overuse",
        1,
        format!("{count} em-dashes in body text"),
    ))
}

const BUZZWORDS: &[&str] = &[
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

fn analyze_buzzwords(content: &str) -> Option<Finding> {
    let text = strip_html_to_text(content);
    let lower = text.to_lowercase();
    let lower_chars: Vec<char> = lower.chars().collect();
    let text_chars: Vec<char> = text.chars().collect();
    let mut count = 0usize;
    let mut first = String::new();
    for phrase in BUZZWORDS {
        let mut from = 0usize;
        while let Some(rel) = char_find(&lower_chars, phrase, from) {
            count += 1;
            if first.is_empty() {
                let plen = phrase.chars().count();
                let start = rel.saturating_sub(12);
                let end = (rel + plen + 12).min(text_chars.len());
                first = text_chars[start..end]
                    .iter()
                    .collect::<String>()
                    .trim()
                    .to_string();
            }
            from = rel + phrase.chars().count();
        }
    }
    if count == 0 {
        return None;
    }
    Some(make(
        "marketing-buzzword",
        1,
        format!(
            "{count} buzzword phrase{}: \"{first}\"",
            if count == 1 { "" } else { "s" }
        ),
    ))
}

/// Find `needle` in `haystack` (both as char slices) at or after `from`, returning
/// a char index. Mirrors JS `String.indexOf` over a lowercased copy.
fn char_find(haystack: &[char], needle: &str, from: usize) -> Option<usize> {
    let n: Vec<char> = needle.chars().collect();
    if n.is_empty() || from >= haystack.len() {
        return None;
    }
    let mut i = from;
    while i + n.len() <= haystack.len() {
        if haystack[i..i + n.len()] == n[..] {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn analyze_numbered_markers(content: &str) -> Option<Finding> {
    let text = strip_html_to_text(content);
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\b(0[1-9]|1[0-2])\b").unwrap());
    let mut seen: Vec<String> = Vec::new();
    for c in re.captures_iter(&text) {
        let s = c[1].to_string();
        if !seen.contains(&s) {
            seen.push(s);
        }
    }
    if seen.len() < 3 {
        return None;
    }
    seen.sort();
    let mut sequential = 0;
    for i in 1..seen.len() {
        if seen[i].parse::<i32>().unwrap_or(0) == seen[i - 1].parse::<i32>().unwrap_or(0) + 1 {
            sequential += 1;
        }
    }
    if sequential < 2 {
        return None;
    }
    let sample: Vec<&str> = seen.iter().take(6).map(|s| s.as_str()).collect();
    Some(make(
        "numbered-section-markers",
        1,
        format!("Sequence: {}", sample.join(", ")),
    ))
}

fn analyze_dark_glow(content: &str) -> Option<Finding> {
    let re = res();
    // dark background present? (rgb with all channels small, or a tailwind dark bg)
    let rgb_dark = re.rgb_dark_bg.captures_iter(content).any(|c| {
        let r: i32 = c[1].parse().unwrap_or(99);
        let g: i32 = c[2].parse().unwrap_or(99);
        let b: i32 = c[3].parse().unwrap_or(99);
        r <= 35 && g <= 35 && b <= 35
    });
    let hex_dark = {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r"(?i)background(?:-color)?\s*:\s*#(0[0-9a-f]|1[0-9a-f]|2[0-3])[0-9a-f]{4}\b|background(?:-color)?\s*:\s*#(0|1)[0-9a-f]{2}\b").unwrap()).is_match(content)
    };
    if !(rgb_dark || hex_dark || re.tw_dark_bg.is_match(content)) {
        return None;
    }
    // colored box-shadow with blur > 4px
    static RGB: OnceLock<Regex> = OnceLock::new();
    static PX: OnceLock<Regex> = OnceLock::new();
    let rgb =
        RGB.get_or_init(|| Regex::new(r"(?i)rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)").unwrap());
    let px_re = PX.get_or_init(|| Regex::new(r"(\d+)px|\b0\b").unwrap());
    for c in re.box_shadow.captures_iter(content) {
        let val = &c[1];
        let Some(col) = rgb.captures(val) else {
            continue;
        };
        let (r, g, b): (i32, i32, i32) = (
            col[1].parse().unwrap_or(0),
            col[2].parse().unwrap_or(0),
            col[3].parse().unwrap_or(0),
        );
        if r.max(g).max(b) - r.min(g).min(b) < 30 {
            continue; // gray glow
        }
        // pixel lengths in the shadow (treat a bare 0 as 0px); blur is the 3rd.
        let px: Vec<i32> = px_re
            .find_iter(val)
            .map(|m| m.as_str().trim_end_matches("px").parse().unwrap_or(0))
            .collect();
        if px.len() >= 3 && px[2] > 4 {
            let line = content[..c.get(0).unwrap().start()].lines().count();
            return Some(make(
                "dark-glow",
                line.max(1),
                format!("Colored glow (rgb({r},{g},{b})) on dark page"),
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(html: &str) -> Vec<String> {
        audit(html).into_iter().map(|f| f.antipattern).collect()
    }

    #[test]
    fn overused_font_and_single_font_fire() {
        let html = "<style>body{font-family: Inter, sans-serif;}</style>";
        assert!(ids(html).contains(&"overused-font".to_string()));
    }

    #[test]
    fn gradient_text_fires_only_with_gradient() {
        assert!(
            ids("h1{background:linear-gradient(#f0f,#0ff);-webkit-background-clip:text}")
                .contains(&"gradient-text".to_string())
        );
        // background-clip:text with no gradient on the line should NOT fire
        assert!(!ids("h1{-webkit-background-clip:text}").contains(&"gradient-text".to_string()));
    }

    #[test]
    fn side_tab_thick_border_fires_but_neutral_does_not() {
        assert!(
            ids(".card{border-radius:8px; border-left: 4px solid #ff2a55;}")
                .contains(&"side-tab".to_string())
        );
        // neutral gray border is allowed
        assert!(!ids(".card{border-left: 4px solid #cccccc;}").contains(&"side-tab".to_string()));
    }

    #[test]
    fn ai_palette_and_bounce_and_gray_on_color() {
        assert!(
            ids("<h1 class=\"text-purple-500\">Hi</h1>").contains(&"ai-color-palette".to_string())
        );
        assert!(
            ids(".x{transition: transform .2s cubic-bezier(.5,-0.6,.5,1.6)}")
                .contains(&"bounce-easing".to_string())
        );
        assert!(ids("<p class=\"text-gray-400 bg-blue-600\">x</p>")
            .contains(&"gray-on-color".to_string()));
    }

    #[test]
    fn broken_image_and_layout_transition() {
        assert!(ids("<img src=\"\">").contains(&"broken-image".to_string()));
        assert!(ids("<img alt=\"no source\">").contains(&"broken-image".to_string()));
        assert!(ids(".x{transition: width .3s ease}").contains(&"layout-transition".to_string()));
    }

    #[test]
    fn page_analyzers_buzzword_and_emdash() {
        let page = "<!doctype html><html><body><p>We supercharge your workflow — fast — clean — sharp — bold — done.</p></body></html>";
        let got = ids(page);
        assert!(got.contains(&"marketing-buzzword".to_string()), "{got:?}");
        assert!(got.contains(&"em-dash-overuse".to_string()), "{got:?}");
    }

    #[test]
    fn a_clean_page_has_no_findings() {
        // tasteful: distinctive font, solid color, no slop tells
        let page = "<!doctype html><html><head><style>\
            h1{font-family:\"Sohne\",serif;font-size:48px;color:#1a1a1a}\
            body{font-family:\"Tiempos\",Georgia,serif;font-size:18px}\
            .btn{transition:transform .2s ease;border-radius:6px}\
            </style></head><body><h1>Real words about a real thing.</h1>\
            <p>This paragraph says what the product does, plainly.</p></body></html>";
        let got = audit(page);
        assert!(got.is_empty(), "expected clean, got: {got:?}");
    }
}
