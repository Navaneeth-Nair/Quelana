use anyhow::Result;
use clap::Parser;
use dotenv::dotenv;
use log::{error, info};
use reqwest::Client;
use serde_json::Value;
use std::env;
use std::io::{self, Write};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tempfile::NamedTempFile;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use hound;
use rdev::{listen, Event, EventType, Key};
use tray_item::TrayItem;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long)]
    api_url: Option<String>,

    #[arg(long)]
    api_key: Option<String>,

    #[arg(long, default_value_t = 400)]
    max_output_tokens: usize,
}

enum AppEvent {
    RecordRequest,
    QuitRequested,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    env_logger::init();

    let args = Args::parse();

    let api_key = args
        .api_key
        .or_else(|| env::var("GEMINI_API_KEY").ok())
        .unwrap_or_default();
    let api_url = args
        .api_url
        .or_else(|| env::var("GEMINI_API_URL").ok())
        .unwrap_or_default();

    if api_key.is_empty() || api_url.is_empty() {
        error!("Please set GEMINI_API_KEY and GEMINI_API_URL in your environment or pass --api-key/--api-url.");
        std::process::exit(1);
    }

    let client = Client::new();

    let _tray = match setup_system_tray(api_url.clone(), api_key.clone(), args.max_output_tokens) {
        Ok(tray) => Some(tray),
        Err(err) => {
            eprintln!("Tray init failed, continuing without tray: {}", err);
            None
        }
    };
    start_global_hotkey(api_url.clone(), api_key.clone(), args.max_output_tokens);

    info!("Quelana Gemini Assistant started");
    println!("Quelana Gemini Assistant (user-activated mode)");
    println!("System tray started. Use tray menu or Ctrl+Shift+R to record a question.");
    println!("Type a question and press Enter, or type 'quit' to exit.");

    loop {
        print!("Question (type message or 'r' to record): ");
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let line = line.trim();
        if line.eq_ignore_ascii_case("quit") {
            break;
        }

        let question = if line.eq_ignore_ascii_case("r") {
            println!("Recording audio for hotkey/tray mode (7 seconds)...");
            let wav_path = record_audio_timeout(7)?;
            println!("Saved recording to {}", wav_path.display());
            let whisper_cmd = env::var("WHISPER_CMD").unwrap_or_default();
            if whisper_cmd.is_empty() {
                eprintln!("WHISPER_CMD not configured. Set WHISPER_CMD in environment to a command template containing {{input}}.");
                continue;
            }
            match transcribe_with_whisper_cmd(&whisper_cmd, &wav_path) {
                Ok(t) => {
                    println!("Transcription: {}", t);
                    println!("Press Enter to accept transcript or type edits:");
                    let mut edited = String::new();
                    io::stdin().read_line(&mut edited)?;
                    let edited = edited.trim();
                    if edited.is_empty() { t } else { edited.to_string() }
                }
                Err(e) => {
                    eprintln!("Transcription failed: {}", e);
                    continue;
                }
            }
        } else if line.is_empty() {
            println!("Empty question; skipped.");
            continue;
        } else {
            line.to_string()
        };

        let response = match send_to_gemini(&client, &api_url, &api_key, &question, args.max_output_tokens).await {
            Ok(resp) => {
                println!("Gemini reply:\n{}\n", resp);
                resp
            }
            Err(err) => {
                eprintln!("Error querying Gemini: {}", err);
                continue;
            }
        };

        if let Ok(tts_cmd) = env::var("TTS_CMD") {
            if !tts_cmd.trim().is_empty() {
                println!("Actions: [p]lay TTS, [s]kip");
                print!("Choose action> ");
                io::stdout().flush()?;
                let mut action = String::new();
                io::stdin().read_line(&mut action)?;
                let action = action.trim();
                if action.eq_ignore_ascii_case("p") {
                    if let Err(e) = speak_with_tts_cmd(&tts_cmd, &response) {
                        eprintln!("TTS failed: {}", e);
                    }
                }
            }
        }
    }

    Ok(())
}

async fn send_to_gemini(
    client: &Client,
    api_url: &str,
    api_key: &str,
    prompt: &str,
    max_output_tokens: usize,
) -> Result<String> {
    let body = serde_json::json!({
        "prompt": prompt,
        "max_output_tokens": max_output_tokens,
    });

    let resp = client
        .post(api_url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?;

    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(anyhow::anyhow!("API returned {}: {}", status, text));
    }

    if let Ok(json) = serde_json::from_str::<Value>(&text) {
        if let Some(v) = json.get("response") {
            return Ok(extract_text(v));
        }
        if let Some(v) = json.get("output") {
            return Ok(extract_text(v));
        }

        if let Some(cand) = json.get("candidates") {
            if let Some(first) = cand.get(0) {
                return Ok(extract_text(first));
            }
        }

        if let Some(v) = json.get("text") {
            return Ok(extract_text(v));
        }
        if let Some(v) = json.get("answer") {
            return Ok(extract_text(v));
        }

        return Ok(serde_json::to_string_pretty(&json)?);
    }

    Ok(text)
}

