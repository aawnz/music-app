//! The Local Files page, and the player bar while a local song has it.

use std::sync::atomic::Ordering;

use egui::{
    Align, Color32, CornerRadius, FontId, Frame, Layout, Margin, Rect, Sense, UiBuilder, Vec2,
    pos2, vec2,
};

use crate::app::App;
use crate::app::local_files::{LocalAction, LocalSort};
use crate::local_files::{LocalTrack, Repeat};
use crate::theme::{self, Icon};
use crate::util;

use super::widgets::{self, SliderEvent, thin_slider};

const ROW_HEIGHT: f32 = 56.0;
const HEADER_HEIGHT: f32 = 36.0;
const NUMBER_WIDTH: f32 = 40.0;
const DURATION_WIDTH: f32 = 64.0;

fn push(app: &mut App, ui: &egui::Ui, action: LocalAction) {
    app.local_library.actions.push(action);
    ui.ctx().request_repaint();
}

/// One line of text in `rect`, cut with an ellipsis when it does not fit.
fn paint_line(ui: &egui::Ui, text: &str, font: FontId, color: Color32, rect: Rect) {
    let galley = crate::bidi::layout(
        ui.painter(),
        text,
        font,
        color,
        rect.width().max(0.0),
        1,
        Some(crate::bidi::ELLIPSIS),
    );
    let text_rect = Rect::from_min_size(
        pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
        vec2(rect.width(), galley.size().y),
    );
    ui.painter()
        .galley(crate::bidi::galley_pos(text_rect, &galley), galley, color);
}

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    ui.add_space(8.0);
    if !app.local_library.is_loaded() {
        theme::spinner(ui, 24.0, palette.secondary);
        return;
    }
    header(app, ui);
    ui.add_space(16.0);
    controls(app, ui);
    ui.add_space(12.0);
    folders(app, ui);
    scan_progress(app, ui);

    if app.local_library.folders.is_empty() {
        empty_library(app, ui);
        return;
    }
    let rows = app.local_library.visible_rows();
    if rows.is_empty() {
        if app.local_library.scan.is_some() {
            return;
        }
        let (title, body) = if app.local_library.filter.trim().is_empty() {
            (
                "No songs found".to_string(),
                "These folders have no MP3, FLAC, M4A, OGG or WAV files.".to_string(),
            )
        } else {
            (
                format!("No songs match “{}”", app.local_library.filter.trim()),
                "Try another word, or clear the filter.".to_string(),
            )
        };
        widgets::empty_state(ui, &palette, Icon::Music, &title, &body);
        return;
    }
    ui.add_space(8.0);
    table(app, ui, rows);
}

fn header(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    ui.horizontal(|ui| {
        let (tile, _) = ui.allocate_exact_size(Vec2::splat(120.0), Sense::hover());
        ui.painter()
            .rect_filled(tile, CornerRadius::same(theme::RADIUS), palette.accent);
        theme::paint_icon(ui, Icon::Music, tile, 52.0, palette.on_accent);
        ui.add_space(16.0);
        ui.vertical(|ui| {
            ui.add_space(30.0);
            theme::text(
                ui,
                "On this computer",
                theme::medium(13.0),
                palette.secondary,
            );
            theme::text(ui, "Local Files", theme::bold(34.0), palette.text);
            let count = app.local_library.tracks.len();
            let total_ms: u64 = app
                .local_library
                .tracks
                .iter()
                .map(|track| u64::from(track.duration_ms))
                .sum();
            let hours = total_ms / 3_600_000;
            let minutes = (total_ms / 60_000) % 60;
            let length = if hours > 0 {
                format!("{hours} hr {minutes} min")
            } else {
                format!("{minutes} min")
            };
            let summary = match count {
                0 => "No songs yet".to_string(),
                1 => format!("1 song, {length}"),
                count => format!("{count} songs, {length}"),
            };
            theme::text(ui, summary, theme::regular(13.0), palette.secondary);
        });
    });
}

