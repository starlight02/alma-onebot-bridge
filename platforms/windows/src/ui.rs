use std::cell::RefCell;
use std::sync::Arc;

use windows_reactor::*;

use crate::app_state::{AppState, BridgeStatus};
use crate::config_model::ConfigModel;
use crate::i18n::{self, tr, Text};
use crate::shell::{open_path, open_url, show_error};

thread_local! {
    static SETTINGS_HOST: RefCell<Option<ReactorHost>> = const { RefCell::new(None) };
    static ABOUT_HOST: RefCell<Option<ReactorHost>> = const { RefCell::new(None) };
}

pub fn show_settings(state: Arc<AppState>) {
    SETTINGS_HOST.with(|slot| {
        if let Some(host) = slot.borrow().as_ref() {
            let _ = host.activate();
            return;
        }

        let root: Box<dyn Component> = Box::new(SettingsRoot {
            state: Arc::clone(&state),
        });
        let host = match ReactorHost::new_with_window_options(
            tr(Text::SettingsWindowTitle),
            Some(WindowSize {
                width: 820.0,
                height: 760.0,
            }),
            InnerConstraints {
                min_width: Some(720.0),
                min_height: Some(620.0),
                ..Default::default()
            },
            root,
            |_| {},
        ) {
            Ok(host) => host,
            Err(error) => {
                report_window_error(tr(Text::SettingsWindowName), &state, error);
                return;
            }
        };
        host.set_backdrop(Backdrop::Mica);
        if let Err(error) = host.activate() {
            report_window_error(tr(Text::SettingsWindowName), &state, error);
        }
        slot.borrow_mut().replace(host);
    });
}

pub fn show_about(state: Arc<AppState>) {
    ABOUT_HOST.with(|slot| {
        if let Some(host) = slot.borrow().as_ref() {
            let _ = host.activate();
            return;
        }

        let root: Box<dyn Component> = Box::new(AboutRoot {
            state: Arc::clone(&state),
        });
        let host = match ReactorHost::new_with_window_options(
            tr(Text::AboutWindowTitle),
            Some(WindowSize {
                width: 540.0,
                height: 440.0,
            }),
            InnerConstraints {
                min_width: Some(480.0),
                min_height: Some(360.0),
                ..Default::default()
            },
            root,
            |_| {},
        ) {
            Ok(host) => host,
            Err(error) => {
                report_window_error(tr(Text::About), &state, error);
                return;
            }
        };
        host.set_backdrop(Backdrop::Mica);
        if let Err(error) = host.activate() {
            report_window_error(tr(Text::About), &state, error);
        }
        slot.borrow_mut().replace(host);
    });
}

fn report_window_error(window_name: &str, state: &Arc<AppState>, error: windows_reactor::Error) {
    tracing::error!(
        window = window_name,
        error = ?error,
        "Failed to open WinUI window"
    );
    show_error(
        "Alma OneBot Bridge",
        &i18n::window_open_failed(window_name, &state.log_file().display().to_string()),
    );
}

struct SettingsRoot {
    state: Arc<AppState>,
}

impl Component for SettingsRoot {
    fn render(&self, _props: &(), cx: &mut RenderCx) -> Element {
        render_settings(&self.state, cx)
    }
}

struct AboutRoot {
    state: Arc<AppState>,
}

impl Component for AboutRoot {
    fn render(&self, _props: &(), _cx: &mut RenderCx) -> Element {
        render_about(&self.state)
    }
}

