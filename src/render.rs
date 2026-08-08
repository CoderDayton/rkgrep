//! Output formatting.

use crate::search::Hit;

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

/// Anchored, model-readable rendering of a result set.
///
/// Every span leads with `path:start-end` so a reader — human or model — can
/// go straight to the source rather than searching for the snippet again.
pub fn render_text(hits: &[Hit], show_text: bool, color: bool) -> String {
    let (dim, bold, reset) = if color {
        (DIM, BOLD, RESET)
    } else {
        ("", "", "")
    };
    let mut out = String::new();
    for (i, hit) in hits.iter().enumerate() {
        if i > 0 && show_text {
            out.push('\n');
        }
        out.push_str(bold);
        out.push_str(&hit.anchor());
        out.push_str(reset);
        if let (Some(symbol), Some(kind)) = (&hit.symbol, &hit.kind) {
            out.push_str(&format!(" {dim}({kind} {symbol}){reset}"));
        }
        out.push_str(&format!(" {dim}[{} tok]{reset}", hit.tokens));
        out.push('\n');
        if show_text {
            out.push_str(&hit.text);
            out.push('\n');
        }
    }
    // Callers add their own trailing newline.
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

pub fn render_json(hits: &[Hit]) -> String {
    serde_json::to_string_pretty(hits).unwrap_or_else(|_| "[]".to_string())
}