fn controls(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let playing_here = app.local_library.active && app.local_library.shown_playing();
        let has_songs = !app.local_library.tracks.is_empty();
        let icon = if playing_here {
            Icon::PauseFilled
        } else {
            Icon::PlayFilled
        };
        let play = ui.add_enabled_ui(has_songs, |ui| {
            theme::circle_button(
                ui,
                icon,
                52.0,
                palette.accent,
                palette.accent_hover,
                palette.on_accent,
                if playing_here { "Pause" } else { "Play" },
            )
        });
        if play.inner.clicked() {
            if app.local_library.active && app.local_library.shown_track().is_some() {
                push(app, ui, LocalAction::TogglePlay);
            } else {
                let rows = app.local_library.visible_rows();
                if app.local_library.shuffle {
                    push(app, ui, LocalAction::ShuffleAll(rows));
                } else {
                    push(app, ui, LocalAction::Play { rows, index: 0 });
                }
            }
        }
        let shuffle = app.local_library.shuffle;
        if theme::icon_button(
            ui,
            Icon::Shuffle,
            24.0,
            if shuffle {
                palette.accent
            } else {
                palette.secondary
            },
            if shuffle {
                palette.accent_hover
            } else {
                palette.text
            },
            if shuffle { "Shuffle off" } else { "Shuffle on" },
        )
        .clicked()
        {
            push(app, ui, LocalAction::ToggleShuffle);
        }
        if theme::soft_button(ui, &palette, Some(Icon::Plus), "Add folder", false).clicked() {
            push(app, ui, LocalAction::AddFolder);
        }
        if !app.local_library.folders.is_empty() {
            let scanning = app.local_library.scan.is_some();
            let rescan = ui.add_enabled_ui(!scanning, |ui| {
                theme::soft_button(ui, &palette, Some(Icon::Refresh), "Rescan", false)
            });
            if rescan
                .inner
                .on_hover_text("Look for new and changed files")
                .clicked()
            {
                push(app, ui, LocalAction::Rescan);
            }
        }
        if has_songs {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let frame = Frame::new()
                    .fill(palette.surface)
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(10, 6));
                frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        theme::icon(ui, Icon::Search, 14.0, palette.dim);
                        widgets::text_edit(
                            ui,
                            app.locale,
                            egui::TextEdit::singleline(&mut app.local_library.filter)
                                .id(egui::Id::new("local-files-filter"))
                                .hint_text(
                                    egui::RichText::new("Filter local files").color(palette.dim),
                                )
                                .font(theme::regular(13.0))
                                .frame(egui::Frame::NONE)
                                .desired_width(180.0),
                        );
                    });
                });
            });
        }
    });
}

fn folders(app: &mut App, ui: &mut egui::Ui) {
    if app.local_library.folders.is_empty() {
        return;
    }
    let palette = app.palette;
    let folders = app.local_library.folders.clone();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        for folder in folders {
            let name = std::path::Path::new(&folder)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| folder.clone());
            let (response, dismissed) =
                theme::soft_button_dismiss(ui, &palette, Icon::ListMusic, &name);
            let response = response.on_hover_text(format!("{folder}\nClick the icon to remove it"));
            if dismissed {
                push(app, ui, LocalAction::RemoveFolder(folder.clone()));
            } else if response.clicked() {
                push(app, ui, LocalAction::OpenFolder(folder.clone()));
            }
        }
    });
}

fn scan_progress(app: &mut App, ui: &mut egui::Ui) {
    let Some(scan) = &app.local_library.scan else {
        return;
    };
    let palette = app.palette;
    let found = scan.found.load(Ordering::Relaxed);
    let read = scan.read.load(Ordering::Relaxed);
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        theme::spinner(ui, 16.0, palette.secondary);
        let text = if read == 0 {
            format!("Looking for music… {found} files found")
        } else {
            format!("Reading songs… {read} of {found}")
        };
        theme::text(ui, text, theme::regular(13.0), palette.secondary);
    });
}

fn empty_library(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    widgets::empty_state(
        ui,
        &palette,
        Icon::Music,
        "Add your music",
        "Pick a folder with MP3, FLAC, M4A, OGG or WAV files. Folders inside it are included too.",
    );
    ui.add_space(16.0);
    let music = directories::UserDirs::new().and_then(|dirs| {
        dirs.audio_dir()
            .and_then(|dir| dir.to_str().map(str::to_string))
    });
    ui.vertical_centered(|ui| {
        if theme::pill_button(ui, &palette, "Add a folder", true).clicked() {
            push(app, ui, LocalAction::AddFolder);
        }
        if let Some(music) = music {
            ui.add_space(8.0);
            if theme::pill_button(ui, &palette, "Use my Music folder", false)
                .on_hover_text(music.clone())
                .clicked()
            {
                push(app, ui, LocalAction::AddFolderPath(music));
            }
        }
    });
}

