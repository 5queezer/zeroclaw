use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Paragraph},
};

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
) {
    let block = Block::default()
        .title(" Status ")
        .title_style(theme::bold())
        .borders(Borders::ALL)
        .border_style(theme::border());

    let mut lines: Vec<Line> = Vec::new();

    // Agent section
    lines.push(Line::from("Agent").style(theme::bold()));
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
    lines.push(Line::from("Tools").style(theme::bold()));
    if active_tools.is_empty() {
        lines.push(Line::from("  (idle)").style(theme::dim()));
    } else {
        for tool in active_tools {
            let elapsed = tool.started.elapsed().as_secs_f64();
            lines.push(
                Line::from(format!("  {} ({:.1}s)", tool.name, elapsed)).style(theme::style()),
            );
        }
    }
    lines.push(Line::from(""));

    // Channels section
    lines.push(Line::from("Channels").style(theme::bold()));
    if channel_status.is_empty() {
        lines.push(Line::from("  (none)").style(theme::dim()));
    } else {
        for ch in channel_status {
            let indicator = if ch.connected { "+" } else { "-" };
            lines.push(Line::from(format!("  {indicator} {}", ch.name)).style(theme::style()));
        }
    }
    lines.push(Line::from(""));

    // Memory section
    lines.push(Line::from("Memory").style(theme::bold()));
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
    lines.push(Line::from("Peripherals").style(theme::bold()));
    if peripheral_status.is_empty() {
        lines.push(Line::from("  (none)").style(theme::dim()));
    } else {
        for p in peripheral_status {
            lines.push(Line::from(format!("  {} [{}]", p.name, p.state)).style(theme::style()));
        }
    }

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}
