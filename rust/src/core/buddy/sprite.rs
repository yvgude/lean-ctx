//! Frame selection for the animated mascot. The actual sprite art lives in
//! [`super::mascot_art`]; this module only picks which pre-rendered frame to
//! show for a given animation tick.

use super::types::BuddyState;

/// Return the sprite lines to render for the given animation tick. When frames
/// are available and a tick is provided, cycle through them; otherwise fall
/// back to the static base sprite.
pub(super) fn sprite_lines_for_tick(state: &BuddyState, tick: Option<u64>) -> &[String] {
    if let Some(t) = tick
        && !state.ascii_frames.is_empty()
    {
        let idx = (t as usize) % state.ascii_frames.len();
        return &state.ascii_frames[idx];
    }
    &state.ascii_art
}