fn table(app: &mut App, ui: &mut egui::Ui, rows: Vec<LocalTrack>) {
    let palette = app.palette;
    let width = ui.available_width();
    let show_album = width > 640.0;
    let album_width = if show_album {
        (width * 0.32).min(360.0)
    } else {
        0.0
    };
    let left = ui.cursor().left();
    let title_left = left + NUMBER_WIDTH;
    let album_left = left + width - DURATION_WIDTH - album_width;
    let title_right = if show_album {
        album_left - 16.0
    } else {
        left + width - DURATION_WIDTH - 16.0
    };

    // Column headings, which sort the list.
    let (heading, _) = ui.allocate_exact_size(vec2(width, HEADER_HEIGHT), Sense::hover());
    let mut heading_button = |ui: &mut egui::Ui, rect: Rect, label: &str, sort: LocalSort| {
        let response = ui.interact(rect, egui::Id::new(("local-sort", label)), Sense::click());
        let active = app.local_library.sort == sort;
        let color = if active || response.hovered() {
            palette.text
        } else {
            palette.secondary
        };
        paint_line(ui, label, theme::medium(13.0), color, rect);
        if active && sort != LocalSort::Artist {
            let galley_width = ui
                .painter()
                .layout_no_wrap(label.to_string(), theme::medium(13.0), color)
                .size()
                .x;
            let icon = if app.local_library.sort_descending {
                Icon::ChevronDown
            } else {
                Icon::ChevronUp
            };
            let icon_rect = Rect::from_center_size(
                pos2(rect.left() + galley_width + 10.0, rect.center().y),
                Vec2::splat(14.0),
            );
            icon.image(color, 14.0).paint_at(ui, icon_rect);
        }
        if response.clicked() {
            if app.local_library.sort == sort && sort != LocalSort::Artist {
                app.local_library.sort_descending = !app.local_library.sort_descending;
            } else {
                app.local_library.sort = sort;
                app.local_library.sort_descending = false;
            }
        }
        let _ = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    };
    heading_button(
        ui,
        Rect::from_min_max(
            pos2(left + 8.0, heading.top()),
            pos2(title_left, heading.bottom()),
        ),
        "#",
        LocalSort::Artist,
    );
    heading_button(
        ui,
        Rect::from_min_max(
            pos2(title_left, heading.top()),
            pos2(title_right, heading.bottom()),
        ),
        "Title",
        LocalSort::Title,
    );
    if show_album {
        heading_button(
            ui,
            Rect::from_min_max(
                pos2(album_left, heading.top()),
                pos2(album_left + album_width - 16.0, heading.bottom()),
            ),
            "Album",
            LocalSort::Album,
        );
    }
    let clock = Rect::from_min_max(
        pos2(left + width - DURATION_WIDTH, heading.top()),
        pos2(left + width - 12.0, heading.bottom()),
    );
    let clock_response = ui.interact(clock, egui::Id::new("local-sort-duration"), Sense::click());
    let clock_color = if app.local_library.sort == LocalSort::Duration || clock_response.hovered() {
        palette.text
    } else {
        palette.secondary
    };
    theme::paint_icon(
        ui,
        Icon::Clock,
        Rect::from_center_size(
            pos2(clock.right() - 12.0, clock.center().y),
            Vec2::splat(16.0),
        ),
        16.0,
        clock_color,
    );
    if clock_response.on_hover_text("Sort by length").clicked() {
        if app.local_library.sort == LocalSort::Duration {
            app.local_library.sort_descending = !app.local_library.sort_descending;
        } else {
            app.local_library.sort = LocalSort::Duration;
            app.local_library.sort_descending = false;
        }
    }
    ui.painter().hline(
        heading.x_range(),
        heading.bottom() - 0.5,
        egui::Stroke::new(1.0, palette.outline),
    );
    ui.add_space(8.0);

    // Only the rows on screen are drawn: a library can hold thousands.
    let count = rows.len();
    let (area, _) = ui.allocate_exact_size(vec2(width, ROW_HEIGHT * count as f32), Sense::hover());
    let clip = ui.clip_rect();
    let first = (((clip.top() - area.top()) / ROW_HEIGHT).floor().max(0.0) as usize).min(count);
    let last = (((clip.bottom() - area.top()) / ROW_HEIGHT).ceil().max(0.0) as usize).min(count);
    let playing = app.local_library.shown_playing();
    for index in first..last {
        let track = &rows[index];
        let rect = Rect::from_min_size(
            pos2(area.left(), area.top() + index as f32 * ROW_HEIGHT),
            vec2(width, ROW_HEIGHT),
        );
        let response = ui.interact(
            rect,
            egui::Id::new(("local-file-row", &track.path)),
            Sense::click(),
        );
        let current = app.local_library.is_current(&track.path);
        if response.hovered() {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(6), palette.surface_hover);
        }
        let number_rect = Rect::from_min_max(
            pos2(rect.left() + 8.0, rect.top()),
            pos2(title_left, rect.bottom()),
        );
        if current && playing {
            theme::paint_icon(
                ui,
                Icon::AudioLines,
                Rect::from_center_size(
                    pos2(number_rect.left() + 8.0, rect.center().y),
                    Vec2::splat(16.0),
                ),
                16.0,
                palette.accent,
            );
        } else if response.hovered() {
            theme::paint_icon(
                ui,
                Icon::PlayFilled,
                Rect::from_center_size(
                    pos2(number_rect.left() + 8.0, rect.center().y),
                    Vec2::splat(14.0),
                ),
                14.0,
                palette.text,
            );
        } else {
            paint_line(
                ui,
                &(index + 1).to_string(),
                theme::regular(14.0),
                if current {
                    palette.accent
                } else {
                    palette.secondary
                },
                number_rect,
            );
        }
        let title_color = if current {
            palette.accent
        } else {
            palette.text
        };
        paint_line(
            ui,
            &track.title,
            theme::semibold(15.0),
            title_color,
            Rect::from_min_max(
                pos2(title_left, rect.top() + 8.0),
                pos2(title_right, rect.center().y),
            ),
        );
        paint_line(
            ui,
            track.artist_or_unknown(),
            theme::regular(13.0),
            palette.secondary,
            Rect::from_min_max(
                pos2(title_left, rect.center().y),
                pos2(title_right, rect.bottom() - 8.0),
            ),
        );
        if show_album {
            paint_line(
                ui,
                &track.album,
                theme::regular(14.0),
                palette.secondary,
                Rect::from_min_max(
                    pos2(album_left, rect.top()),
                    pos2(album_left + album_width - 16.0, rect.bottom()),
                ),
            );
        }
        let length = if track.duration_ms > 0 {
            util::format_duration_ms(track.duration_ms)
        } else {
            "--:--".to_string()
        };
        let galley = ui
            .painter()
            .layout_no_wrap(length, theme::regular(13.0), palette.secondary);
        let pos = pos2(
            rect.right() - 12.0 - galley.size().x,
            rect.center().y - galley.size().y / 2.0,
        );
        ui.painter().galley(pos, galley, palette.secondary);

        // A click on the number plays, as does a double click anywhere.
        let number_clicked = response.clicked()
            && response
                .interact_pointer_pos()
                .is_some_and(|pos| number_rect.contains(pos));
        if response.double_clicked() || number_clicked {
            push(
                app,
                ui,
                LocalAction::Play {
                    rows: rows.clone(),
                    index,
                },
            );
        }
        let folder = std::path::Path::new(&track.path)
            .parent()
            .and_then(|folder| folder.to_str())
            .map(str::to_string);
        let mut chosen = None;
        egui::Popup::context_menu(&response)
            .frame(widgets::menu_frame(&palette))
            .show(|ui| {
                if widgets::menu_item(ui, &palette, Some(Icon::Play), "Play") {
                    chosen = Some(LocalAction::Play {
                        rows: rows.clone(),
                        index,
                    });
                }
                // Real Spotify playlists hold local files the same way the
                // official app adds them, so they show up there too.
                let playlists = app.editable_playlists();
                widgets::menu_submenu_with_field(
                    ui,
                    &palette,
                    Some(Icon::ListPlus),
                    "Add to playlist",
                    |ui| {
                        ui.set_min_width(200.0);
                        if playlists.is_empty() {
                            widgets::menu_item_enabled(
                                ui,
                                &palette,
                                None,
                                "No playlists you can edit",
                                false,
                            );
                        }
                        egui::ScrollArea::vertical()
                            .max_height(320.0)
                            .show(ui, |ui| {
                                for (id, name) in &playlists {
                                    if widgets::menu_item(ui, &palette, Some(Icon::ListMusic), name)
                                    {
                                        chosen = Some(LocalAction::AddToPlaylist {
                                            playlist_id: id.clone(),
                                            name: name.clone(),
                                            tracks: vec![track.clone()],
                                        });
                                    }
                                }
                            });
                    },
                );
                if let Some(folder) = &folder
                    && widgets::menu_item(ui, &palette, Some(Icon::ExternalLink), "Show in folder")
                {
                    chosen = Some(LocalAction::OpenFolder(folder.clone()));
                }
            });
        if let Some(action) = chosen {
            push(app, ui, action);
        }
    }
}

