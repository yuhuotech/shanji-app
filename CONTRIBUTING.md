# Contributing to Shanji

感谢你对闪记的关注。这个项目是一个 Rust + Slint 原生桌面应用，欢迎通过 issue、discussion 或 pull request 参与改进。

## How to contribute

### 1. Report bugs

提交 issue 时，尽量包含：

- 操作系统版本
- Rust 版本
- 闪记版本或提交号
- 复现步骤
- 实际结果与预期结果
- 相关日志、截图或录屏

### 2. Propose features

如果你想增加新功能，建议先说明：

- 使用场景
- 交互预期
- 是否影响音频、快捷键、托盘、权限或模型下载
- 是否需要跨平台支持

### 3. Submit code

1. Fork repository.
2. Create a feature branch.
3. Make focused changes.
4. Run formatting and tests.
5. Open a pull request with a clear description.

## Development workflow

Recommended commands:

```bash
cargo fmt --all
cargo check
cargo test -p shanji-core
cargo test -p shanji-platform
cargo test -p shanji-app
```

If your change touches model download or packaging, also review:

- `scripts/README.md`
- `public/model_registry.json`
- `docs/technical-design.md`

## Code style

- Use idiomatic Rust and `rustfmt`
- Keep business logic in `shanji-core` when possible
- Avoid coupling platform behavior directly to UI code
- Prefer small, reviewable pull requests

## Areas that need care

Changes in these areas often require extra verification:

- Audio capture and device enumeration
- Global hotkeys
- Clipboard and input simulation
- Tray behavior
- Model download and registry layout
- LLM provider and prompt configuration

## Pull request checklist

- Code compiles locally
- Tests pass for touched crates
- UI changes have screenshots or a short video
- Platform-specific behavior is described clearly
- User-visible changes are documented

## Communication

Please keep discussions factual and concise. If a change affects a specific platform, say so explicitly.
