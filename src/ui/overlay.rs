use super::AppSettings;
use eframe::egui;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::settings::render_settings_ui;

enum UiMessage {
    Reply(String),
    Error(String),
    #[allow(dead_code)]
    SttUpdate(String),
}

enum ActiveTab {
    Overlay,
    Settings,
}

#[derive(Clone, Copy, PartialEq)]
enum RecordingState {
    Idle,
    Recording,
    Processing,
}

#[cfg(target_os = "windows")]
fn detect_active_meeting_window() -> Option<String> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW, IsWindowVisible};

    unsafe extern "system" fn enum_window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        if IsWindowVisible(hwnd).as_bool() {
            let mut title = vec![0u16; 512];
            let len = GetWindowTextW(hwnd, &mut title);
            
            if len > 0 {
                let title_str = String::from_utf16_lossy(&title[..len as usize]);
                let title_lower = title_str.to_lowercase();
                
                // Detect various meeting platforms - browser-based and native
                if title_lower.contains("google meet") 
                    || title_lower.contains("meet.google.com")
                    || title_lower.contains("teams") 
                    || title_lower.contains("microsoft teams")
                    || title_lower.contains("zoom")
                    || title_lower.contains("discord")
                    || title_lower.contains("webex")
                    || title_lower.contains("skype")
                    || title_lower.contains("jitsi")
                    || title_lower.contains("big blue button")
                    || title_lower.contains("whereby")
                    || title_lower.contains("appear.in")
                    || title_lower.contains("slack")
                    || title_lower.contains("meet") {
                    
                    let result_ptr = lparam.0 as *mut Option<String>;
                    if !result_ptr.is_null() {
                        *result_ptr = Some(title_str);
                        return BOOL(0); // Stop enumerating
                    }
                }
            }
        }
        BOOL(1) // Continue enumerating
    }

    unsafe {
        let mut found_meeting: Option<String> = None;
        let lparam = LPARAM(&mut found_meeting as *mut Option<String> as isize);
        let _ = EnumWindows(Some(enum_window_callback), lparam);
        found_meeting
    }
}

#[cfg(not(target_os = "windows"))]
fn detect_active_meeting_window() -> Option<String> {
    None
}

pub struct OverlayApp {
    settings: AppSettings,
    question: String,
    response: String,
    status: String,
    stt_text: String,
    recording_state: RecordingState,
    tab: ActiveTab,
    pending_reply: Option<mpsc::Receiver<UiMessage>>,
    auto_submit_deadline: Option<Instant>,
    last_submitted_question: String,
    startup_visible: bool,
    startup_until: Option<Instant>,
    api_url: String,
    api_key: String,
    max_output_tokens: usize,
    active_meeting: Option<String>,
    animation_frame: u32,
}

impl OverlayApp {
    pub fn new(settings: AppSettings, api_url: String, api_key: String, max_output_tokens: usize) -> Self {
        Self {
            settings: settings.clone(),
            question: String::new(),
            response: String::new(),
            status: "Ready to help in your meeting.".to_string(),
            stt_text: String::new(),
            recording_state: RecordingState::Idle,
            tab: ActiveTab::Overlay,
            pending_reply: None,
            auto_submit_deadline: None,
            last_submitted_question: String::new(),
            startup_visible: true,
            startup_until: Some(Instant::now() + Duration::from_secs(2)),
            api_url,
            api_key,
            max_output_tokens,
            active_meeting: detect_active_meeting_window(),
            animation_frame: 0,
        }
    }

    fn submit_question(&mut self) {
        let trimmed = self.question.trim().to_string();
        if trimmed.is_empty() {
            self.status = "Type a question first.".to_string();
            return;
        }

        if trimmed == self.last_submitted_question {
            return;
        }

        self.last_submitted_question = trimmed.clone();
        self.status = "Asking Gemini…".to_string();
        self.response.clear();

        let (tx, rx) = mpsc::channel();
        self.pending_reply = Some(rx);

        let api_url = self.api_url.clone();
        let api_key = self.api_key.clone();
        let max_output_tokens = self.max_output_tokens;
        let question = trimmed.clone();

        thread::spawn(move || {
            let client = reqwest::Client::new();
            let rt = tokio::runtime::Runtime::new().unwrap();
            match rt.block_on(crate::send_to_gemini(&client, &api_url, &api_key, &question, max_output_tokens)) {
                Ok(reply) => {
                    let _ = tx.send(UiMessage::Reply(reply));
                }
                Err(err) => {
                    let _ = tx.send(UiMessage::Error(err.to_string()));
                }
            }
        });
    }

    fn handle_pending_reply(&mut self) {
        if let Some(rx) = self.pending_reply.as_mut() {
            while let Ok(message) = rx.try_recv() {
                match message {
                    UiMessage::Reply(reply) => {
                        self.response = reply;
                        self.status = "Answer ready.".to_string();
                        self.recording_state = RecordingState::Idle;
                    }
                    UiMessage::Error(error) => {
                        self.response.clear();
                        self.status = error;
                        self.recording_state = RecordingState::Idle;
                    }
                    UiMessage::SttUpdate(text) => {
                        self.stt_text = text;
                        self.question = self.stt_text.clone();
                    }
                }
            }
        }
    }
}