/// The player bar while local files have it: the same places as Spotify's
/// bar, for the song playing from this computer.
pub fn player_bar(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    egui::Panel::bottom("player-bar")
        .exact_size(theme::PLAYER_BAR_HEIGHT)
        .resizable(false)
        .show_separator_line(false)
        .frame(
            Frame::new()
                .fill(palette.panel)
                .inner_margin(Margin::symmetric(16, 0)),
        )
        .show(ui, |ui| {
            let rect = ui.max_rect();
            // The same visualizer as for Spotify's songs, behind the bar:
            // the local player feeds the same sound to it.
            let behind = rect.expand2(vec2(16.0, 0.0));
            let sounding = app.local_library.shown_playing();
            // In the colours of the song's own cover, as for Spotify's.
            let colours = app
                .settings
                .accent_from_art
                .then(|| app.local_library.shown_palette().map(<[_]>::to_vec))
                .flatten();
            if crate::ui::player_bar::draw_visualizer(app, ui, behind, sounding, colours) {
                theme::visualiser_frame(ui.ctx(), app.visualiser_fps());
            }
            if app.settings.player_bar_vis_click {
                let empty = ui.interact(
                    behind,
                    ui.id().with("local-player-bar-visualizer"),
                    Sense::click(),
                );
                if empty.clicked() {
                    app.actions.push(crate::model::Action::CyclePlayerBarVis);
                }
            }
            ui.painter().hline(
                rect.x_range(),
                rect.top() + 0.5,
                egui::Stroke::new(1.0, palette.outline),
            );
            let side = (rect.width() * 0.3).clamp(200.0, 420.0);
            let left = Rect::from_min_max(rect.min, pos2(rect.left() + side, rect.bottom()));
            let center = Rect::from_min_max(
                pos2(rect.left() + side, rect.top()),
                pos2(rect.right() - side, rect.bottom()),
            );
            now_playing(app, ui, left);
            transport(app, ui, center);
            let right = Rect::from_min_size(
                pos2(rect.right() - side, rect.center().y - 15.0),
                vec2(side, 30.0),
            );
            let mut right_ui = ui.new_child(
                UiBuilder::new()
                    .max_rect(right)
                    .layout(Layout::right_to_left(Align::Center)),
            );
            extras(app, &mut right_ui);
        });
}

