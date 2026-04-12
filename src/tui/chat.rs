use std::time::Instant;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::theme;
use super::{ChatMessage, ToolStatus};

/// Maximum tool output lines shown before truncation.
const MAX_TOOL_OUTPUT_LINES: usize = 10;

const SPINNER_FRAMES: &[&str] = &["\u{25DC}", "\u{25DD}", "\u{25DE}", "\u{25DF}"];

pub(crate) struct Spinner {
    frames: &'static [&'static str],
}

impl Spinner {
    pub(crate) const fn new() -> Self {
        Self {
            frames: SPINNER_FRAMES,
        }
    }

    pub(crate) fn frame(&self, tick: usize) -> &'static str {
        self.frames[tick % self.frames.len()]
    }
}

pub(crate) struct SpinnerState {
    start: Instant,
    pub(crate) label: String,
}

impl SpinnerState {
    pub(crate) fn new(label: impl Into<String>) -> Self {
        Self {
            start: Instant::now(),
            label: label.into(),
        }
    }

    pub(crate) fn elapsed_secs(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }
}

// ── Main render ────────────────────────────────────────────────────────

/// Render the full chat area including messages and any streaming chunk.
pub(crate) fn render_chat_area(
    frame: &mut Frame,
    area: Rect,
    messages: &[ChatMessage],
    pending_chunk: &str,
    scroll_offset: u16,
) {
    let block = Block::default()
        .title(" Hrafn ")
        .title_style(theme::bold())
        .borders(Borders::ALL)
        .border_style(theme::border());

    let mut lines: Vec<Line<'_>> = Vec::new();

    for msg in messages {
        lines.extend(render_message(msg));
        lines.push(Line::default());
    }

    if !pending_chunk.is_empty() {
        lines.extend(render_pending_chunk(pending_chunk));
    }

    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset, 0))
        .style(theme::style());

    frame.render_widget(paragraph, area);
}

/// Render the spinner status line (shown while agent is processing).
pub(crate) fn render_spinner_line(
    frame: &mut Frame,
    area: Rect,
    state: &SpinnerState,
    tick: usize,
) {
    let spinner = Spinner::new();
    let elapsed = state.elapsed_secs();
    let text = format!(
        "{} {}... ({:.1}s)",
        spinner.frame(tick),
        state.label,
        elapsed
    );

    let line = Line::from(vec![Span::styled(text, theme::dim())]);
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}

// ── Message dispatch ───────────────────────────────────────────────────

fn render_message(msg: &ChatMessage) -> Vec<Line<'_>> {
    match msg {
        ChatMessage::User { text } => render_user(text),
        ChatMessage::Assistant { text } => render_assistant(text),
        ChatMessage::ToolCall { name, args, status } => render_tool_call(name, args, status),
        ChatMessage::ToolResult { name, output } => render_tool_result(name, output),
        ChatMessage::System { text } => render_system(text),
    }
}

// ── User ───────────────────────────────────────────────────────────────

fn render_user(text: &str) -> Vec<Line<'_>> {
    text.lines()
        .map(|line| {
            Line::from(vec![
                Span::styled("> ", theme::bold()),
                Span::styled(line, theme::style()),
            ])
        })
        .collect()
}

// ── Assistant (markdown) ───────────────────────────────────────────────

fn render_assistant(text: &str) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    render_markdown(text)
}

/// Parse markdown into styled ratatui Lines using pulldown-cmark.
fn render_markdown(text: &str) -> Vec<Line<'static>> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(text, opts);

    let base = theme::style();
    let mut style_stack: Vec<Style> = vec![base];
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut in_code_block = false;
    let mut code_block_buf = String::new();
    let mut list_depth: usize = 0;

    for event in parser {
        match event {
            Event::Text(cow) => {
                if in_code_block {
                    code_block_buf.push_str(&cow);
                } else {
                    let style = current_style(&style_stack);
                    current_spans.push(Span::styled(cow.into_string(), style));
                }
            }
            Event::Code(cow) => {
                let style = current_style(&style_stack).add_modifier(Modifier::REVERSED);
                current_spans.push(Span::styled(format!(" {cow} "), style));
            }
            Event::SoftBreak => {
                if in_code_block {
                    code_block_buf.push('\n');
                } else {
                    current_spans.push(Span::raw(" "));
                }
            }
            Event::HardBreak | Event::End(TagEnd::Item) => {
                flush_line(&mut lines, &mut current_spans);
            }

            // Inline style tags
            Event::Start(Tag::Strong) => {
                let new = current_style(&style_stack).add_modifier(Modifier::BOLD);
                style_stack.push(new);
            }
            Event::Start(Tag::Emphasis) => {
                let new = current_style(&style_stack).add_modifier(Modifier::ITALIC);
                style_stack.push(new);
            }
            Event::Start(Tag::Strikethrough) => {
                let new = current_style(&style_stack).add_modifier(Modifier::CROSSED_OUT);
                style_stack.push(new);
            }
            Event::End(
                TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough | TagEnd::Link,
            ) => {
                pop_style(&mut style_stack);
            }

            // Headings
            Event::Start(Tag::Heading { .. }) => {
                flush_line(&mut lines, &mut current_spans);
                let new = base.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
                style_stack.push(new);
            }
            Event::End(TagEnd::Heading(_) | TagEnd::BlockQuote(_)) => {
                pop_style(&mut style_stack);
                flush_line(&mut lines, &mut current_spans);
            }

            // Paragraphs
            Event::End(TagEnd::Paragraph) => {
                flush_line(&mut lines, &mut current_spans);
                lines.push(Line::default());
            }

            // Lists
            Event::Start(Tag::List(_)) => {
                list_depth += 1;
            }
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
            }
            Event::Start(Tag::Item) => {
                flush_line(&mut lines, &mut current_spans);
                let indent = "  ".repeat(list_depth.saturating_sub(1));
                current_spans.push(Span::styled(format!("{indent}\u{2022} "), base));
            }

            // Code blocks
            Event::Start(Tag::CodeBlock(_)) => {
                flush_line(&mut lines, &mut current_spans);
                in_code_block = true;
                code_block_buf.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                let code_style = base.add_modifier(Modifier::DIM);
                for code_line in code_block_buf.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {code_line}"),
                        code_style,
                    )));
                }
                code_block_buf.clear();
                lines.push(Line::default());
            }

            // Block quotes
            Event::Start(Tag::BlockQuote(_)) => {
                flush_line(&mut lines, &mut current_spans);
                let new = base.add_modifier(Modifier::ITALIC);
                style_stack.push(new);
                current_spans.push(Span::styled("\u{2502} ", theme::dim()));
            }

            // Links (show text, ignore URL in terminal)
            Event::Start(Tag::Link { .. }) => {
                let new = current_style(&style_stack).add_modifier(Modifier::UNDERLINED);
                style_stack.push(new);
            }

            // Thematic break
            Event::Rule => {
                flush_line(&mut lines, &mut current_spans);
                lines.push(Line::from(Span::styled(
                    "\u{2500}".repeat(40),
                    theme::dim(),
                )));
            }

            _ => {}
        }
    }

    flush_line(&mut lines, &mut current_spans);

    while lines.last().is_some_and(|l| l.spans.is_empty()) {
        lines.pop();
    }

    lines
}

