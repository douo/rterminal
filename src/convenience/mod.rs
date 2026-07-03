pub(crate) mod cursor_indicator;
pub(crate) mod input_method;

use std::time::{Duration, Instant};

pub(crate) use input_method::InputMode;

const INPUT_MODE_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug)]
pub(crate) struct ConvenienceState {
    input_mode: InputMode,
    last_input_mode_refresh_at: Option<Instant>,
}

impl ConvenienceState {
    pub(crate) fn new() -> Self {
        let mut state = Self {
            input_mode: InputMode::Unknown,
            last_input_mode_refresh_at: None,
        };
        state.refresh_input_mode();
        state
    }

    pub(crate) fn input_mode(&self) -> InputMode {
        self.input_mode
    }

    pub(crate) fn refresh_input_mode(&mut self) -> bool {
        self.last_input_mode_refresh_at = Some(Instant::now());
        let next = input_method::current_input_mode();
        if next == self.input_mode {
            return false;
        }
        self.input_mode = next;
        true
    }

    pub(crate) fn refresh_input_mode_if_due(&mut self) -> bool {
        if self
            .last_input_mode_refresh_at
            .is_some_and(|last| last.elapsed() < INPUT_MODE_REFRESH_INTERVAL)
        {
            return false;
        }
        self.refresh_input_mode()
    }
}

impl Default for ConvenienceState {
    fn default() -> Self {
        Self::new()
    }
}