fn now_playing(app: &mut App, ui: &mut egui::Ui, region: Rect) {
    let palette = app.palette;
    let cy = region.center().y;
    let cover_rect = Rect::from_min_size(pos2(region.left() + 4.0, cy - 28.0), Vec2::splat(56.0));
    let track = app.local_library.shown_track();
    let cover = app.local_library.shown_cover().cloned();
    match &cover {
        Some(texture) => {
            egui::Image::new(texture)
                .corner_radius(CornerRadius::same(6))
                .paint_at(ui, cover_rect);
        }
        None => widgets::paint_cover(ui, &palette, None, cover_rect, 6.0, Icon::Music, None),
    }
    // Hovering the cover offers to show it large at the sidebar's bottom,
    // as for Spotify's songs.
    let offer_expand =
        cover.is_some() && !app.settings.art_expanded && app.settings.sidebar_visible;
    if offer_expand {
        let cover_response = ui.interact(
            cover_rect,
            egui::Id::new("local-now-playing-cover"),
            Sense::hover(),
        );
        let expand_rect = Rect::from_center_size(
            pos2(cover_rect.right() - 10.0, cover_rect.top() + 10.0),
            Vec2::splat(18.0),
        );
        if cover_response.hovered() || ui.rect_contains_pointer(expand_rect) {
            let expand = ui.interact(
                expand_rect,
                egui::Id::new("local-now-playing-art-expand"),
                Sense::click(),
            );
            ui.painter().circle_filled(
                expand_rect.center(),
                9.0,
                palette.panel.gamma_multiply(0.9),
            );
            Icon::ChevronUp.image(palette.text, 12.0).paint_at(
                ui,
                Rect::from_center_size(expand_rect.center(), Vec2::splat(12.0)),
            );
            if expand.clicked() {
                app.settings.art_expanded = true;
                app.actions.push(crate::model::Action::SettingsChanged);
            }
        }
    }
    let text_left = cover_rect.right() + 12.0;
    let text_rect = Rect::from_min_size(
        pos2(text_left, cy - 17.0),
        vec2((region.right() - text_left - 8.0).max(40.0), 34.0),
    );
    let mut text_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(text_rect)
            .layout(Layout::top_down(Align::Min)),
    );
    text_ui.spacing_mut().item_spacing.y = 2.0;
    match track {
        Some(track) => {
            theme::text(
                &mut text_ui,
                &track.title,
                theme::medium(14.0),
                palette.text,
            );
            theme::text(
                &mut text_ui,
                track.artist_or_unknown(),
                theme::regular(12.0),
                palette.secondary,
            );
        }
        None => {
            theme::text(
                &mut text_ui,
                "Nothing playing",
                theme::medium(14.0),
                palette.secondary,
            );
            theme::text(
                &mut text_ui,
                "Pick a song from Local Files",
                theme::regular(12.0),
                palette.dim,
            );
        }
    }
}

