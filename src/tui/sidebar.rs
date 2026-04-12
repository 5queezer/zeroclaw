use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use super::chat::Spinner;
use super::theme;
use super::{ActiveTool, AgentInfo, ChannelStatus, MemoryOp, PeripheralStatus};

/// Render the sidebar panel showing agent status, channels, memory, and peripherals.
pub(crate) fn render_sidebar(
    frame: &mut Frame,
    area: Rect,
    agent_info: &AgentInfo,
    active_tools: &[ActiveTool],
    channel_status: &[ChannelStatus],
    memory_activity: &[MemoryOp],
    peripheral_status: &[PeripheralStatus],
    tick: usize,
) {
    let block = Block::default()
        .title(" Status ")
        .title_style(theme::sidebar_heading())
        .borders(Borders::ALL)
        .border_style(theme::border());

    let mut lines: Vec<Line> = Vec::new();

    // Agent section
    lines.push(Line::from("Agent").style(theme::sidebar_heading()));
    lines.push(
        Line::from(format!("  {} / {}", agent_info.provider, agent_info.model))
            .style(theme::style()),
    );
    lines.push(
        Line::from(format!(
            "  tokens: {} in / {} out",
            agent_info.input_tokens, agent_info.output_tokens
        ))
        .style(theme::dim()),
    );
    if let Some(cost) = agent_info.cost_usd {
        lines.push(Line::from(format!("  cost: ${cost:.4}")).style(theme::dim()));
    }
    lines.push(Line::from(""));

    // Active tools section
    let spinner = Spinner::new();
    lines.push(Line::from("Tools").style(theme::sidebar_heading()));
    if active_tools.is_empty() {
        lines.push(Line::from("  (idle)").style(theme::dim()));
    } else {
        for tool in active_tools {
            let elapsed = tool.started.elapsed().as_secs_f64();
            let frame_char = spinner.frame(tick);
            lines.push(Line::from(vec![
                Span::styled(format!("  {frame_char} "), theme::style()),
                Span::styled(format!("{} ({:.1}s)", tool.name, elapsed), theme::style()),
            ]));
        }
    }
    lines.push(Line::from(""));

    // Channels section
    lines.push(Line::from("Channels").style(theme::sidebar_heading()));
    if channel_status.is_empty() {
        lines.push(Line::from("  (none)").style(theme::dim()));
    } else {
        for ch in channel_status {
            let (indicator, color) = if ch.connected {
                ("\u{25CF}", Style::new().fg(theme::PRIMARY))
            } else {
                ("\u{25CF}", Style::new().fg(theme::ERROR))
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {indicator} "), color),
                Span::styled(ch.name.as_str(), theme::style()),
            ]));
        }
    }
    lines.push(Line::from(""));

    // Memory section
    lines.push(Line::from("Memory").style(theme::sidebar_heading()));
    if memory_activity.is_empty() {
        lines.push(Line::from("  (no activity)").style(theme::dim()));
    } else {
        for op in memory_activity.iter().rev().take(5) {
            lines
                .push(Line::from(format!("  {} {}", op.operation, op.summary)).style(theme::dim()));
        }
    }
    lines.push(Line::from(""));

    // Peripherals section
    lines.push(Line::from("Peripherals").style(theme::sidebar_heading()));
    if peripheral_status.is_empty() {
        lines.push(Line::from("  (none)").style(theme::dim()));
    } else {
        for p in peripheral_status {
            lines.push(Line::from(format!("  {} [{}]", p.name, p.state)).style(theme::style()));
        }
    }

    // Truncate long lines instead of wrapping so scroll accounting is accurate.
    let inner_width = area.width.saturating_sub(2) as usize;
    let truncated: Vec<Line> = lines
        .into_iter()
        .map(|line| {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            if text.chars().count() > inner_width {
                let truncated_text: String =
                    text.chars().take(inner_width.saturating_sub(1)).collect();
                Line::from(format!("{truncated_text}\u{2026}")).style(line.style)
            } else {
                line
            }
        })
        .collect();

    let inner_height = area.height.saturating_sub(2) as usize;
    let scroll = if truncated.len() > inner_height {
        u16::try_from(truncated.len() - inner_height).unwrap_or(u16::MAX)
    } else {
        0
    };

    let paragraph = Paragraph::new(truncated).block(block).scroll((scroll, 0));
    frame.render_widget(paragraph, area);
}
