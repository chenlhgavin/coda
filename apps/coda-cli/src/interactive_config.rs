//! Interactive config selection using dialoguer.
//!
//! Provides terminal UI prompts for selecting config keys and values
//! when `coda config set` is invoked without arguments.

use anyhow::{Context, Result};
use coda_core::{ConfigKeyDescriptor, ConfigValueType, OperationSummary};
use dialoguer::{Input, Select, theme::ColorfulTheme};

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

/// Prompts the user to select an operation, then configures backend/model/effort together.
///
/// For core operations (init/plan/run), returns backend/model/effort pairs.
/// For quality phases (review/verify), first shows an enable/disable toggle,
/// then optionally continues to backend/model/effort configuration.
///
/// Returns a vec of `(key, value)` pairs to set, e.g.:
/// `[("agents.run.backend", "claude"), ("agents.run.model", "claude-opus-4-6"), ...]`
/// or `[("review.enabled", "true"), ("agents.review.backend", "codex"), ...]`
///
/// # Errors
///
/// Returns an error if the user cancels any prompt.
pub fn prompt_operation_config(ops: &[OperationSummary]) -> Result<Vec<(String, String)>> {
    let theme = ColorfulTheme::default();

    // Step 1: Select operation — show current config inline
    let items: Vec<String> = ops
        .iter()
        .map(|op| {
            let status = match op.enabled {
                Some(false) => " (off)".to_string(),
                _ => String::new(),
            };
            format!(
                "{}{:<8} [{} / {} / {}]",
                op.name, status, op.backend, op.model, op.effort,
            )
        })
        .collect();

    let op_idx = Select::with_theme(&theme)
        .with_prompt("Select operation to configure")
        .items(&items)
        .default(0)
        .interact()
        .context("Operation selection cancelled")?;

    let op = &ops[op_idx];

    // Step 2: Quality phase toggle — if this is a toggleable phase
    if let Some(currently_enabled) = op.enabled {
        return prompt_quality_phase_config(&theme, op, currently_enabled);
    }

    // Step 3: Core operation — configure backend/model/effort
    prompt_agent_config(&theme, op)
}

/// Prompts enable/disable toggle for a quality phase, then optionally
/// continues to backend/model/effort configuration.
fn prompt_quality_phase_config(
    theme: &ColorfulTheme,
    op: &OperationSummary,
    currently_enabled: bool,
) -> Result<Vec<(String, String)>> {
    let enabled_key = format!("{}.enabled", op.name);

    if currently_enabled {
        // Phase is ON — offer to configure or disable
        let items = ["Configure backend/model/effort", "Disable"];
        let choice = Select::with_theme(theme)
            .with_prompt(format!("{} is enabled", op.label))
            .items(&items)
            .default(0)
            .interact()
            .context("Toggle selection cancelled")?;

        if choice == 1 {
            // Disable
            return Ok(vec![(enabled_key, "false".to_string())]);
        }

        // Configure — continue to backend/model/effort
        let mut pairs = vec![(enabled_key, "true".to_string())];
        pairs.extend(prompt_agent_config(theme, op)?);
        Ok(pairs)
    } else {
        // Phase is OFF — offer to enable or keep disabled
        let items = ["Enable", "Keep disabled"];
        let choice = Select::with_theme(theme)
            .with_prompt(format!("{} is disabled", op.label))
            .items(&items)
            .default(0)
            .interact()
            .context("Toggle selection cancelled")?;

        if choice == 1 {
            // Keep disabled
            return Ok(vec![(enabled_key, "false".to_string())]);
        }

        // Enable — continue to backend/model/effort
        let mut pairs = vec![(enabled_key, "true".to_string())];
        pairs.extend(prompt_agent_config(theme, op)?);
        Ok(pairs)
    }
}

/// Prompts for backend, model, and effort selection for an operation.
fn prompt_agent_config(
    theme: &ColorfulTheme,
    op: &OperationSummary,
) -> Result<Vec<(String, String)>> {
    // Select backend
    let backend_default = op
        .backend_options
        .iter()
        .position(|b| *b == op.backend)
        .unwrap_or(0);

    let backend_idx = Select::with_theme(theme)
        .with_prompt("Backend")
        .items(&op.backend_options)
        .default(backend_default)
        .interact()
        .context("Backend selection cancelled")?;

    let selected_backend = &op.backend_options[backend_idx];

    // Select model — rebuild suggestions based on selected backend
    let model_suggestions = coda_core::config::model_suggestions_for_backend(selected_backend);
    let mut model_items: Vec<String> = model_suggestions.clone();
    model_items.push("Custom...".to_string());

    let model_default = model_suggestions
        .iter()
        .position(|m| *m == op.model)
        .unwrap_or(0);

    let model_idx = Select::with_theme(theme)
        .with_prompt("Model")
        .items(&model_items)
        .default(model_default)
        .interact()
        .context("Model selection cancelled")?;

    let selected_model = if model_idx == model_suggestions.len() {
        // "Custom..." selected
        let input: String = Input::with_theme(theme)
            .with_prompt("Enter custom model name")
            .interact_text()
            .context("Model input cancelled")?;
        input
    } else {
        model_items[model_idx].clone()
    };

    // Select effort — rebuild options based on selected backend
    let effort_options = coda_core::config::effort_options_for_backend(selected_backend);
    let effort_default = effort_options
        .iter()
        .position(|e| *e == op.effort)
        .unwrap_or(2); // default to "high" index

    let effort_idx = Select::with_theme(theme)
        .with_prompt("Effort")
        .items(&effort_options)
        .default(effort_default)
        .interact()
        .context("Effort selection cancelled")?;

    let selected_effort = &effort_options[effort_idx];

    Ok(vec![
        (
            format!("agents.{}.backend", op.name),
            selected_backend.clone(),
        ),
        (format!("agents.{}.model", op.name), selected_model),
        (
            format!("agents.{}.effort", op.name),
            selected_effort.clone(),
        ),
    ])
}
