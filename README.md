# HLS Downloader (Tauri GUI)

Графічна обгортка над тією ж логікою, що в консольній версії: поле для URL m3u8,
поле для назви вихідного файлу, кнопка "Завантажити" і прогрес-бар.

## Передумови

- Rust (рекомендується через `rustup`, не через apt — там часто застаріла версія)
- Node.js (для збірки фронтенду/інструментів Tauri)
- Tauri CLI:
  ```bash
  cargo install tauri-cli --version "^1"
  ```
- Системні залежності (Linux): `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`,
  `libayatana-appindicator3-dev`, `librsvg2-dev` (на Windows/Mac нічого додатково
  ставити не треба, Tauri сам використовує системний WebView).

## Запуск у режимі розробки

```bash
cd hls_downloader_tauri/src-tauri
cargo tauri dev
```

Відкриється вікно застосунку. Встав URL m3u8, назву вихідного файлу — і тисни
"Завантажити". Прогрес-бар оновлюється в реальному часі через Tauri events.

## Збірка готового .exe / .app / AppImage

```bash
cargo tauri build
```

Готовий інсталятор/бінарник з'явиться в
`src-tauri/target/release/bundle/`.

## Структура проєкту

- `src-tauri/src/main.rs` — бекенд: команда `download_stream`, яка парсить
  m3u8 (включно з master playlist), качає сегменти паралельно і шле прогрес
  у фронтенд через `window.emit("progress", ...)`.
- `dist/index.html` — фронтенд: чистий HTML/CSS/JS без фреймворків,
  викликає бекенд через `window.__TAURI__.tauri.invoke`.
- `src-tauri/tauri.conf.json` — конфігурація вікна, іконки, ідентифікатора
  застосунку.

## Відома межа

Шифрування AES-128 (`#EXT-X-KEY`) і тим паче DRM (Widevine/FairPlay) тут не
підтримуються — це окремий рівень складності й технічно інша задача.
Використовуй тільки для контенту, на який маєш право (власні відео,
дозволені CDN, публічні test-стріми).