impl eframe::App for OverlayApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(target_os = "windows")]
        update_window_affinity(self.settings.hide_during_screen_share);

        self.handle_pending_reply();
        self.animation_frame = self.animation_frame.wrapping_add(1);

        // Periodically check for active meeting window
        if self.animation_frame % 30 == 0 {
            self.active_meeting = detect_active_meeting_window();
        }

        let startup_expired = self.startup_until.map(|until| Instant::now() >= until).unwrap_or(true);
        if !startup_expired {
            self.startup_visible = true;
        } else if self.startup_visible {
            self.startup_visible = false;
        }

        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));

        if self.settings.auto_answer_mode && !self.question.trim().is_empty() {
            if let Some(deadline) = self.auto_submit_deadline {
                if Instant::now() >= deadline {
                    self.submit_question();
                    self.auto_submit_deadline = None;
                }
            } else {
                self.auto_submit_deadline = Some(Instant::now() + Duration::from_millis(800));
            }
        } else {
            self.auto_submit_deadline = None;
        }

        // Set background color for better visibility
        let bg_color = if self.recording_state == RecordingState::Recording {
            egui::Color32::from_rgb(255, 100, 100)
        } else {
            egui::Color32::from_rgb(20, 25, 40)
        };

        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(&ctx.style()).fill(bg_color))
            .show(ctx, |ui| {
                ui.set_width(f32::INFINITY);
                ui.set_height(f32::INFINITY);

                // Header with branding
                ui.horizontal(|ui| {
                    ui.heading("🎙️ Quelana");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("⚙️ Settings").clicked() {
                            self.tab = ActiveTab::Settings;
                        }
                    });
                });

                ui.separator();

                // Meeting status indicator
                if let Some(meeting) = &self.active_meeting {
                    ui.colored_label(egui::Color32::from_rgb(100, 200, 100), format!("✓ Active meeting: {}", meeting));
                } else {
                    ui.colored_label(egui::Color32::from_rgb(200, 150, 50), "⚠️ No meeting detected - assist anyway?");
                }

                ui.separator();

                match self.tab {
                    ActiveTab::Overlay => {
                        // STT Display - Large and visible
                        if self.recording_state == RecordingState::Recording {
                            ui.group(|ui| {
                                ui.label("🎤 LISTENING...");
                                let dots = match (self.animation_frame / 10) % 3 {
                                    0 => "●",
                                    1 => "● ●",
                                    _ => "● ● ●",
                                };
                                ui.colored_label(egui::Color32::RED, dots);
                            });
                        }

                        // STT Text Display (Real-time feedback)
                        if !self.stt_text.is_empty() {
                            ui.group(|ui| {
                                ui.label("📝 Transcription:");
                                ui.text_edit_multiline(&mut self.stt_text.clone());
                            });
                        }

                        // Question input
                        ui.label("📌 Your Question:");
                        let response = egui::TextEdit::multiline(&mut self.question)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY)
                            .hint_text("Type or use voice to ask...");
                        let response = response.show(ui);
                        
                        if response.response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            self.submit_question();
                        }

                        // Action buttons
                        ui.horizontal(|ui| {
                            let button_text = match self.recording_state {
                                RecordingState::Idle => "🎤 Start Voice Input",
                                RecordingState::Recording => "⏹️ Stop Recording",
                                RecordingState::Processing => "⏳ Processing...",
                            };

                            if ui.button(button_text).clicked() && self.recording_state != RecordingState::Processing {
                                if self.recording_state == RecordingState::Idle {
                                    self.recording_state = RecordingState::Recording;
                                } else {
                                    self.recording_state = RecordingState::Processing;
                                }
                            }

                            if ui.button("✅ Ask Gemini").clicked() && self.recording_state != RecordingState::Processing {
                                self.submit_question();
                            }

                            if self.settings.auto_answer_mode {
                                ui.colored_label(egui::Color32::from_rgb(100, 200, 100), "⚡ Auto-answer ON");
                            }
                        });

                        ui.separator();

                        // Status
                        ui.colored_label(egui::Color32::from_rgb(100, 180, 255), format!("Status: {}", &self.status));

                        // Response display
                        if !self.response.is_empty() {
                            ui.group(|ui| {
                                ui.label("💡 Suggested Answer:");
                                ui.text_edit_multiline(&mut self.response.clone());
                            });
                        }
                    }
                    ActiveTab::Settings => {
                        if let Err(err) = render_settings_ui(ui, &mut self.settings) {
                            self.status = format!("Settings save failed: {}", err);
                        }
                        if self.settings.keep_on_top {
                            ui.colored_label(egui::Color32::from_rgb(100, 200, 100), "✓ Window will stay on top");
                        }

                        // Back button
                        if ui.button("← Back to Overlay").clicked() {
                            self.tab = ActiveTab::Overlay;
                        }
                    }
                }
            });

        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

#[cfg(target_os = "windows")]
fn update_window_affinity(hide: bool) {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE};
    
    unsafe {
        let window_name: Vec<u16> = std::ffi::OsStr::new("Quelana")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
            
        if let Ok(hwnd) = FindWindowW(PCWSTR(std::ptr::null()), PCWSTR(window_name.as_ptr())) {
            if !hwnd.0.is_null() {
                let affinity = if hide {
                    WDA_EXCLUDEFROMCAPTURE
                } else {
                    WDA_NONE
                };
                let _ = SetWindowDisplayAffinity(hwnd, affinity);
            }
        }
    }
}
