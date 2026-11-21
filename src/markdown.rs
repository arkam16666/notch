//! Tiny Markdown → NSAttributedString for Claude's answers in the notch.
//! Handles what chat answers actually use: headings, bullets, numbered
//! lists, **bold**, `inline code` and ``` code blocks. Everything else is
//! shown as plain text.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AllocAnyThread;
use objc2_app_kit::{
    NSColor, NSFont, NSFontAttributeName, NSFontWeightBold, NSFontWeightRegular, NSFontWeightSemibold,
    NSForegroundColorAttributeName, NSMutableParagraphStyle, NSParagraphStyleAttributeName,
};
use objc2_foundation::{NSAttributedString, NSAttributedStringKey, NSDictionary, NSMutableAttributedString, NSString};

#[derive(Clone, Copy)]
enum Style {
    Body,
    Bold,
    Code,
    Heading(f64),
    Muted,
}

pub struct Theme {
    pub size: f64,
}

fn piece(text: &str, style: Style, t: &Theme, para: &NSMutableParagraphStyle) -> Retained<NSAttributedString> {
    let (font, color) = unsafe {
        match style {
            Style::Body => (NSFont::systemFontOfSize_weight(t.size, NSFontWeightRegular), NSColor::colorWithWhite_alpha(1.0, 0.9)),
            Style::Bold => (NSFont::systemFontOfSize_weight(t.size, NSFontWeightSemibold), NSColor::colorWithWhite_alpha(1.0, 1.0)),
            Style::Code => (
                NSFont::monospacedSystemFontOfSize_weight(t.size - 1.0, NSFontWeightRegular),
                NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.76, 0.62, 1.0),
            ),
            Style::Heading(s) => (NSFont::systemFontOfSize_weight(s, NSFontWeightBold), NSColor::colorWithWhite_alpha(1.0, 1.0)),
            Style::Muted => (NSFont::systemFontOfSize_weight(t.size - 0.5, NSFontWeightRegular), NSColor::colorWithWhite_alpha(1.0, 0.5)),
        }
    };
    unsafe {
        let keys: [&NSAttributedStringKey; 3] =
            [NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName];
        let vals: [&AnyObject; 3] = [&font, &color, para];
        let attrs = NSDictionary::from_slices(&keys, &vals);
        NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &NSString::from_str(text), Some(&attrs))
    }
}

/// Split a line into (text, style) spans for **bold** and `code`.
fn inline(line: &str, base: Style) -> Vec<(String, Style)> {
    let mut out = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let next_bold = rest.find("**");
        let next_code = rest.find('`');
        let (pos, is_bold) = match (next_bold, next_code) {
            (Some(b), Some(c)) if b < c => (b, true),
            (Some(b), None) => (b, true),
            (_, Some(c)) => (c, false),
            (None, None) => {
                out.push((rest.to_string(), base));
                break;
            }
        };
        let marker = if is_bold { "**" } else { "`" };
        let after = &rest[pos + marker.len()..];
        match after.find(marker) {
            Some(end) => {
                if pos > 0 {
                    out.push((rest[..pos].to_string(), base));
                }
                if is_bold {
                    // `code` can sit inside **bold**
                    out.extend(inline(&after[..end], Style::Bold));
                } else {
                    out.push((after[..end].to_string(), Style::Code));
                }
                rest = &after[end + marker.len()..];
            }
            None => {
                out.push((rest.to_string(), base));
                break;
            }
        }
    }
    out
}

/// Question (muted) + rendered answer.
pub fn render(question: &str, answer: &str, t: &Theme) -> Retained<NSMutableAttributedString> {
    let out = NSMutableAttributedString::new();
    let para = NSMutableParagraphStyle::new();
    para.setLineSpacing(3.0);
    para.setParagraphSpacing(7.0);
    let add = |text: &str, style: Style| out.appendAttributedString(&piece(text, style, t, &para));

    if !question.is_empty() {
        add(question, Style::Muted);
        add("\n", Style::Body);
    }
    let mut in_code = false;
    let lines: Vec<&str> = answer.lines().collect();
    for (i, raw) in lines.iter().enumerate() {
        let last = i + 1 == lines.len();
        let nl = if last { "" } else { "\n" };
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            add(&format!("{raw}{nl}"), Style::Code);
            continue;
        }
        if trimmed.is_empty() {
            continue; // paragraph spacing already separates blocks
        }
        if let Some(h) = trimmed.strip_prefix("### ") {
            add(&format!("{h}{nl}"), Style::Heading(t.size + 0.5));
        } else if let Some(h) = trimmed.strip_prefix("## ") {
            add(&format!("{h}{nl}"), Style::Heading(t.size + 1.5));
        } else if let Some(h) = trimmed.strip_prefix("# ") {
            add(&format!("{h}{nl}"), Style::Heading(t.size + 2.5));
        } else {
            let indent = raw.len() - trimmed.len();
            let (prefix, body) = if let Some(b) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
                (format!("{}•  ", " ".repeat(indent)), b)
            } else {
                (" ".repeat(indent), trimmed)
            };
            if !prefix.is_empty() {
                add(&prefix, Style::Body);
            }
            for (text, style) in inline(body, Style::Body) {
                add(&text, style);
            }
            add(nl, Style::Body);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn spans() {
        let s = super::inline("a **b** c `d` e", super::Style::Body);
        let texts: Vec<&str> = s.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(texts, ["a ", "b", " c ", "d", " e"]);
        let s = super::inline("unclosed **bold", super::Style::Body);
        assert_eq!(s.len(), 1);
        let s = super::inline("**`x` y**", super::Style::Body);
        let texts: Vec<&str> = s.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(texts, ["x", " y"]);
    }
}