fn render_settings(state: &Arc<AppState>, cx: &mut RenderCx) -> Element {
    let initial = ConfigModel::load_from(&state.config_file()).unwrap_or_default();
    let (model, set_model) = cx.use_state(initial);
    let (save_message, set_save_message) = cx.use_state(String::new());
    let (refresh, set_refresh) = cx.use_state(0_i32);
    let snapshot = state.snapshot();

    let status_message = snapshot.status_line();

    let save_state = Arc::clone(state);
    let save_model = model.clone();
    let save_set_message = set_save_message.clone();
    let save_set_refresh = set_refresh.clone();
    let save_enabled = model.is_valid();

    let mut content = vec![
        title("Alma OneBot Bridge").into(),
        InfoBar::new(tr(Text::BridgeStatus))
            .message(status_message)
            .severity(match snapshot.status {
                BridgeStatus::Running if snapshot.healthy => InfoBarSeverity::Success,
                BridgeStatus::Failed(_) => InfoBarSeverity::Error,
                BridgeStatus::Starting | BridgeStatus::Stopping => InfoBarSeverity::Informational,
                _ => InfoBarSeverity::Warning,
            })
            .is_closable(false)
            .into(),
        command_row(state, refresh, &set_refresh),
        section(
            tr(Text::BridgeService),
            vec![
                text_field(
                    tr(Text::ListenPort),
                    &model,
                    &set_model,
                    |m| &m.bridge_port,
                    |m, value| m.bridge_port = value,
                    "8090",
                ),
                text_field(
                    tr(Text::AlmaApi),
                    &model,
                    &set_model,
                    |m| &m.alma_api,
                    |m, value| m.alma_api = value,
                    "http://localhost:23001",
                ),
                text_field(
                    tr(Text::ModelOverride),
                    &model,
                    &set_model,
                    |m| &m.alma_model,
                    |m, value| m.alma_model = value,
                    i18n::empty_model_placeholder(),
                ),
                text_field(
                    tr(Text::GenerationTimeout),
                    &model,
                    &set_model,
                    |m| &m.alma_timeout,
                    |m, value| m.alma_timeout = value,
                    "120",
                ),
                text_field(
                    tr(Text::MaxRetries),
                    &model,
                    &set_model,
                    |m| &m.alma_max_retries,
                    |m, value| m.alma_max_retries = value,
                    "2",
                ),
                text_field(
                    tr(Text::RetryDelayMs),
                    &model,
                    &set_model,
                    |m| &m.alma_retry_delay_ms,
                    |m, value| m.alma_retry_delay_ms = value,
                    "3000",
                ),
            ],
        ),
        section(
            tr(Text::OneBotAndChat),
            vec![
                text_field(
                    tr(Text::OneBotTimeout),
                    &model,
                    &set_model,
                    |m| &m.onebot_api_timeout,
                    |m, value| m.onebot_api_timeout = value,
                    "30",
                ),
                password_field(tr(Text::AccessToken), &model, &set_model),
                text_field(
                    tr(Text::GroupHistorySize),
                    &model,
                    &set_model,
                    |m| &m.group_history_size,
                    |m, value| m.group_history_size = value,
                    "30",
                ),
                text_field(
                    tr(Text::ThinkingMessage),
                    &model,
                    &set_model,
                    |m| &m.thinking_message,
                    |m, value| m.thinking_message = value,
                    i18n::empty_disabled_placeholder(),
                ),
                toggle_row(tr(Text::ShowThinkingInQq), model.show_thinking, {
                    let base = model.clone();
                    let set_model = set_model.clone();
                    move |value| {
                        let mut next = base.clone();
                        next.show_thinking = value;
                        set_model.call(next);
                    }
                }),
                toggle_row(tr(Text::ShowToolCalls), model.show_tool_calls, {
                    let base = model.clone();
                    let set_model = set_model.clone();
                    move |value| {
                        let mut next = base.clone();
                        next.show_tool_calls = value;
                        set_model.call(next);
                    }
                }),
                toggle_row(tr(Text::SegmentReplies), model.segmented_replies, {
                    let base = model.clone();
                    let set_model = set_model.clone();
                    move |value| {
                        let mut next = base.clone();
                        next.segmented_replies = value;
                        set_model.call(next);
                    }
                }),
                toggle_row(tr(Text::ListenToGroupMessages), model.listen_group_messages, {
                    let base = model.clone();
                    let set_model = set_model.clone();
                    move |value| {
                        let mut next = base.clone();
                        next.listen_group_messages = value;
                        if !value {
                            next.respond_to_group_messages = false;
                        }
                        set_model.call(next);
                    }
                }),
                toggle_row(
                    tr(Text::RespondToGroupMessages),
                    model.respond_to_group_messages && model.listen_group_messages,
                    {
                        let base = model.clone();
                        let set_model = set_model.clone();
                        move |value| {
                            let mut next = base.clone();
                            if next.listen_group_messages {
                                next.respond_to_group_messages = value;
                            }
                            set_model.call(next);
                        }
                    },
                ),
            ],
        ),
        section(
            tr(Text::Storage),
            vec![
                text_field(
                    tr(Text::DatabasePath),
                    &model,
                    &set_model,
                    |m| &m.db_path,
                    |m, value| m.db_path = value,
                    "bridge-state.db",
                ),
                text_field(
                    tr(Text::PeopleDirectory),
                    &model,
                    &set_model,
                    |m| &m.people_dir,
                    |m, value| m.people_dir = value,
                    "C:\\Users\\you\\.config\\alma\\people",
                ),
            ],
        ),
    ];

    if !save_message.is_empty() {
        content.push(
            InfoBar::new(tr(Text::Settings))
                .message(save_message.clone())
                .severity(if model.is_valid() {
                    InfoBarSeverity::Success
                } else {
                    InfoBarSeverity::Error
                })
                .is_closable(false)
                .into(),
        );
    }

    content.push(
        hstack((
            button(tr(Text::SaveAndRestartBridge))
                .accent()
                .enabled(save_enabled)
                .on_click(move || {
                    if !save_model.is_valid() {
                        save_set_message.call(i18n::invalid_settings_message().to_string());
                        return;
                    }
                    match save_model.save_to(&save_state.config_file()) {
                        Ok(()) => {
                            save_state.restart_bridge();
                            save_set_message.call(i18n::saved_restart_message().to_string());
                            save_set_refresh.call(refresh + 1);
                        }
                        Err(e) => {
                            save_set_message.call(i18n::save_failed_message(&e));
                        }
                    }
                }),
            button(tr(Text::OpenConfig)).subtle().on_click({
                let path = state.config_file();
                move || open_path(&path)
            }),
            button(tr(Text::OpenLog)).subtle().on_click({
                let path = state.log_file();
                move || open_path(&path)
            }),
        ))
        .spacing(10.0)
        .into(),
    );

    scroll_view(vstack(content).spacing(18.0).padding(24.0)).into()
}