fn extract_text(value: &Value) -> String {
    // If value is a string, return it. If object, try common fields.
    match value {
        Value::String(s) => s.clone(),
        Value::Object(map) => {
            if let Some(Value::String(s)) = map.get("content") {
                return s.clone();
            }
            if let Some(Value::String(s)) = map.get("text") {
                return s.clone();
            }
            if let Some(Value::String(s)) = map.get("output") {
                return s.clone();
            }
            if let Some(Value::Array(arr)) = map.get("content") {
                // join string pieces
                let mut parts = Vec::new();
                for item in arr {
                    if let Value::String(s) = item {
                        parts.push(s.clone());
                    }
                }
                return parts.join("");
            }

            serde_json::to_string(&value).unwrap_or_default()
        }
        other => other.to_string(),
    }
}

fn record_audio() -> Result<std::path::PathBuf, anyhow::Error> {
    let host = cpal::default_host();
    let device = host.default_input_device().ok_or_else(|| anyhow::anyhow!("No input device available"))?;
    let config = device.default_input_config()?;

    let sample_format = config.sample_format();
    let config: cpal::StreamConfig = config.into();

    let sample_rate = config.sample_rate.0 as u32;
    let channels = config.channels as u16;

    let temp = NamedTempFile::new()?;
    let path = temp.path().to_path_buf();
    let wav_path = path.with_extension("wav");

    let writer = Arc::new(Mutex::new(Some(hound::WavWriter::create(&wav_path, hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    })?)));

    let writer_cloned = writer.clone();

    let err_fn = |err| eprintln!("an error occurred on stream: {}", err);

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _| {
                let mut w = writer_cloned.lock().unwrap();
                if let Some(writer) = w.as_mut() {
                    for &sample in data {
                        let s = (sample * i16::MAX as f32) as i16;
                        let _ = writer.write_sample(s);
                    }
                }
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _| {
                let mut w = writer_cloned.lock().unwrap();
                if let Some(writer) = w.as_mut() {
                    for &sample in data {
                        let _ = writer.write_sample(sample);
                    }
                }
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _| {
                let mut w = writer_cloned.lock().unwrap();
                if let Some(writer) = w.as_mut() {
                    for &sample in data {
                        let s = (sample as i16).wrapping_sub(i16::MIN);
                        let _ = writer.write_sample(s);
                    }
                }
            },
            err_fn,
            None,
        )?,
        _ => return Err(anyhow::anyhow!("Unsupported sample format")),
    };

    stream.play()?;

    let mut stop = String::new();
    io::stdin().read_line(&mut stop)?;

    drop(stream);
    let mut w = writer.lock().unwrap();
    if let Some(writer) = w.as_mut() {
        writer.flush()?;
    }
    if let Some(writer) = w.take() {
        writer.finalize()?;
    }

    Ok(wav_path)
}

fn record_audio_timeout(seconds: u64) -> Result<std::path::PathBuf, anyhow::Error> {
    let host = cpal::default_host();
    let device = host.default_input_device().ok_or_else(|| anyhow::anyhow!("No input device available"))?;
    let config = device.default_input_config()?;

    let sample_format = config.sample_format();
    let config: cpal::StreamConfig = config.into();

    let sample_rate = config.sample_rate.0 as u32;
    let channels = config.channels as u16;

    let temp = NamedTempFile::new()?;
    let path = temp.path().to_path_buf();
    let wav_path = path.with_extension("wav");

    let writer = Arc::new(Mutex::new(Some(hound::WavWriter::create(&wav_path, hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    })?)));

    let writer_cloned = writer.clone();

    let err_fn = |err| eprintln!("an error occurred on stream: {}", err);

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _| {
                let mut w = writer_cloned.lock().unwrap();
                if let Some(writer) = w.as_mut() {
                    for &sample in data {
                        let s = (sample * i16::MAX as f32) as i16;
                        let _ = writer.write_sample(s);
                    }
                }
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _| {
                let mut w = writer_cloned.lock().unwrap();
                if let Some(writer) = w.as_mut() {
                    for &sample in data {
                        let _ = writer.write_sample(sample);
                    }
                }
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _| {
                let mut w = writer_cloned.lock().unwrap();
                if let Some(writer) = w.as_mut() {
                    for &sample in data {
                        let s = (sample as i16).wrapping_sub(i16::MIN);
                        let _ = writer.write_sample(s);
                    }
                }
            },
            err_fn,
            None,
        )?,
        _ => return Err(anyhow::anyhow!("Unsupported sample format")),
    };

    stream.play()?;
    thread::sleep(Duration::from_secs(seconds));
    drop(stream);

    let mut w = writer.lock().unwrap();
    if let Some(writer) = w.as_mut() {
        writer.flush()?;
    }
    if let Some(writer) = w.take() {
        writer.finalize()?;
    }

    Ok(wav_path)
}

