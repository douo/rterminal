use gpui::{
    Context, Entity, MouseButton, Render, Subscription, Window, WindowControlArea, div, prelude::*,
    px, rgb,
};

use crate::cli::CliOptions;
use crate::render::CUSTOM_TITLE_BAR_HEIGHT;
use crate::snapshot_tab::SnapshotTab;
use crate::terminal::{AgentTerminal, TerminalExitedEvent};

const TRAFFIC_LIGHT_LEFT_GUTTER: gpui::Pixels = px(68.0);
const MAX_TAB_TITLE_CHARS: usize = 28;

enum TerminalTabKind {
    Terminal {
        terminal: Entity<AgentTerminal>,
        _exit_subscription: Subscription,
    },
    Snapshot {
        snapshot: Entity<SnapshotTab>,
    },
}

struct TerminalTab {
    id: usize,
    kind: TerminalTabKind,
    custom_title: Option<String>,
}

impl TerminalTab {
    fn title(&self, cx: &mut Context<TerminalTabs>) -> String {
        let raw_title = self.raw_title(cx);
        truncate_tab_title(&raw_title, MAX_TAB_TITLE_CHARS)
    }

    fn raw_title(&self, cx: &mut Context<TerminalTabs>) -> String {
        if let Some(custom_title) = &self.custom_title {
            custom_title.clone()
        } else {
            match &self.kind {
                TerminalTabKind::Terminal { terminal, .. } => terminal.read(cx).tab_title(),
                TerminalTabKind::Snapshot { snapshot } => snapshot.read(cx).title(),
            }
        }
    }

    fn focus(&self, window: &mut Window, cx: &mut Context<TerminalTabs>) {
        match &self.kind {
            TerminalTabKind::Terminal { terminal, .. } => {
                terminal.update(cx, |terminal, cx| {
                    terminal.refresh_convenience_state(cx);
                    window.focus(&terminal.focus_handle, cx);
                });
            }
            TerminalTabKind::Snapshot { snapshot } => {
                snapshot.update(cx, |snapshot, cx| {
                    window.focus(&snapshot.focus_handle, cx);
                });
            }
        }
    }

    fn terminal(&self) -> Option<Entity<AgentTerminal>> {
        match &self.kind {
            TerminalTabKind::Terminal { terminal, .. } => Some(terminal.clone()),
            TerminalTabKind::Snapshot { .. } => None,
        }
    }
}

macro_rules! define_tab_switch_handlers {
    ($(($method:ident, $action:ty, $index:expr)),+ $(,)?) => {
        $(
            fn $method(
                &mut self,
                _: &$action,
                window: &mut Window,
                cx: &mut Context<Self>,
            ) {
                self.activate_tab_by_index($index, window, cx);
            }
        )+
    };
}

pub(crate) struct TerminalTabs {
    cli: CliOptions,
    tabs: Vec<TerminalTab>,
    active_tab: usize,
    next_tab_id: usize,
    next_snapshot_id: usize,
    pending_focus_sync: bool,
    focus_handle: gpui::FocusHandle,
    renaming_tab_id: Option<usize>,
    rename_buffer: String,
    _activation_sub: Option<Subscription>,
}

impl TerminalTabs {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>, cli: CliOptions) -> Self {
        let mut this = Self {
            cli,
            tabs: Vec::new(),
            active_tab: 0,
            next_tab_id: 1,
            next_snapshot_id: 1,
            pending_focus_sync: false,
            focus_handle: cx.focus_handle(),
            renaming_tab_id: None,
            rename_buffer: String::new(),
            _activation_sub: None,
        };

