use super::App;

/// Map an agent turn event to App state updates.
///
/// This will be expanded to handle streaming chunks, tool calls, completions, etc.
pub(crate) fn handle_turn_event(_app: &mut App, _event_line: &str) {
    // Stub: events.rs will be expanded to parse structured TurnEvent variants
    // and update app.messages, app.active_tools, app.agent_info, etc.
}

/// Map an observer event to App state updates.
///
/// This will be expanded to handle channel status changes, memory ops, peripheral events, etc.
pub(crate) fn handle_observer_event(_app: &mut App, _event_line: &str) {
    // Stub: events.rs will be expanded to parse structured ObserverEvent variants
    // and update app.channel_status, app.memory_activity, app.peripheral_status, etc.
}