fn current_style(stack: &[Style]) -> Style {
    stack.last().copied().unwrap_or(theme::style())
}

fn pop_style(stack: &mut Vec<Style>) {
    if stack.len() > 1 {
        stack.pop();
    }
}

fn flush_line(lines: &mut Vec<Line<'static>>, spans: &mut Vec<Span<'static>>) {
    if !spans.is_empty() {
        lines.push(Line::from(std::mem::take(spans)));
    }
}

// ── Tool call ──────────────────────────────────────────────────────────

fn render_tool_call<'a>(name: &'a str, args: &'a str, status: &'a ToolStatus) -> Vec<Line<'a>> {
    let tb = theme::tool_block();
    let tb_dim = theme::tool_block_dim();

    let width: usize = 40;
    let header_label = format!(" tool: {name} ");
    let pad = width.saturating_sub(header_label.len() + 2);
    let header = format!("\u{250C}\u{2500}{header_label}{}", "\u{2500}".repeat(pad));

    let mut lines: Vec<Line<'a>> = Vec::new();
    lines.push(Line::from(Span::styled(header, tb)));

    for line in args.lines() {
        lines.push(Line::from(vec![
            Span::styled("\u{2502} ", tb),
            Span::styled(line, tb_dim),
        ]));
    }

    let status_text = match status {
        ToolStatus::Running(started) => {
            format!(
                "\u{2502} \u{23F3} running {:.1}s",
                started.elapsed().as_secs_f64()
            )
        }
        ToolStatus::Done(d) => {
            format!("\u{2502} \u{2713} done {:.1}s", d.as_secs_f64())
        }
        ToolStatus::Failed(e) => {
            format!("\u{2502} \u{2717} failed: {e}")
        }
    };
    lines.push(Line::from(Span::styled(status_text, tb)));

    let footer = format!("\u{2514}{}", "\u{2500}".repeat(width.saturating_sub(1)));
    lines.push(Line::from(Span::styled(footer, tb)));

    lines
}

// ── Tool result ────────────────────────────────────────────────────────

fn render_tool_result<'a>(name: &'a str, output: &'a str) -> Vec<Line<'a>> {
    let tb = theme::tool_block();
    let tb_dim = theme::tool_block_dim();

    let width: usize = 40;
    let header_label = format!(" result: {name} ");
    let pad = width.saturating_sub(header_label.len() + 2);
    let header = format!("\u{250C}\u{2500}{header_label}{}", "\u{2500}".repeat(pad));

    let mut lines: Vec<Line<'a>> = Vec::new();
    lines.push(Line::from(Span::styled(header, tb)));

    let output_lines: Vec<&str> = output.lines().collect();
    let total = output_lines.len();
    let shown = output_lines.iter().take(MAX_TOOL_OUTPUT_LINES);

    for line in shown {
        lines.push(Line::from(vec![
            Span::styled("\u{2502} ", tb),
            Span::styled(*line, tb_dim),
        ]));
    }

    if total > MAX_TOOL_OUTPUT_LINES {
        let remaining = total - MAX_TOOL_OUTPUT_LINES;
        let truncation = format!("\u{2502} [... {remaining} more lines]");
        lines.push(Line::from(Span::styled(truncation, tb_dim)));
    }

    let footer = format!("\u{2514}{}", "\u{2500}".repeat(width.saturating_sub(1)));
    lines.push(Line::from(Span::styled(footer, tb)));

    lines
}

// ── System ─────────────────────────────────────────────────────────────

fn render_system(text: &str) -> Vec<Line<'_>> {
    text.lines()
        .map(|line| Line::from(Span::styled(line, theme::system())))
        .collect()
}

// ── Streaming indicator ────────────────────────────────────────────────

fn render_pending_chunk(chunk: &str) -> Vec<Line<'_>> {
    let mut lines: Vec<Line<'_>> = chunk
        .lines()
        .map(|line| Line::from(Span::styled(line, theme::dim())))
        .collect();

    if let Some(last) = lines.last_mut() {
        last.spans.push(Span::styled("\u{258C}", theme::style()));
    } else {
        lines.push(Line::from(Span::styled("\u{258C}", theme::style())));
    }

    lines
}
