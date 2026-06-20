#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use futures_util::stream::{FuturesOrdered, StreamExt};
use reqwest::Client;
use serde::Serialize;
use tauri::Manager;
use url::Url;

#[derive(Clone, Serialize)]
struct ProgressPayload {
    done: usize,
    total: usize,
}

fn resolve_url(base: &Url, relative: &str) -> Result<Url, url::ParseError> {
    base.join(relative)
}

enum Playlist {
    Master(Vec<(u64, String)>),
    Media(Vec<String>),
}

fn parse_playlist(text: &str) -> Playlist {
    let lines: Vec<&str> = text.lines().collect();
    let mut is_master = false;
    let mut variants = Vec::new();
    let mut segments = Vec::new();
    let mut pending_bandwidth: Option<u64> = None;

    for line in &lines {
        let line = line.trim();
        if line.starts_with("#EXT-X-STREAM-INF") {
            is_master = true;
            if let Some(idx) = line.find("BANDWIDTH=") {
                let rest = &line[idx + "BANDWIDTH=".len()..];
                let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                pending_bandwidth = num.parse::<u64>().ok();
            }
        } else if !line.starts_with('#') && !line.is_empty() {
            if is_master {
                variants.push((pending_bandwidth.unwrap_or(0), line.to_string()));
                pending_bandwidth = None;
            } else {
                segments.push(line.to_string());
            }
        }
    }

    if is_master {
        Playlist::Master(variants)
    } else {
        Playlist::Media(segments)
    }
}

fn check_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

#[tauri::command]
fn ffmpeg_available() -> bool {
    check_ffmpeg()
}

#[tauri::command]
async fn download_stream(
    window: tauri::Window,
    url: String,
    out: String,
) -> Result<String, String> {
    if !check_ffmpeg() {
        return Err(
            "ffmpeg не знайдено в PATH. Встанови його: Windows — winget install ffmpeg, \
             macOS — brew install ffmpeg, Linux — sudo apt install ffmpeg"
                .to_string(),
        );
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;

    let mut playlist_url = Url::parse(&url).map_err(|e| e.to_string())?;
    let mut playlist_text = client
        .get(playlist_url.clone())
        .send()
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;

    let segments;
    loop {
        match parse_playlist(&playlist_text) {
            Playlist::Master(variants) => {
                let best = variants
                    .iter()
                    .max_by_key(|(bw, _)| *bw)
                    .cloned()
                    .ok_or("Master playlist без варіантів якості")?;
                let next_url = resolve_url(&playlist_url, &best.1).map_err(|e| e.to_string())?;
                playlist_url = next_url;
                playlist_text = client
                    .get(playlist_url.clone())
                    .send()
                    .await
                    .map_err(|e| e.to_string())?
                    .text()
                    .await
                    .map_err(|e| e.to_string())?;
                continue;
            }
            Playlist::Media(s) => {
                segments = s;
                break;
            }
        }
    }

    let total = segments.len();
    let urls: Vec<Url> = segments
        .iter()
        .map(|s| resolve_url(&playlist_url, s))
        .collect::<Result<_, _>>()
        .map_err(|e: url::ParseError| e.to_string())?;

    let mut results: Vec<Option<Vec<u8>>> = vec![None; urls.len()];
    let mut futures = FuturesOrdered::new();
    let mut in_flight = 0usize;
    let mut done = 0usize;
    let concurrency = 8usize;

    let mut idx_queue: Vec<usize> = (0..urls.len()).collect();
    idx_queue.reverse();

    while !idx_queue.is_empty() || in_flight > 0 {
        while in_flight < concurrency && !idx_queue.is_empty() {
            let idx = idx_queue.pop().unwrap();
            let u = urls[idx].clone();
            let c = client.clone();
            futures.push_back(async move {
                let bytes = c.get(u).send().await?.bytes().await?;
                Ok::<(usize, Vec<u8>), reqwest::Error>((idx, bytes.to_vec()))
            });
            in_flight += 1;
        }

        if let Some(res) = futures.next().await {
            let (idx, data) = res.map_err(|e| e.to_string())?;
            results[idx] = Some(data);
            in_flight -= 1;
            done += 1;
            let _ = window.emit("progress", ProgressPayload { done, total });
        }
    }

    let out_path = PathBuf::from(&out);
    let mut out_file = File::create(&out_path).map_err(|e| e.to_string())?;
    for chunk in results {
        let data = chunk.ok_or("Відсутній сегмент")?;
        out_file.write_all(&data).map_err(|e| e.to_string())?;
    }
    drop(out_file);

    let _ = window.emit("status", "Конвертую в mp4...");

    let mp4_path = out_path.with_extension("mp4");
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-i",
            out_path.to_str().ok_or("Невалідний шлях")?,
            "-c",
            "copy",
            mp4_path.to_str().ok_or("Невалідний шлях")?,
        ])
        .status()
        .map_err(|e| e.to_string())?;

    if status.success() {
        Ok(format!("Готовий mp4: {}", mp4_path.display()))
    } else {
        Ok(format!(
            "ffmpeg завершився з помилкою, .ts файл залишився як є: {}",
            out_path.display()
        ))
    }
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![download_stream, ffmpeg_available])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