fn render_about(state: &Arc<AppState>) -> Element {
    let snapshot = state.snapshot();
    let project_url = "https://github.com/starlight02/alma-onebot-bridge";
    let author_url = "https://github.com/starlight02";
    let license_url = "https://spdx.org/licenses/AGPL-3.0-only";

    vstack((
        title("Alma OneBot Bridge"),
        body(i18n::app_description()).wrap(),
        section(
            tr(Text::Runtime),
            vec![
                about_row(tr(Text::Version), env!("CARGO_PKG_VERSION")),
                about_row(tr(Text::Status), &snapshot.status_line()),
                about_row(tr(Text::ConfigDir), &snapshot.config_dir.to_string_lossy()),
                about_row(tr(Text::Config), &snapshot.config_file.to_string_lossy()),
                about_row(tr(Text::Log), &snapshot.log_file.to_string_lossy()),
            ],
        ),
        section(
            tr(Text::Project),
            vec![
                about_row(tr(Text::Author), "\u{661f}\u{5149}\u{306e}\u{6bb2}\u{6ec5}\u{8005}"),
                about_row(tr(Text::License), "AGPL-3.0-only"),
                hstack((
                    button(tr(Text::Project)).on_click(move || open_url(project_url)),
                    button(tr(Text::Author)).on_click(move || open_url(author_url)),
                    button(tr(Text::License)).on_click(move || open_url(license_url)),
                ))
                .spacing(8.0)
                .into(),
            ],
        ),
    ))
    .spacing(18.0)
    .padding(24.0)
    .into()
}

fn command_row(state: &Arc<AppState>, refresh: i32, set_refresh: &SetState<i32>) -> Element {
    hstack((
        button(tr(Text::Start)).on_click({
            let state = Arc::clone(state);
            let set_refresh = set_refresh.clone();
            move || {
                state.start_bridge();
                set_refresh.call(refresh + 1);
            }
        }),
        button(tr(Text::Stop)).on_click({
            let state = Arc::clone(state);
            let set_refresh = set_refresh.clone();
            move || {
                state.stop_bridge();
                set_refresh.call(refresh + 1);
            }
        }),
        button(tr(Text::Restart)).accent().on_click({
            let state = Arc::clone(state);
            let set_refresh = set_refresh.clone();
            move || {
                state.restart_bridge();
                set_refresh.call(refresh + 1);
            }
        }),
    ))
    .spacing(8.0)
    .into()
}

fn section(title_text: &str, children: Vec<Element>) -> Element {
    vstack((
        subtitle(title_text),
        border(vstack(children).spacing(10.0).padding(16.0))
            .corner_radius(8.0)
            .border_brush(tokens::CardStroke)
            .border_thickness(Thickness::uniform(1.0)),
    ))
    .spacing(8.0)
    .into()
}

fn about_row(label: &str, value: &str) -> Element {
    row(
        label,
        body(value)
            .wrap()
            .selectable()
            .foreground(tokens::SecondaryText)
            .into(),
    )
}

fn text_field(
    label: &str,
    model: &ConfigModel,
    set_model: &SetState<ConfigModel>,
    get: fn(&ConfigModel) -> &String,
    set: fn(&mut ConfigModel, String),
    placeholder: &str,
) -> Element {
    let base = model.clone();
    let setter = set_model.clone();
    row(
        label,
        text_box(get(model).clone())
            .placeholder_text(placeholder)
            .on_text_changed(move |value| {
                let mut next = base.clone();
                set(&mut next, value);
                setter.call(next);
            })
            .width(360.0)
            .into(),
    )
}

fn password_field(label: &str, model: &ConfigModel, set_model: &SetState<ConfigModel>) -> Element {
    let base = model.clone();
    let setter = set_model.clone();
    row(
        label,
            PasswordBox::new()
            .value(model.access_token.clone())
            .placeholder_text(tr(Text::Disabled))
            .on_password_changed(move |value| {
                let mut next = base.clone();
                next.access_token = value;
                setter.call(next);
            })
            .width(360.0)
            .into(),
    )
}

fn toggle_row(label: &str, value: bool, on_change: impl Fn(bool) + 'static) -> Element {
    row(
        label,
        ToggleSwitch::new(value)
            .on_toggled(on_change)
            .on_content(tr(Text::On))
            .off_content(tr(Text::Off))
            .into(),
    )
}

fn row(label: &str, control: Element) -> Element {
    hstack((
        body_strong(label)
            .width(190.0)
            .foreground(tokens::SecondaryText),
        control,
    ))
    .spacing(12.0)
    .vertical_alignment(VerticalAlignment::Center)
    .into()
}