fn transcribe_with_whisper_cmd(cmd_template: &str, input_wav: &std::path::Path) -> Result<String, anyhow::Error> {
    let cmd_str = cmd_template.replace("{input}", &input_wav.to_string_lossy());
    let mut parts = shell_words::split(&cmd_str)?;
    let prog = parts.remove(0);
    let output = Command::new(prog).args(parts).output()?;
    if !output.status.success() {
        return Err(anyhow::anyhow!("Whisper command failed: {}", String::from_utf8_lossy(&output.stderr)));
    }
    let out = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(out.trim().to_string())
}

fn speak_with_tts_cmd(cmd_template: &str, text: &str) -> Result<(), anyhow::Error> {
    let cmd_str = cmd_template.replace("{text}", text);
    let mut parts = shell_words::split(&cmd_str)?;
    if parts.is_empty() {
        return Err(anyhow::anyhow!("TTS command template is empty"));
    }
    let prog = parts.remove(0);
    let output = Command::new(prog).args(parts).output()?;
    if !output.status.success() {
        return Err(anyhow::anyhow!("TTS command failed: {}", String::from_utf8_lossy(&output.stderr)));
    }
    Ok(())
}

fn setup_system_tray(api_url: String, api_key: String, max_output_tokens: usize) -> Result<TrayItem, anyhow::Error> {
    let mut tray = TrayItem::new("Quelana", "Quelana").map_err(|err| anyhow::anyhow!("Tray init failed: {:?}", err))?;
    let api_url_clone = api_url.clone();
    let api_key_clone = api_key.clone();

    tray.add_menu_item("Record Question", move || {
        let api_url = api_url_clone.clone();
        let api_key = api_key_clone.clone();
        thread::spawn(move || {
            if let Err(err) = run_record_and_answer(&api_url, &api_key, max_output_tokens) {
                eprintln!("Tray recording failed: {}", err);
            }
        });
    }).map_err(|err| anyhow::anyhow!("Tray add item failed: {:?}", err))?;
    tray.add_menu_item("Quit", move || {
        std::process::exit(0);
    }).map_err(|err| anyhow::anyhow!("Tray add item failed: {:?}", err))?;
    Ok(tray)
}

fn start_global_hotkey(api_url: String, api_key: String, max_output_tokens: usize) {
    thread::spawn(move || {
        let mut ctrl_pressed = false;
        let mut shift_pressed = false;

        let callback = move |event: Event| {
            match event.event_type {
                EventType::KeyPress(Key::ControlLeft) | EventType::KeyPress(Key::ControlRight) => {
                    ctrl_pressed = true;
                }
                EventType::KeyRelease(Key::ControlLeft) | EventType::KeyRelease(Key::ControlRight) => {
                    ctrl_pressed = false;
                }
                EventType::KeyPress(Key::ShiftLeft) | EventType::KeyPress(Key::ShiftRight) => {
                    shift_pressed = true;
                }
                EventType::KeyRelease(Key::ShiftLeft) | EventType::KeyRelease(Key::ShiftRight) => {
                    shift_pressed = false;
                }
                EventType::KeyPress(Key::KeyR) => {
                    if ctrl_pressed && shift_pressed {
                        println!("Global hotkey pressed: recording question for 7 seconds...");
                        let api_url = api_url.clone();
                        let api_key = api_key.clone();
                        thread::spawn(move || {
                            if let Err(err) = run_record_and_answer(&api_url, &api_key, max_output_tokens) {
                                eprintln!("Hotkey recording failed: {}", err);
                            }
                        });
                    }
                }
                _ => {}
            }
        };

        if let Err(err) = listen(callback) {
            eprintln!("Error starting global hotkey listener: {:?}", err);
        }
    });
}

fn run_record_and_answer(api_url: &str, api_key: &str, max_output_tokens: usize) -> Result<()> {
    let wav_path = record_audio_timeout(7)?;
    println!("Saved recording to {}", wav_path.display());
    let whisper_cmd = env::var("WHISPER_CMD").unwrap_or_default();
    if whisper_cmd.is_empty() {
        return Err(anyhow::anyhow!("WHISPER_CMD not configured. Set WHISPER_CMD in environment."));
    }
    let transcript = transcribe_with_whisper_cmd(&whisper_cmd, &wav_path)?;
    println!("Transcription: {}", transcript);

    let rt = tokio::runtime::Runtime::new()?;
    let client = Client::new();
    let response = rt.block_on(send_to_gemini(&client, api_url, api_key, &transcript, max_output_tokens))?;
    println!("Gemini reply:\n{}\n", response);
    Ok(())
}