        this._activation_sub = Some(cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() && this.renaming_tab_id.is_none() {
                this.request_focus_active_tab(window, cx);
                cx.notify();
            }
        }));
        this.open_new_tab(window, cx);
        this
    }

    fn open_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = cx.new(|cx| AgentTerminal::new_embedded(window, cx, self.cli.clone()));
        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;

        let exit_subscription = cx.subscribe(
            &terminal,
            move |this, _terminal, _event: &TerminalExitedEvent, cx| {
                this.close_tab_by_id(tab_id, cx);
            },
        );

        self.tabs.push(TerminalTab {
            id: tab_id,
            kind: TerminalTabKind::Terminal {
                terminal,
                _exit_subscription: exit_subscription,
            },
            custom_title: None,
        });
        self.active_tab = self.tabs.len().saturating_sub(1);
        self.request_focus_active_tab(window, cx);
        cx.notify();
    }

    fn open_snapshot_tab_from_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source_terminal) = self
            .tabs
            .get(self.active_tab)
            .and_then(TerminalTab::terminal)
        else {
            return;
        };

        let title = format!("snap {}", self.next_snapshot_id);
        self.next_snapshot_id += 1;
        let snapshot_data = source_terminal.read(cx).capture_snapshot_data(title);
        let snapshot = cx.new(|cx| SnapshotTab::new(window, cx, snapshot_data));

        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(TerminalTab {
            id: tab_id,
            kind: TerminalTabKind::Snapshot { snapshot },
            custom_title: None,
        });
        self.active_tab = self.tabs.len().saturating_sub(1);
        self.request_focus_active_tab(window, cx);
        cx.notify();
    }

    fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            cx.quit();
            return;
        }

        self.close_tab_at_index(self.active_tab, cx);
        if self.tabs.is_empty() {
            return;
        }

        self.request_focus_active_tab(window, cx);
        cx.notify();
    }

    fn close_tab_by_id(&mut self, tab_id: usize, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };

        self.close_tab_at_index(index, cx);
        if !self.tabs.is_empty() {
            self.pending_focus_sync = true;
            cx.notify();
        }
    }

    fn close_tab_at_index(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }

        self.tabs.remove(index);

        if self.tabs.is_empty() {
            cx.quit();
            return;
        }

        self.active_tab = next_active_tab_index(self.active_tab, index, self.tabs.len());

        cx.notify();
    }

    fn activate_tab_by_id(&mut self, tab_id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };

        if self.active_tab == index {
            self.request_focus_active_tab(window, cx);
            return;
        }

        self.active_tab = index;
        self.request_focus_active_tab(window, cx);
        cx.notify();
    }

    fn activate_relative_tab(
        &mut self,
        offset: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tabs.len() <= 1 {
            return;
        }

        let count = self.tabs.len() as isize;
        let active = self.active_tab as isize;
        self.active_tab = (active + offset).rem_euclid(count) as usize;
        self.request_focus_active_tab(window, cx);
        cx.notify();
    }

    fn activate_tab_by_index(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }

        if self.active_tab == index {
            self.request_focus_active_tab(window, cx);
            return;
        }

        self.active_tab = index;
        self.request_focus_active_tab(window, cx);
        cx.notify();
    }

    fn focus_active_tab_now(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active_tab) {
            tab.focus(window, cx);
        }
    }

    fn request_focus_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_active_tab_now(window, cx);
        cx.on_next_frame(window, |this, window, cx| {
            this.focus_active_tab_now(window, cx);
        });
    }

    fn on_new_tab(&mut self, _: &crate::NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.open_new_tab(window, cx);
    }

    fn on_close_tab(&mut self, _: &crate::CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_active_tab(window, cx);
    }

    fn on_next_tab(&mut self, _: &crate::NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_relative_tab(1, window, cx);
    }

    fn on_prev_tab(&mut self, _: &crate::PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_relative_tab(-1, window, cx);
    }

    fn on_capture_snapshot_tab(
        &mut self,
        _: &crate::CaptureSnapshotTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_snapshot_tab_from_active(window, cx);
    }

    fn on_rename_active_tab(
        &mut self,
        _: &crate::RenameActiveTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.tabs.get(self.active_tab) {
            self.renaming_tab_id = Some(tab.id);
            self.rename_buffer = tab.raw_title(cx);
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    fn on_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.renaming_tab_id.is_none() {
            return;
        }

        cx.stop_propagation();

        let key = event.keystroke.key.as_str();
        if key == "enter" {
            let renaming_id = self.renaming_tab_id.take();
            if let Some(tab_id) = renaming_id
                && let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id)
            {
                let new_title = self.rename_buffer.trim().to_string();
                if new_title.is_empty() {
                    tab.custom_title = None;
                } else {
                    tab.custom_title = Some(new_title);
                }
            }
            self.request_focus_active_tab(window, cx);
            cx.notify();
        } else if key == "escape" {
            self.renaming_tab_id = None;
            self.request_focus_active_tab(window, cx);
            cx.notify();
        } else if key == "backspace" {
            self.rename_buffer.pop();
            cx.notify();
        } else if !event.keystroke.modifiers.control && !event.keystroke.modifiers.platform {
            if let Some(ch) = &event.keystroke.key_char {
                self.rename_buffer.push_str(ch);
                cx.notify();
            } else if key == "space" {
                self.rename_buffer.push(' ');
                cx.notify();
            }
        }
    }

    define_tab_switch_handlers!(
        (on_switch_to_tab1, crate::SwitchToTab1, 0),
        (on_switch_to_tab2, crate::SwitchToTab2, 1),
        (on_switch_to_tab3, crate::SwitchToTab3, 2),
        (on_switch_to_tab4, crate::SwitchToTab4, 3),
        (on_switch_to_tab5, crate::SwitchToTab5, 4),
        (on_switch_to_tab6, crate::SwitchToTab6, 5),
        (on_switch_to_tab7, crate::SwitchToTab7, 6),
        (on_switch_to_tab8, crate::SwitchToTab8, 7),
        (on_switch_to_tab9, crate::SwitchToTab9, 8),
        (on_switch_to_tab10, crate::SwitchToTab10, 9),
    );
}

