use std::time::{Duration, Instant};

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use super::theme;

pub struct StatusInfo<'a> {
    pub model: Option<&'a str>,
    pub branch: Option<String>,
    pub dirty: bool,
    pub context_percent: Option<u8>,
    pub perm_mode: Option<&'a str>,
    pub hint: &'a str,
}

pub fn render_status_bar(frame: &mut Frame, area: Rect, info: &StatusInfo<'_>) {
    let sep = Span::styled("  \u{2502}  ", theme::dim());
    let mut parts: Vec<Span> = Vec::new();

    if let Some(m) = info.model {
        parts.push(Span::styled(m.to_string(), theme::style()));
    }
    if let Some(b) = &info.branch {
        if !parts.is_empty() {
            parts.push(sep.clone());
        }
        let s = if info.dirty {
            format!("{b}*")
        } else {
            b.clone()
        };
        parts.push(Span::styled(s, theme::style()));
    }
    if let Some(p) = info.context_percent {
        if !parts.is_empty() {
            parts.push(sep.clone());
        }
        parts.push(Span::styled(format!("ctx {p}%"), theme::style()));
    }
    if let Some(mode) = info.perm_mode {
        if !parts.is_empty() {
            parts.push(sep.clone());
        }
        let style = if mode == "bypass" {
            Style::new().fg(theme::WARN).add_modifier(Modifier::BOLD)
        } else {
            theme::dim()
        };
        parts.push(Span::styled(mode.to_string(), style));
    }
    if !parts.is_empty() {
        parts.push(sep);
    }
    parts.push(Span::styled(info.hint.to_string(), theme::dim()));

    let line = Line::from(parts);
    frame.render_widget(Paragraph::new(line), area);
}

/// Cached git branch+dirty reader. Refreshes every 5s.
pub struct GitStatus {
    branch: Option<String>,
    dirty: bool,
    last: Instant,
    initialized: bool,
}

impl Default for GitStatus {
    fn default() -> Self {
        Self {
            branch: None,
            dirty: false,
            last: Instant::now(),
            initialized: false,
        }
    }
}

impl GitStatus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&mut self) -> (Option<String>, bool) {
        let stale = !self.initialized || self.last.elapsed() > Duration::from_secs(5);
        if stale {
            self.branch = read_branch();
            self.dirty = self.branch.is_some() && read_dirty();
            self.last = Instant::now();
            self.initialized = true;
        }
        (self.branch.clone(), self.dirty)
    }
}

fn read_branch() -> Option<String> {
    let head = std::fs::read_to_string(".git/HEAD").ok()?;
    if let Some(rest) = head.trim().strip_prefix("ref: refs/heads/") {
        return Some(rest.to_string());
    }
    Some(head.trim().chars().take(8).collect())
}

fn read_dirty() -> bool {
    use std::process::{Command, Stdio};
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    match out {
        Ok(o) => !o.stdout.is_empty(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_info_with_only_hint_still_renders() {
        let info = StatusInfo {
            model: None,
            branch: None,
            dirty: false,
            context_percent: None,
            perm_mode: None,
            hint: "/help",
        };
        use ratatui::backend::TestBackend;
        let backend = TestBackend::new(40, 1);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| {
            let area = f.area();
            render_status_bar(f, area, &info);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let text: String = buf.content.iter().map(|c| c.symbol().to_string()).collect();
        assert!(text.contains("/help"));
    }

    #[test]
    fn status_info_with_everything_renders() {
        let info = StatusInfo {
            model: Some("opus"),
            branch: Some("feat/x".to_string()),
            dirty: true,
            context_percent: Some(23),
            perm_mode: Some("bypass"),
            hint: "/help",
        };
        use ratatui::backend::TestBackend;
        let backend = TestBackend::new(100, 1);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| {
            let area = f.area();
            render_status_bar(f, area, &info);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let text: String = buf.content.iter().map(|c| c.symbol().to_string()).collect();
        assert!(text.contains("opus"));
        assert!(text.contains("feat/x*"));
        assert!(text.contains("ctx 23%"));
        assert!(text.contains("bypass"));
        assert!(text.contains("/help"));
    }
}
