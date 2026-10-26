//! The SoundCloud tab: search or paste a link, then play.

use egui::{Color32, CornerRadius, FontId, Rect, Sense, Vec2, pos2, vec2};

use super::widgets;
use crate::app::App;
use crate::app::local_files::soundcloud::ScAction;
use crate::theme::{self, Icon};
use crate::util;

const ROW_HEIGHT: f32 = 56.0;

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

fn push(app: &mut App, ui: &egui::Ui, action: ScAction) {
    app.local_library.soundcloud.actions.push(action);
    ui.ctx().request_repaint();
}

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let (tile, _) = ui.allocate_exact_size(Vec2::splat(120.0), Sense::hover());
        ui.painter().rect_filled(
            tile,
            CornerRadius::same(theme::RADIUS),
            Color32::from_rgb(255, 85, 0),
        );
        theme::paint_icon(ui, Icon::AudioLines, tile, 52.0, Color32::WHITE);
        ui.add_space(16.0);
        ui.vertical(|ui| {
            ui.add_space(30.0);
            theme::text(ui, "Unofficial", theme::medium(13.0), palette.secondary);
            theme::text(ui, "SoundCloud", theme::bold(34.0), palette.text);
            theme::text(
                ui,
                "Search, or paste a link to a track or playlist",
                theme::regular(13.0),
                palette.secondary,
            );
        });
    });
    ui.add_space(16.0);
    ui.horizontal(|ui| {
        let field = widgets::text_edit(
            ui,
            locale,
            egui::TextEdit::singleline(&mut app.local_library.soundcloud.query)
                .hint_text("What do you want to hear?")
                .desired_width(360.0),
        );
        let entered = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        if theme::pill_button(ui, &palette, "Search", true).clicked() || entered {
            push(app, ui, ScAction::Search);
        }
        if app.local_library.soundcloud.searching {
            ui.spinner();
        }
    });
    ui.add_space(12.0);
    if let Some(error) = app.local_library.soundcloud.error.clone() {
        ui.add(
            egui::Label::new(
                egui::RichText::new(error)
                    .font(theme::regular(14.0))
                    .color(palette.secondary),
            )
            .wrap(),
        );
        ui.add_space(8.0);
    }
    let results = app.local_library.soundcloud.results.clone();
    if results.is_empty() {
        if app.local_library.soundcloud.shown_for.is_none() {
            widgets::empty_state(
                ui,
                &palette,
                Icon::AudioLines,
                "Play SoundCloud here",
                "Songs play in the player bar, with the visualizer and the queue. Spotify's songs are a click away.",
            );
        }
        return;
    }
    let shown = results.len();
    let width = ui.available_width();
    let playing = app.local_library.soundcloud.current().map(|track| track.id);
    let loading = app.local_library.soundcloud.loading;
    for index in 0..shown {
        let track = &results[index];
        let (rect, response) = ui.allocate_exact_size(vec2(width, ROW_HEIGHT), Sense::click());
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let current = playing == Some(track.id) || loading == Some(track.id);
        if response.hovered() {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(6), palette.surface_hover);
        }
        let number =
            Rect::from_min_size(pos2(rect.left() + 4.0, rect.top()), vec2(32.0, ROW_HEIGHT));
        if loading == Some(track.id) {
            theme::spinner(
                &mut ui.new_child(egui::UiBuilder::new().max_rect(number.shrink(8.0))),
                16.0,
                palette.accent,
            );
        } else {
            ui.painter().text(
                number.center(),
                egui::Align2::CENTER_CENTER,
                (index + 1).to_string(),
                theme::regular(13.0),
                if current {
                    palette.accent
                } else {
                    palette.secondary
                },
            );
        }
        let cover = Rect::from_min_size(
            pos2(number.right() + 4.0, rect.top() + 8.0),
            Vec2::splat(40.0),
        );
        widgets::paint_cover(
            ui,
            &palette,
            track.artwork.as_deref(),
            cover,
            4.0,
            Icon::AudioLines,
            Some(app.backend.art()),
        );
        let right_width = 150.0;
        let text_left = cover.right() + 12.0;
        let text_right = (rect.right() - right_width).max(text_left + 40.0);
        paint_line(
            ui,
            &track.title,
            theme::medium(14.0),
            if current {
                palette.accent
            } else {
                palette.text
            },
            Rect::from_min_max(
                pos2(text_left, rect.top() + 8.0),
                pos2(text_right, rect.center().y),
            ),
        );
        let byline = if track.preview {
            format!("{} · preview only", track.artist)
        } else {
            track.artist.clone()
        };
        paint_line(
            ui,
            &byline,
            theme::regular(12.0),
            palette.secondary,
            Rect::from_min_max(
                pos2(text_left, rect.center().y),
                pos2(text_right, rect.bottom() - 8.0),
            ),
        );
        if let Some(plays) = track.plays {
            ui.painter().text(
                pos2(rect.right() - 70.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                format!("▶ {}", util::format_count(plays)),
                theme::regular(12.0),
                palette.secondary,
            );
        }
        ui.painter().text(
            pos2(rect.right() - 12.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            util::format_duration_ms(track.duration_ms),
            theme::regular(13.0),
            palette.secondary,
        );
        if response.double_clicked() {
            push(
                app,
                ui,
                ScAction::Play {
                    tracks: results.clone(),
                    index,
                },
            );
        }
        let mut chosen = None;
        egui::Popup::context_menu(&response)
            .frame(widgets::menu_frame(&palette))
            .show(|ui| {
                if widgets::menu_item(ui, &palette, Some(Icon::Play), "Play") {
                    chosen = Some(ScAction::Play {
                        tracks: results.clone(),
                        index,
                    });
                }
                if widgets::menu_item(ui, &palette, Some(Icon::ListEnd), "Add to queue") {
                    chosen = Some(ScAction::AddToQueue(track.clone()));
                }
                if !track.permalink.is_empty()
                    && widgets::menu_item(
                        ui,
                        &palette,
                        Some(Icon::ExternalLink),
                        "Open on SoundCloud",
                    )
                {
                    chosen = Some(ScAction::OpenInBrowser(track.permalink.clone()));
                }
            });
        if let Some(action) = chosen {
            push(app, ui, action);
        }
    }
}
