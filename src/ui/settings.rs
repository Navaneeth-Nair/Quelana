use super::{AppSettings, save_settings};
use anyhow::Result;

pub fn render_settings_ui(ui: &mut eframe::egui::Ui, settings: &mut AppSettings) -> Result<()> {
    ui.heading("Settings");
    ui.separator();

    ui.checkbox(&mut settings.auto_answer_mode, "Auto-answer mode");
    ui.label("When enabled, the assistant will submit your question automatically after a short pause.");

    ui.checkbox(&mut settings.keep_on_top, "Keep window on top");
    ui.label("This makes the overlay stay visible above your meeting app.");

    ui.checkbox(&mut settings.hide_during_screen_share, "Hide during screen share");
    ui.label("The overlay will hide itself when a Google Meet, Teams, or Discord window is active for sharing.");

    ui.separator();
    if ui.button("Save settings").clicked() {
        save_settings(settings)?;
    }

    Ok(())
}