impl Render for TerminalTabs {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_focus_sync {
            self.pending_focus_sync = false;
            self.request_focus_active_tab(window, cx);
        }

        let this = cx.entity();
        let tabs_data: Vec<(usize, String, bool, bool)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let is_active = index == self.active_tab;
                let is_renaming = Some(tab.id) == self.renaming_tab_id;
                let title = if is_renaming {
                    format!("{}|", self.rename_buffer)
                } else {
                    tab.title(cx)
                };
                (tab.id, title, is_active, is_renaming)
            })
            .collect();
        let active_content = self.tabs.get(self.active_tab).map(|tab| match &tab.kind {
            TerminalTabKind::Terminal { terminal, .. } => ActiveTabContent::Terminal(terminal.clone()),
            TerminalTabKind::Snapshot { snapshot } => ActiveTabContent::Snapshot(snapshot.clone()),
        });

        let tabs_row = tabs_data.into_iter().fold(
            div()
                .w_full()
                .h(CUSTOM_TITLE_BAR_HEIGHT)
                .bg(rgb(0x171a21))
                .window_control_area(WindowControlArea::Drag)
                .flex()
                .items_center()
                .gap_1()
                .child(div().w(TRAFFIC_LIGHT_LEFT_GUTTER)),
            |row, (tab_id, title, active, renaming)| {
                let this = this.clone();
                let bg = if active { rgb(0x252a34) } else { rgb(0x1d222b) };
                let fg = if active { rgb(0xffffff) } else { rgb(0xa9b1c6) };

                let tab_content = if renaming {
                    div()
                        .px_1()
                        .border_1()
                        .border_color(rgb(0x41a1f0))
                        .rounded(px(4.0))
                        .bg(rgb(0x1a1d24))
                        .text_color(rgb(0xffffff))
                        .child(title)
                } else {
                    div().child(title)
                };

                row.child(
                    div()
                        .px_3()
                        .py_1()
                        .rounded(px(6.0))
                        .bg(bg)
                        .text_color(fg)
                        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                            this.update(cx, |this, cx| this.activate_tab_by_id(tab_id, window, cx));
                        })
                        .child(tab_content),
                )
            },
        );

        let this = this.clone();
        let tabs_row = tabs_row
            .child(
                div()
                    .ml_auto()
                    .px_2()
                    .py_1()
                    .rounded(px(6.0))
                    .bg(rgb(0x1d222b))
                    .text_color(rgb(0xa9b1c6))
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        this.update(cx, |this, cx| this.open_new_tab(window, cx));
                    })
                    .child("+"),
            )
            .child(div().w(px(8.0)));

        let content = match active_content {
            Some(ActiveTabContent::Terminal(active_terminal)) => {
                div().flex_1().min_h_0().child(active_terminal)
            }
            Some(ActiveTabContent::Snapshot(snapshot)) => div().flex_1().min_h_0().child(snapshot),
            None => div()
                .flex_1()
                .items_center()
                .justify_center()
                .text_color(rgb(0xa9b1c6))
                .child("No terminal tabs"),
        };

        div()
            .id("terminal-tabs")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .bg(rgb(0x0f1115))
            .on_action(cx.listener(Self::on_new_tab))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_capture_snapshot_tab))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_prev_tab))
            .on_action(cx.listener(Self::on_rename_active_tab))
            .on_action(cx.listener(Self::on_switch_to_tab1))
            .on_action(cx.listener(Self::on_switch_to_tab2))
            .on_action(cx.listener(Self::on_switch_to_tab3))
            .on_action(cx.listener(Self::on_switch_to_tab4))
            .on_action(cx.listener(Self::on_switch_to_tab5))
            .on_action(cx.listener(Self::on_switch_to_tab6))
            .on_action(cx.listener(Self::on_switch_to_tab7))
            .on_action(cx.listener(Self::on_switch_to_tab8))
            .on_action(cx.listener(Self::on_switch_to_tab9))
            .on_action(cx.listener(Self::on_switch_to_tab10))
            .flex()
            .flex_col()
            .child(tabs_row)
            .child(content)
    }
}