fn transport(app: &mut App, ui: &mut egui::Ui, region: Rect) {
    let palette = app.palette;
    let cy = region.center().y - 8.0;
    let playing = app.local_library.shown_playing();
    let shuffle = app.local_library.shuffle;
    let repeat = app.local_library.repeat;
    let dim = palette.secondary;

    let widths = [29.0, 30.0, 36.0, 30.0, 29.0];
    let gap = 10.0;
    let total: f32 = widths.iter().sum::<f32>() + gap * 4.0;
    let mut x = region.center().x - total / 2.0;
    let mut slot = |width: f32| {
        let rect = Rect::from_center_size(pos2(x + width / 2.0, cy), vec2(width, 36.0));
        x += width + gap;
        rect
    };
    let centered = |ui: &mut egui::Ui, rect: Rect| {
        ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::centered_and_justified(egui::Direction::LeftToRight)),
        )
    };

    let mut cell = centered(ui, slot(widths[0]));
    if theme::icon_button(
        &mut cell,
        Icon::Shuffle,
        17.0,
        if shuffle { palette.accent } else { dim },
        if shuffle {
            palette.accent_hover
        } else {
            palette.text
        },
        "Shuffle",
    )
    .clicked()
    {
        push(app, ui, LocalAction::ToggleShuffle);
    }

    let mut cell = centered(ui, slot(widths[1]));
    if theme::icon_button(
        &mut cell,
        Icon::SkipBackFilled,
        18.0,
        dim,
        palette.text,
        "Previous",
    )
    .clicked()
    {
        push(app, ui, LocalAction::Previous);
    }

    let mut cell = centered(ui, slot(widths[2]));
    let hover = if palette.dark {
        Color32::WHITE
    } else {
        palette.text
    };
    if theme::circle_button(
        &mut cell,
        if playing {
            Icon::PauseFilled
        } else {
            Icon::PlayFilled
        },
        36.0,
        palette.text,
        hover,
        palette.window,
        if playing { "Pause" } else { "Play" },
    )
    .clicked()
    {
        push(app, ui, LocalAction::TogglePlay);
    }

    let mut cell = centered(ui, slot(widths[3]));
    if theme::icon_button(
        &mut cell,
        Icon::SkipForwardFilled,
        18.0,
        dim,
        palette.text,
        "Next",
    )
    .clicked()
    {
        push(app, ui, LocalAction::Next);
    }

    let (repeat_icon, repeat_color, tooltip) = match repeat {
        Repeat::Off => (Icon::Repeat, dim, "Repeat"),
        Repeat::All => (Icon::Repeat, palette.accent, "Repeat one"),
        Repeat::One => (Icon::Repeat1, palette.accent, "Repeat off"),
    };
    let mut cell = centered(ui, slot(widths[4]));
    if theme::icon_button(
        &mut cell,
        repeat_icon,
        17.0,
        repeat_color,
        if repeat == Repeat::Off {
            palette.text
        } else {
            palette.accent_hover
        },
        tooltip,
    )
    .clicked()
    {
        push(app, ui, LocalAction::CycleRepeat);
    }

    let row_cy = cy + 31.0;
    let slider_width = (region.width() - 120.0).clamp(120.0, 620.0);
    let duration = app.local_library.shown_duration_ms();
    let position = app.local_library.shown_position_ms().min(duration.max(1));
    let shown_position = match app.local_library.seek_preview {
        Some(fraction) => (fraction * duration as f32) as u32,
        None => position,
    };
    let slider_left = region.center().x - slider_width / 2.0;
    ui.painter().text(
        pos2(slider_left - 8.0, row_cy),
        egui::Align2::RIGHT_CENTER,
        util::format_duration_ms(shown_position),
        theme::regular(11.5),
        palette.secondary,
    );
    let slider_rect =
        Rect::from_center_size(pos2(region.center().x, row_cy), vec2(slider_width, 16.0));
    let mut slider_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(slider_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    let fraction = if duration > 0 {
        position as f32 / duration as f32
    } else {
        0.0
    };
    match thin_slider(
        &mut slider_ui,
        &palette,
        egui::Id::new("local-seek-slider"),
        "Playback position (%)",
        fraction,
        slider_width,
        None,
    ) {
        SliderEvent::Dragging(value) => app.local_library.seek_preview = Some(value),
        SliderEvent::Committed(value) => {
            app.local_library.seek_preview = None;
            if duration > 0 {
                push(app, ui, LocalAction::Seek((value * duration as f32) as u32));
            }
        }
        SliderEvent::None => {}
    }
    ui.painter().text(
        pos2(slider_left + slider_width + 8.0, row_cy),
        egui::Align2::LEFT_CENTER,
        if duration > 0 {
            util::format_duration_ms(duration)
        } else {
            "--:--".to_string()
        },
        theme::regular(11.5),
        palette.secondary,
    );
}

