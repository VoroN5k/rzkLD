#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream::{FuturesOrdered, StreamExt};
use headless_chrome::browser::tab::point::Point;
use headless_chrome::browser::tab::RequestPausedDecision;
use headless_chrome::browser::transport::{SessionId, Transport};
use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption;
use headless_chrome::protocol::cdp::Fetch::events::RequestPausedEvent;
use headless_chrome::{Browser, LaunchOptions};
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

// Фіксований розмір вікна headless-браузера — однаковий для скріншота
// і для реального кліку, інакше координати не збігатимуться
const BROWSER_WINDOW_SIZE: (u32, u32) = (1280, 800);

#[tauri::command]
async fn debug_screenshot(
    page_url: String,
    out_path: String,
    scroll_y: Option<i64>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let launch_options = LaunchOptions::default_builder()
            .window_size(Some(BROWSER_WINDOW_SIZE))
            .build()
            .map_err(|e| e.to_string())?;
        let browser = Browser::new(launch_options).map_err(|e| e.to_string())?;
        let tab = browser.new_tab().map_err(|e| e.to_string())?;

        tab.navigate_to(&page_url).map_err(|e| e.to_string())?;
        tab.wait_until_navigated().map_err(|e| e.to_string())?;
        std::thread::sleep(std::time::Duration::from_secs(2));

        if let Some(y) = scroll_y {
            if y != 0 {
                let script = format!("window.scrollTo(0, {});", y);
                tab.evaluate(&script, false).map_err(|e| e.to_string())?;
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }

        let png_bytes = tab
            .capture_screenshot(CaptureScreenshotFormatOption::Png, None, None, true)
            .map_err(|e| e.to_string())?;

        std::fs::write(&out_path, &png_bytes).map_err(|e| e.to_string())?;
        Ok(out_path)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn find_playlist_url(
    window: tauri::Window,
    page_url: String,
    button_selector: String,
    click_x: Option<f64>,
    click_y: Option<f64>,
    scroll_y: Option<i64>,
) -> Result<String, String> {
    let _ = window.emit("status", "Відкриваю сторінку плеєра...");

    let result = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let launch_options = LaunchOptions::default_builder()
            .window_size(Some(BROWSER_WINDOW_SIZE))
            .build()
            .map_err(|e| e.to_string())?;
        let browser = Browser::new(launch_options).map_err(|e| e.to_string())?;
        let tab = browser.new_tab().map_err(|e| e.to_string())?;

        let found_urls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let found_urls_clone = found_urls.clone();
        let all_urls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let all_urls_clone = all_urls.clone();

        tab.enable_fetch(None, None).map_err(|e| e.to_string())?;
        tab.enable_request_interception(Arc::new(
            move |_transport: Arc<Transport>,
                  _session_id: SessionId,
                  intercepted: RequestPausedEvent| {
                let req_url = &intercepted.params.request.url;
                if req_url.contains(".m3u8") || req_url.contains(".mpd") {
                    found_urls_clone.lock().unwrap().push(req_url.clone());
                }
                // Зберігаємо геть усе для діагностики, якщо основний пошук не спрацює
                let mut all = all_urls_clone.lock().unwrap();
                if all.len() < 60 {
                    all.push(req_url.clone());
                }
                RequestPausedDecision::Continue(None)
            },
        ))
        .map_err(|e| e.to_string())?;

        tab.navigate_to(&page_url).map_err(|e| e.to_string())?;
        tab.wait_until_navigated().map_err(|e| e.to_string())?;
        std::thread::sleep(std::time::Duration::from_secs(1));

        if let Some(y) = scroll_y {
            if y != 0 {
                let script = format!("window.scrollTo(0, {});", y);
                tab.evaluate(&script, false).map_err(|e| e.to_string())?;
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }

        if !button_selector.trim().is_empty() {
            // Звичайний шлях: шукаємо кнопку в DOM (працює тільки
            // для елементів у головному документі, не всередині iframe)
            if let Ok(button) = tab.wait_for_element(&button_selector) {
                let _ = button.click();
            }
        } else if let (Some(x), Some(y)) = (click_x, click_y) {
            // Fallback для плеєрів усередині iframe: клік по координатах
            // на екрані, незалежно від того, який документ там намальований
            std::thread::sleep(std::time::Duration::from_millis(800));
            let _ = tab.click_point(Point { x, y });
        }

        std::thread::sleep(std::time::Duration::from_secs(6));

        let urls = found_urls.lock().unwrap();
        if let Some(found) = urls.first() {
            return Ok(found.clone());
        }

        let all = all_urls.lock().unwrap();
        if all.is_empty() {
            Err("Жодного мережевого запиту взагалі не зафіксовано. Схоже, клік не потрапив на елемент — перевір координати ще раз.".to_string())
        } else {
            let sample: Vec<String> = all.iter().take(20).cloned().collect();
            Err(format!(
                "m3u8/mpd не знайдено серед {} запитів. Ось що реально побачив браузер:\n{}",
                all.len(),
                sample.join("\n")
            ))
        }
    })
    .await
    .map_err(|e| e.to_string())?;

    match &result {
        Ok(found) => {
            let _ = window.emit("status", format!("Знайдено: {}", found));
        }
        Err(e) => {
            let _ = window.emit("status", format!("Помилка пошуку: {}", e));
        }
    }

    result
}

#[tauri::command]
async fn download_stream(
    window: tauri::Window,
    url: String,
    out: String,
    out_dir: Option<String>,
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

    let out_path = match out_dir {
        Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir).join(&out),
        _ => PathBuf::from(&out),
    };
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
        .invoke_handler(tauri::generate_handler![
            download_stream,
            ffmpeg_available,
            find_playlist_url,
            debug_screenshot
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
