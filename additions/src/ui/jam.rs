//! The Jam window: start or join a Jam, share its code, see who is here and
//! what they added.

use egui::{CornerRadius, Frame, Margin, Stroke};

use crate::app::App;
use crate::theme;

pub fn show(app: &mut App, ctx: &egui::Context) {
    if !app.local_files.jam.open {
        return;
    }
    let palette = app.palette;
    let locale = app.locale;
    let frame = Frame::new()
        .fill(palette.overlay)
        .stroke(Stroke::new(1.0, palette.outline))
        .corner_radius(CornerRadius::same(theme::RADIUS + 4))
        .inner_margin(Margin::same(24));
    let mut start = false;
    let mut join = false;
    let mut leave = false;
    let mut close = false;
    let default_name = app.jam_default_name();
    let response = egui::Modal::new(egui::Id::new("jam"))
        .frame(frame)
        .backdrop_color(egui::Color32::from_black_alpha(if palette.dark {
            150
        } else {
            80
        }))
        .show(ctx, |ui| {
            ui.set_width(380.0);
            let jam = &mut app.local_files.jam;
            theme::text(ui, "Jam", theme::bold(20.0), palette.text);
            ui.add_space(4.0);
            let note = |ui: &mut egui::Ui, text: &str| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(text)
                            .font(theme::regular(13.0))
                            .color(palette.secondary),
                    )
                    .wrap(),
                );
            };
            match jam.session.as_ref() {
                Some(session) if session.joined => {
                    let code = crate::jam::show_code(&session.code);
                    if session.host {
                        note(ui, "Share this code. Friends enter it in Jam in their Spotifast.");
                    } else {
                        note(ui, "You're listening along. Play, pause and skip work for everyone.");
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        theme::text(ui, &code, theme::bold(28.0), palette.text);
                        ui.add_space(8.0);
                        if theme::pill_button(ui, &palette, "Copy", false).clicked() {
                            ui.ctx().copy_text(code.clone());
                        }
                    });
                    ui.add_space(14.0);
                    theme::text(
                        ui,
                        &format!("Listening ({})", session.members.len()),
                        theme::bold(15.0),
                        palette.text,
                    );
                    ui.add_space(4.0);
                    for member in &session.members {
                        let label = if member.host {
                            format!("{} · host", member.name)
                        } else {
                            member.name.clone()
                        };
                        theme::text(ui, &label, theme::regular(14.0), palette.text);
                    }
                    ui.add_space(12.0);
                    theme::text(ui, "Added to the Jam", theme::bold(15.0), palette.text);
                    ui.add_space(4.0);
                    if session.queue.is_empty() {
                        note(ui, "Songs anyone adds to the queue show here.");
                    } else {
                        egui::ScrollArea::vertical()
                            .max_height(180.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for item in &session.queue {
                                    theme::text(ui, &item.title, theme::regular(14.0), palette.text);
                                    theme::text(
                                        ui,
                                        &format!("added by {}", item.by),
                                        theme::regular(12.0),
                                        palette.secondary,
                                    );
                                    ui.add_space(4.0);
                                }
                            });
                    }
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        let label = if session.host { "End Jam" } else { "Leave Jam" };
                        if theme::pill_button(ui, &palette, label, false).clicked() {
                            leave = true;
                        }
                        if theme::pill_button(ui, &palette, "Close", false).clicked() {
                            close = true;
                        }
                    });
                }
                Some(_) => {
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        theme::text(ui, "Connecting…", theme::regular(14.0), palette.secondary);
                    });
                    ui.add_space(12.0);
                    if theme::pill_button(ui, &palette, "Cancel", false).clicked() {
                        leave = true;
                    }
                }
                None => {
                    note(
                        ui,
                        "Listen together with friends who use Spotifast. Everyone hears the same song at the same time, each on their own Spotify Premium.",
                    );
                    ui.add_space(12.0);
                    theme::text(ui, "Your name", theme::semibold(13.0), palette.text);
                    ui.add_space(4.0);
                    crate::ui::widgets::text_edit(
                        ui,
                        locale,
                        egui::TextEdit::singleline(&mut jam.name)
                            .hint_text(default_name.clone())
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(12.0);
                    if theme::pill_button(ui, &palette, "Start a Jam", true).clicked() {
                        start = true;
                    }
                    ui.add_space(14.0);
                    theme::text(ui, "Or join one", theme::semibold(13.0), palette.text);
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let field = crate::ui::widgets::text_edit(
                            ui,
                            locale,
                            egui::TextEdit::singleline(&mut jam.code_input)
                                .hint_text("Code, like K7Q-4PM")
                                .desired_width(180.0),
                        );
                        let entered = field.lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter));
                        if theme::pill_button(ui, &palette, "Join", false).clicked() || entered {
                            join = true;
                        }
                    });
                    if let Some(error) = &jam.error {
                        ui.add_space(10.0);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(error)
                                    .font(theme::regular(13.0))
                                    .color(palette.danger),
                            )
                            .wrap(),
                        );
                    }
                    ui.add_space(16.0);
                    if theme::pill_button(ui, &palette, "Close", false).clicked() {
                        close = true;
                    }
                }
            }
        });
    if start {
        app.jam_start(ctx);
    }
    if join {
        app.jam_join(ctx);
    }
    if leave {
        app.jam_leave();
    }
    if close || response.should_close() {
        app.local_files.jam.open = false;
    }
}
