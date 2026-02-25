//! Interactive config selection using dialoguer.
//!
//! Provides terminal UI prompts for selecting config keys and values
//! when `coda config set` is invoked without arguments.

use anyhow::{Context, Result};
use coda_core::{ConfigKeyDescriptor, ConfigValueType};
use dialoguer::{FuzzySelect, Input, Select, theme::ColorfulTheme};

/// Prompts the user to select a config key from the full schema,
/// then prompts for a value based on the key's type.
///
/// Returns `(key, value)` ready to pass to `Engine::config_set()`.
///
/// # Errors
///
/// Returns an error if the user cancels the prompt or input is invalid.
pub fn prompt_key_and_value(schema: &[ConfigKeyDescriptor]) -> Result<(String, String)> {
    let items: Vec<String> = schema
        .iter()
        .map(|d| format!("{:<36} [current: {}]", d.key, d.current_value))
        .collect();

    let selection = FuzzySelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Select config key")
        .items(&items)
        .default(0)
        .interact()
        .context("Config key selection cancelled")?;

    let descriptor = &schema[selection];
    let value = prompt_value(descriptor)?;
    Ok((descriptor.key.clone(), value))
}

/// Prompts the user to select or input a value for a specific config key.
///
/// Returns the selected/entered value as a string.
///
/// # Errors
///
/// Returns an error if the user cancels the prompt.
pub fn prompt_value(descriptor: &ConfigKeyDescriptor) -> Result<String> {
    match &descriptor.value_type {
        ConfigValueType::Enum(options) => prompt_enum(descriptor, options),
        ConfigValueType::Suggest(suggestions) => prompt_suggest(descriptor, suggestions),
        ConfigValueType::Bool => prompt_bool(descriptor),
        ConfigValueType::U32 | ConfigValueType::F64 | ConfigValueType::String => {
            prompt_input(descriptor)
        }
    }
}

/// Prompts with a fixed set of options using `Select`.
fn prompt_enum(descriptor: &ConfigKeyDescriptor, options: &[String]) -> Result<String> {
    let default_idx = options
        .iter()
        .position(|o| *o == descriptor.current_value)
        .unwrap_or(0);

    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt(format!("{} ({})", descriptor.label, descriptor.key))
        .items(options)
        .default(default_idx)
        .interact()
        .context("Value selection cancelled")?;

    Ok(options[selection].clone())
}

/// Prompts with suggestions plus a "Custom..." free-text option.
fn prompt_suggest(descriptor: &ConfigKeyDescriptor, suggestions: &[String]) -> Result<String> {
    let mut items: Vec<String> = suggestions.to_vec();
    items.push("Custom...".to_string());

    let default_idx = suggestions
        .iter()
        .position(|s| *s == descriptor.current_value)
        .unwrap_or(0);

    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt(format!("{} ({})", descriptor.label, descriptor.key))
        .items(&items)
        .default(default_idx)
        .interact()
        .context("Value selection cancelled")?;

    if selection == suggestions.len() {
        // "Custom..." selected — fall through to free text input
        prompt_input(descriptor)
    } else {
        Ok(items[selection].clone())
    }
}

/// Prompts with true/false options using `Select`.
fn prompt_bool(descriptor: &ConfigKeyDescriptor) -> Result<String> {
    let options = ["true", "false"];
    let default_idx = if descriptor.current_value == "true" {
        0
    } else {
        1
    };

    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt(format!("{} ({})", descriptor.label, descriptor.key))
        .items(&options)
        .default(default_idx)
        .interact()
        .context("Value selection cancelled")?;

    Ok(options[selection].to_string())
}

/// Prompts for free-form text input with the current value as default.
fn prompt_input(descriptor: &ConfigKeyDescriptor) -> Result<String> {
    let theme = ColorfulTheme::default();
    let mut input =
        Input::with_theme(&theme).with_prompt(format!("{} ({})", descriptor.label, descriptor.key));

    // Only set default if the current value is meaningful
    if descriptor.current_value != "(default)" {
        input = input.default(descriptor.current_value.clone());
    }

    let value: String = input.interact_text().context("Value input cancelled")?;
    Ok(value)
}