fn extras(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    ui.spacing_mut().item_spacing.x = 6.0;
    if theme::icon_button(
        ui,
        Icon::X,
        16.0,
        palette.secondary,
        palette.text,
        "Back to Spotify",
    )
    .clicked()
    {
        push(app, ui, LocalAction::Close);
    }
    ui.add_space(6.0);
    let level = app
        .local_library
        .volume_preview
        .unwrap_or_else(|| app.local_library.effective_volume());
    match thin_slider(
        ui,
        &palette,
        egui::Id::new("local-volume-slider"),
        "Volume (%)",
        level,
        92.0,
        Some(0.05),
    ) {
        SliderEvent::Dragging(value) => {
            app.local_library.volume_preview = Some(value);
            push(app, ui, LocalAction::PreviewVolume(value));
        }
        SliderEvent::Committed(value) => {
            app.local_library.volume_preview = None;
            push(app, ui, LocalAction::SetVolume(value));
        }
        SliderEvent::None => {}
    }
    let percent = (level * 100.0).round() as u32;
    let icon = match percent {
        0 => Icon::VolumeX,
        1..=33 => Icon::Volume,
        34..=66 => Icon::Volume1,
        _ => Icon::Volume2,
    };
    if theme::icon_button(
        ui,
        icon,
        18.0,
        palette.secondary,
        palette.text,
        if percent == 0 { "Unmute" } else { "Mute" },
    )
    .clicked()
    {
        push(app, ui, LocalAction::ToggleMute);
    }
}
