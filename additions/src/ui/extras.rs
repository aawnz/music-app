//! The song credits window.

use egui::{CornerRadius, Frame, Margin, Stroke};

use crate::app::App;
use crate::app::extras::CreditsState;
use crate::model::{Action, Page};
use crate::theme;

pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(view) = app.extras.credits.as_ref() else {
        return;
    };
    let palette = app.palette;
    let title = view.title.clone();
    let artists = view.artists.clone();
    let mut open_artist = None;
    let mut close = false;
    let frame = Frame::new()
        .fill(palette.overlay)
        .stroke(Stroke::new(1.0, palette.outline))
        .corner_radius(CornerRadius::same(theme::RADIUS + 4))
        .inner_margin(Margin::same(24));
    let response = egui::Modal::new(egui::Id::new("song-credits"))
        .frame(frame)
        .backdrop_color(egui::Color32::from_black_alpha(if palette.dark {
            150
        } else {
            80
        }))
        .show(ctx, |ui| {
            ui.set_width(420.0);
            theme::text(ui, "Credits", theme::bold(20.0), palette.text);
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("{title} · {artists}"))
                        .font(theme::regular(14.0))
                        .color(palette.secondary),
                )
                .truncate(),
            );
            ui.add_space(14.0);
            egui::ScrollArea::vertical()
                .max_height(420.0)
                .auto_shrink([false, true])
                .show(ui, |ui| match &view.state {
                    CreditsState::Loading => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            theme::text(
                                ui,
                                "Loading credits…",
                                theme::regular(14.0),
                                palette.secondary,
                            );
                        });
                    }
                    CreditsState::Failed(error) => {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(format!("Couldn't load credits: {error}"))
                                    .font(theme::regular(14.0))
                                    .color(palette.secondary),
                            )
                            .wrap(),
                        );
                    }
                    CreditsState::Ready(credits) if credits.roles.is_empty() => {
                        theme::text(
                            ui,
                            "Spotify has no credits for this song.",
                            theme::regular(14.0),
                            palette.secondary,
                        );
                    }
                    CreditsState::Ready(credits) => {
                        for role in &credits.roles {
                            theme::text(ui, &role.title, theme::bold(15.0), palette.text);
                            ui.add_space(6.0);
                            for person in &role.people {
                                let link = person
                                    .artist_uri
                                    .as_deref()
                                    .and_then(|uri| uri.strip_prefix("spotify:artist:"));
                                let name = egui::RichText::new(&person.name)
                                    .font(theme::regular(14.0))
                                    .color(palette.text);
                                let clicked = match link {
                                    Some(_) => ui
                                        .add(
                                            egui::Label::new(name.underline())
                                                .sense(egui::Sense::click()),
                                        )
                                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                                        .clicked(),
                                    None => {
                                        ui.add(egui::Label::new(name));
                                        false
                                    }
                                };
                                if clicked {
                                    open_artist = link.map(str::to_string);
                                }
                                if !person.subroles.is_empty() {
                                    theme::text(
                                        ui,
                                        &person.subroles.join(", "),
                                        theme::regular(12.0),
                                        palette.secondary,
                                    );
                                }
                                ui.add_space(6.0);
                            }
                            ui.add_space(8.0);
                        }
                        if !credits.sources.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!(
                                        "Source: {}",
                                        credits.sources.join(", ")
                                    ))
                                    .font(theme::regular(12.0))
                                    .color(palette.secondary),
                                )
                                .wrap(),
                            );
                        }
                    }
                });
            ui.add_space(12.0);
            if theme::pill_button(ui, &palette, "Close", false).clicked() {
                close = true;
            }
        });
    if let Some(id) = open_artist {
        app.actions.push(Action::Open(Page::Artist(id)));
        close = true;
    }
    if close || response.should_close() {
        app.extras.close_credits();
    }
}
