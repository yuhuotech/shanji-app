use crate::config::OutputConfig;
use crate::error::{AppError, Result};
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct OutputDelivery {
    pub formatted_text: String,
    pub pasted: bool,
    pub auto_paste_error: Option<String>,
}

pub fn format_output(text: &str, config: &OutputConfig) -> String {
    let mut result = text.trim().to_string();

    match config.punct_style.as_str() {
        "zh" => {
            result = result
                .replace(", ", "，")
                .replace(',', "，")
                .replace(". ", "。")
                .replace('.', "。")
                .replace("! ", "！")
                .replace('!', "！")
                .replace("? ", "？")
                .replace('?', "？")
                .replace(": ", "：")
                .replace(':', "：")
                .replace("; ", "；")
                .replace(';', "；");
        }
        "en" => {
            result = result
                .replace('，', ", ")
                .replace('。', ". ")
                .replace('！', "! ")
                .replace('？', "? ")
                .replace('：', ": ")
                .replace('；', "; ");
        }
        _ => {}
    }

    match config.append_content.as_str() {
        "space" => result.push(' '),
        "newline" => result.push('\n'),
        "double_newline" => result.push_str("\n\n"),
        _ => {}
    }

    result
}

pub fn copy_to_clipboard(text: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|e| AppError::Output(format!("Clipboard init failed: {}", e)))?;
    clipboard
        .set_text(text.to_string())
        .map_err(|e| AppError::Output(format!("Clipboard write failed: {}", e)))?;
    Ok(())
}

pub fn deliver_output(text: &str, config: &OutputConfig) -> Result<OutputDelivery> {
    let formatted_text = format_output(text, config);

    match paste_via_clipboard(&formatted_text, config.restore_clipboard) {
        Ok(()) => Ok(OutputDelivery {
            formatted_text,
            pasted: true,
            auto_paste_error: None,
        }),
        Err(err) => {
            copy_to_clipboard(&formatted_text)?;
            Ok(OutputDelivery {
                formatted_text,
                pasted: false,
                auto_paste_error: Some(err.to_string()),
            })
        }
    }
}

pub fn is_auto_paste_supported() -> bool {
    true
}

pub fn output_method_recommendation() -> &'static str {
    "已启用自动粘贴，如系统权限阻止，将自动回退为复制到剪贴板"
}

fn paste_via_clipboard(text: &str, restore_clipboard: bool) -> Result<()> {
    let backup = if restore_clipboard {
        Some(read_clipboard_text()?)
    } else {
        None
    };

    copy_to_clipboard(text)?;
    thread::sleep(Duration::from_millis(50));
    simulate_paste()?;
    thread::sleep(Duration::from_millis(120));

    if let Some(previous) = backup {
        thread::sleep(Duration::from_millis(320));
        let _ = copy_to_clipboard(&previous);
    }

    Ok(())
}

pub fn read_clipboard_text() -> Result<String> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|e| AppError::Output(format!("Clipboard init failed: {}", e)))?;
    clipboard
        .get_text()
        .map_err(|e| AppError::Output(format!("Clipboard read failed: {}", e)))
}

pub fn simulate_paste() -> Result<()> {
    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|e| AppError::Output(format!("Input init failed: {:?}", e)))?;

    #[cfg(target_os = "macos")]
    let modifier = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let modifier = Key::Control;

    enigo
        .key(modifier, Direction::Press)
        .map_err(|e| AppError::Output(format!("Modifier press failed: {:?}", e)))?;
    thread::sleep(Duration::from_millis(10));
    enigo
        .key(Key::Unicode('v'), Direction::Click)
        .map_err(|e| AppError::Output(format!("Paste key failed: {:?}", e)))?;
    thread::sleep(Duration::from_millis(10));
    enigo
        .key(modifier, Direction::Release)
        .map_err(|e| AppError::Output(format!("Modifier release failed: {:?}", e)))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_output_zh_punct() {
        let config = OutputConfig {
            restore_clipboard: false,
            append_content: "space".to_string(),
            punct_style: "zh".to_string(),
        };

        let result = format_output("Hello, world.", &config);
        assert!(result.contains('，'));
        assert!(result.contains('。'));
        assert!(result.ends_with(' '));
    }

    #[test]
    fn format_output_en_punct() {
        let config = OutputConfig {
            restore_clipboard: false,
            append_content: "newline".to_string(),
            punct_style: "en".to_string(),
        };

        let result = format_output("你好，世界。", &config);
        assert!(result.contains(", "));
        assert!(result.contains(". "));
        assert!(result.ends_with('\n'));
    }
}