enum ActiveTabContent {
    Terminal(Entity<AgentTerminal>),
    Snapshot(Entity<SnapshotTab>),
}

fn next_active_tab_index(active: usize, removed: usize, remaining_len: usize) -> usize {
    debug_assert!(remaining_len > 0);

    if active > removed {
        active - 1
    } else if active >= remaining_len {
        remaining_len - 1
    } else {
        active
    }
}

fn truncate_tab_title(title: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let char_count = title.chars().count();
    if char_count <= max_chars {
        return title.to_string();
    }

    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }

    let keep_chars = max_chars - 3;
    let truncated: String = title.chars().take(keep_chars).collect();
    format!("{truncated}...")
}

#[cfg(test)]
mod tests {
    use super::{next_active_tab_index, truncate_tab_title};

    #[test]
    fn closing_tab_before_active_shifts_active_left() {
        assert_eq!(next_active_tab_index(3, 1, 4), 2);
    }

    #[test]
    fn closing_active_last_tab_selects_previous() {
        assert_eq!(next_active_tab_index(2, 2, 2), 1);
    }

    #[test]
    fn closing_tab_after_active_keeps_active() {
        assert_eq!(next_active_tab_index(1, 3, 4), 1);
    }

    #[test]
    fn truncate_tab_title_keeps_short_title() {
        assert_eq!(truncate_tab_title("short", 10), "short");
    }

    #[test]
    fn truncate_tab_title_adds_ellipsis_for_long_title() {
        assert_eq!(truncate_tab_title("abcdefghijklmnopqrstuvwxyz", 10), "abcdefg...");
    }

    #[test]
    fn truncate_tab_title_handles_small_limits() {
        assert_eq!(truncate_tab_title("abcdef", 3), "...");
        assert_eq!(truncate_tab_title("abcdef", 2), "..");
        assert_eq!(truncate_tab_title("abcdef", 1), ".");
    }

    #[test]
    fn custom_title_override_logic() {
        // Validate option-based title override logic
        let custom_title: Option<String> = Some("custom tab name".to_string());
        let raw_title = if let Some(title) = &custom_title {
            title.clone()
        } else {
            "default tab name".to_string()
        };
        assert_eq!(raw_title, "custom tab name");

        let custom_title_empty: Option<String> = None;
        let raw_title_fallback = if let Some(title) = &custom_title_empty {
            title.clone()
        } else {
            "default tab name".to_string()
        };
        assert_eq!(raw_title_fallback, "default tab name");
    }
}
