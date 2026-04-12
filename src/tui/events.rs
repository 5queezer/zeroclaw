use super::App;

/// Map an agent turn event to App state updates.
///
/// Not yet wired — will handle streaming chunks, tool calls, completions, etc.
pub(crate) fn handle_turn_event(_app: &mut App, _event_line: &str) {
    todo!("handle_turn_event: replace with real TurnEvent mapping")
}

/// Map an observer event to App state updates.
///
/// Not yet wired — will handle channel status changes, memory ops, peripheral events, etc.
pub(crate) fn handle_observer_event(_app: &mut App, _event_line: &str) {
    todo!("handle_observer_event: replace with real ObserverEvent mapping")
}
