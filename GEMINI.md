# GEMINI Context: 闪记 (Shanji)

## Project Overview
**闪记 (Shanji)** is a cross-platform (macOS, Windows, Linux) desktop AI voice input tool. It allows users to record audio via global hotkeys, transcribe it locally using **FunASR Paraformer** (via ONNX Runtime), optionally refine the text with an **LLM** (OpenAI-compatible API), and automatically paste the result into the active application.

### Key Technologies
- **Language**: Rust (Edition 2021)
- **UI Framework**: [Slint](https://slint.dev/) (Native desktop UI)
- **ASR Engine**: FunASR Paraformer + ONNX Runtime (`ort` crate)
- **Audio Processing**: `cpal` (capture), `rodio` (playback), `hound` (wav)
- **Persistence**: SQLite (via `rusqlite` with FTS5 for history)
- **LLM Integration**: `async-openai` (compatible with DeepSeek, Ollama, etc.)
- **Platform Integration**: `global-hotkey`, `tray-icon`, `enigo` (input simulation), `arboard` (clipboard)
- **Async Runtime**: `tokio`

## Architecture
The project is organized as a Rust workspace with three main crates:

1.  **`crates/shanji-core`**: The heart of the application. Handles ASR logic, LLM communication, configuration management, audio recording/processing, history storage, and model management. It is strictly decoupled from the UI.
2.  **`crates/shanji-platform`**: Handles system-level integration such as global hotkeys, system tray icons, and input simulation.
3.  **`crates/shanji-slint` (Package Name: `shanji-app`)**: The main entry point. Contains the Slint UI definitions (`.slint` files) and the "glue" code that binds the UI to the core logic.

## Building and Running

### Prerequisites
- Rust stable toolchain.
- System-level dependencies for audio (e.g., `alsa` on Linux) and UI.

### Key Commands
- **Run Application**: `cargo run -p shanji-app`
- **Build Release**: `cargo build -p shanji-app --release`
- **Run Tests**: `cargo test --workspace`
- **Format Code**: `cargo fmt --all`
- **Linting**: `cargo clippy`

## Development Conventions

### UI State Management
The application uses a **Snapshot/Apply** pattern to sync the Rust backend state with the Slint UI.
- `UiSnapshot`: A struct containing all data needed by the main window.
- `SettingsWindowSnapshot`: Data for the settings/history view.
- Events from the UI are handled via Slint callbacks (e.g., `on_download_model_requested`).

### Configuration
Configuration is managed in `crates/shanji-core/src/config.rs`.
- It is versioned (currently `version: 13`) and includes automated migration logic in the `migrate()` method.
- Changes to the config trigger event listeners that sync state across the app.

### ASR & Models
- Models are defined in `public/model_registry.json`.
- Supports "Streaming" (live) and "Whole" (offline refinement) Paraformer models.
- Uses VAD (Voice Activity Detection) and Punctuation models as auxiliary dependencies.

### Platform Specifics
- **macOS**: Includes specialized code for Dock icon handling and AppleScript for error dialogs.
- **Global Hotkeys**: Supports "Toggle" and "Push-to-Talk" modes.

## Project Structure
- `crates/`: Sub-crates for core, platform, and UI.
- `docs/`: Comprehensive documentation including PRD, Technical Design, and ASR implementation details.
- `public/`: Static assets, model registry, and default vocabularies.
- `scripts/`: Python and Shell scripts for model downloading, exporting, and publishing.
- `target/`: Build artifacts.
