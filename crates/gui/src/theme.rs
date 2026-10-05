use anyhow::{Context as _, Result};
use gpui_kit::{
    component::{ActiveTheme, Colorize, Theme, ThemeMode},
    *,
};
use oxide_i18n::tr;
use oxide_model::Diagnostic;
use serde::{Deserialize, Serialize};
use std::io::Write;

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Preference {
    #[default]
    Auto,
    Light,
    Dark,
}

impl Preference {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Light, Self::Dark];

    pub fn id(self) -> &'static str {
        match self {
            Self::Auto => "theme-auto",
            Self::Light => "theme-light",
            Self::Dark => "theme-dark",
        }
    }

    pub fn label(self) -> String {
        tr!(match self {
            Self::Auto => "theme.auto",
            Self::Light => "theme.light",
            Self::Dark => "theme.dark",
        })
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    theme: Preference,
}

struct Appearance {
    settings: Settings,
    load_error: Option<Diagnostic>,
}

impl Global for Appearance {}

fn path() -> std::path::PathBuf {
    // GUI choices are independent of the language and GUI/TUI view preferences.
    oxide_i18n::preference_path().with_file_name("gui.json")
}

fn load() -> Result<Settings> {
    match std::fs::read(path()) {
        Ok(data) => Ok(serde_json::from_slice(&data)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn init(cx: &mut App) {
    let settings = load().context(Diagnostic::new(
        "error.load_theme",
        "Could not read the theme preference",
    ));
    let load_error = settings.as_ref().err().map(Diagnostic::from_error);
    cx.set_global(Appearance {
        settings: settings.unwrap_or_default(),
        load_error,
    });
    apply(cx.window_appearance(), cx);
}

pub(super) fn take_load_error(cx: &mut App) -> Option<Diagnostic> {
    cx.global_mut::<Appearance>().load_error.take()
}

pub(super) fn preference(cx: &App) -> Preference {
    cx.global::<Appearance>().settings.theme
}

pub(super) fn set_preference(value: Preference, window: &Window, cx: &mut App) -> Result<()> {
    // Save first: a failed write keeps the selected choice and current colors intact.
    let settings = Settings { theme: value };
    (|| -> Result<()> {
        let path = path();
        let parent = path
            .parent()
            .context("Preferences need a parent directory")?;
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut file, &settings)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    })()
    .context(Diagnostic::new(
        "error.save_theme",
        "Could not save the theme preference",
    ))?;
    cx.global_mut::<Appearance>().settings = settings;
    apply(window.appearance(), cx);
    Ok(())
}

pub(super) fn sync(window: &Window, cx: &mut App) {
    if preference(cx) == Preference::Auto {
        apply(window.appearance(), cx);
    }
}

fn apply(appearance: WindowAppearance, cx: &mut App) {
    let mode = match preference(cx) {
        Preference::Auto => ThemeMode::from(appearance),
        Preference::Light => ThemeMode::Light,
        Preference::Dark => ThemeMode::Dark,
    };
    Theme::change(mode, None, cx);
    Theme::update(cx, |theme| {
        // Adwaita 1.10 semantic roles, shared by both modes:
        // https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1-latest/css-variables.html
        let color = |light, dark| -> Hsla {
            rgb(if mode == ThemeMode::Dark { dark } else { light }).into()
        };
        let white = rgb(0xffffff).into();
        let background = color(0xfafafb, 0x222226);
        let view = color(0xffffff, 0x1d1d20);
        let sidebar = color(0xebebed, 0x2e2e32);
        // The dark card role is white at 8% over the window background. Use an
        // opaque composite so virtual list fragments have identical surfaces.
        let card = color(0xffffff, 0x343437);
        let popover = color(0xffffff, 0x36363a);
        let foreground = color(0x303034, 0xffffff);
        let dim = color(0x66666c, 0xb2b2b5);
        let border = color(0xd9d9de, 0x48484d);
        let control = color(0xededf0, 0x44444a);
        let hover = color(0xe8e8ec, 0x414147);
        let pressed = color(0xdcdce2, 0x505057);
        let blue: Hsla = rgb(0x3584e4).into();
        let hover_blue = color(0x4990e7, 0x4990e7);
        let active_blue = color(0x2b70c4, 0x2b70c4);
        let selection = color(0xdbe9fa, 0x34587f);
        let link = color(0x0461be, 0x81d0ff);
        let danger = color(0xe01b24, 0xc01c28);
        let success = color(0x2ec27e, 0x26a269);
        let warning = color(0xe5a50a, 0xcd9309);

        theme.radius = px(6.);
        theme.radius_lg = px(12.);
        theme.shadow = true;
        // Keep keyboard focus inside controls, including clipped virtual rows.
        theme.focus_ring = false;
        theme.background = background;
        theme.foreground = foreground;
        theme.border = border;
        theme.muted = control;
        theme.muted_foreground = dim;
        theme.accent = hover;
        theme.accent_foreground = foreground;
        theme.primary = blue;
        theme.primary_foreground = white;
        theme.primary_hover = hover_blue;
        theme.primary_active = active_blue;
        theme.secondary = control;
        theme.secondary_foreground = foreground;
        theme.secondary_hover = hover;
        theme.secondary_active = pressed;
        theme.button = control;
        theme.button_foreground = foreground;
        theme.button_hover = hover;
        theme.button_active = pressed;
        theme.button_primary = blue;
        theme.button_primary_foreground = white;
        theme.button_primary_hover = hover_blue;
        // GPUI uses this role for persistent selection as well as pressing.
        // Selected buttons, checked switches and navigation must match.
        theme.button_primary_active = blue;
        theme.button_secondary = control;
        theme.button_secondary_foreground = foreground;
        theme.button_secondary_hover = hover;
        theme.button_secondary_active = pressed;
        theme.popover = popover;
        theme.popover_foreground = foreground;
        theme.group_box = card;
        theme.group_box_foreground = foreground;
        theme.accordion = card;
        theme.input = border;
        theme.caret = blue;
        theme.ring = blue;
        theme.link = link;
        theme.link_hover = color(0x034c96, 0xa6dfff);
        theme.link_active = link;
        theme.selection = selection;
        theme.drag_border = blue;
        theme.drop_target = selection;
        theme.sidebar = sidebar;
        theme.sidebar_foreground = foreground;
        theme.sidebar_border = color(0xd7d7dd, 0x1d1d20);
        theme.sidebar_primary = blue;
        theme.sidebar_primary_foreground = white;
        theme.sidebar_accent = hover;
        theme.sidebar_accent_foreground = foreground;
        theme.switch = color(0xd9d9de, 0x505057);
        theme.switch_thumb = white;
        theme.progress_bar = blue;
        theme.slider_bar = blue;
        theme.slider_thumb = white;
        theme.scrollbar = background;
        theme.scrollbar_thumb = color(0xb6b6bd, 0x6a6a73);
        theme.scrollbar_thumb_hover = color(0x8d8d97, 0x93939c);
        theme.title_bar = color(0xffffff, 0x2e2e32);
        theme.title_bar_border = border;
        theme.status_bar = sidebar;
        theme.status_bar_border = border;
        theme.window_border = border;
        theme.overlay = rgb(0x000000).opacity(0.32).into();
        theme.colors.list = view;
        theme.list_active = selection;
        theme.list_active_border = blue;
        theme.list_hover = hover;
        theme.list_even = view;
        theme.list_head = card;
        theme.table = view;
        theme.table_active = selection;
        theme.table_active_border = blue;
        theme.table_hover = hover;
        theme.table_even = view;
        theme.table_head = card;
        theme.table_head_foreground = dim;
        theme.table_foot = card;
        theme.table_foot_foreground = dim;
        theme.table_row_border = border;
        theme.tab = control;
        theme.tab_foreground = dim;
        theme.tab_active = card;
        theme.tab_active_foreground = foreground;
        theme.tab_bar = sidebar;
        theme.tab_bar_segmented = control;
        theme.description_list_label = control;
        theme.description_list_label_foreground = dim;
        theme.skeleton = control;

        theme.danger = danger;
        theme.danger_foreground = white;
        theme.danger_hover = danger.lighten(0.08);
        theme.danger_active = danger.darken(0.08);
        theme.button_danger = danger;
        theme.button_danger_foreground = white;
        theme.button_danger_hover = theme.danger_hover;
        theme.button_danger_active = theme.danger_active;
        theme.success = success;
        theme.success_foreground = white;
        theme.success_hover = success.lighten(0.08);
        theme.success_active = success.darken(0.08);
        theme.button_success = success;
        theme.button_success_foreground = white;
        theme.button_success_hover = theme.success_hover;
        theme.button_success_active = theme.success_active;
        theme.warning = warning;
        theme.warning_foreground = rgb(0x000000).into();
        theme.warning_hover = warning.lighten(0.08);
        theme.warning_active = warning.darken(0.08);
        theme.button_warning = warning;
        theme.button_warning_foreground = theme.warning_foreground;
        theme.button_warning_hover = theme.warning_hover;
        theme.button_warning_active = theme.warning_active;
        theme.info = blue;
        theme.info_foreground = white;
        theme.info_hover = hover_blue;
        theme.info_active = active_blue;
        theme.button_info = blue;
        theme.button_info_foreground = white;
        theme.button_info_hover = hover_blue;
        theme.button_info_active = active_blue;
        // Standalone semantic text needs different light/dark colors from fills.
        theme.red = color(0xc30000, 0xff938c);
        theme.red_light = color(0xfbe5e6, 0x4c2c32);
        theme.green = color(0x007c3d, 0x78e9ab);
        theme.green_light = color(0xe3f5eb, 0x294538);
        theme.yellow = color(0x905400, 0xffc252);
        theme.yellow_light = color(0xfbf0d9, 0x4a3d26);
        theme.blue = link;
        theme.blue_light = selection;
        theme.chart_1 = blue;
        theme.chart_2 = success;
        theme.chart_3 = warning;
        theme.chart_4 = danger;
        theme.chart_5 = color(0x9141ac, 0xdc8add);
        theme.chart_grid = border;
        theme.chart_bullish = theme.green;
        theme.chart_bearish = theme.red;
    });
}

pub(super) fn surface(cx: &App) -> Hsla {
    cx.theme().group_box
}

pub(super) fn selected_node(cx: &App) -> Hsla {
    cx.theme().selection
}
