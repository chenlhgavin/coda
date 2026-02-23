# coda-pm API Reference

> A template-based prompt management system using MiniJinja. Supports loading `.j2` template files from directories and rendering them with structured context data.

## Re-exports

| Item | Kind | Description |
|------|------|-------------|
| `PromptManager` | struct | Manages prompt templates using MiniJinja for rendering |
| `PromptTemplate` | struct | A named prompt template with its raw MiniJinja content |
| `PromptError` | enum | Error types for template loading, rendering, and validation |

## Modules

### `mod loader`

| Item | Kind | Description |
|------|------|-------------|
| `load_templates_from_dir` | fn | Recursively loads all `.j2` template files from a directory, deriving names from relative paths |

## Type Details

### `PromptManager`

| Method | Signature | Description |
|--------|-----------|-------------|
| `new` | `fn new() -> Self` | Creates a new empty prompt manager with no templates loaded |
| `add_template` | `fn add_template(&mut self, template: PromptTemplate) -> Result<(), PromptError>` | Registers a single template with the manager |
| `load_from_dir` | `fn load_from_dir(&mut self, dir: &Path) -> Result<(), PromptError>` | Loads all `.j2` templates from a directory recursively |
| `render` | `fn render<T: Serialize>(&self, name: &str, ctx: T) -> Result<String, PromptError>` | Renders a named template with the given context data |
| `get_template` | `fn get_template(&self, name: &str) -> Option<&PromptTemplate>` | Returns a reference to the template with the given name, if it exists |

### `PromptTemplate`

| Field/Method | Kind | Description |
|--------------|------|-------------|
| `name` | field (`String`) | Template identifier (e.g., `"init/system"`, `"run/dev_phase"`) |
| `content` | field (`String`) | Raw MiniJinja template content |
| `new` | fn | Creates a new prompt template with the given name and content |

### `PromptError`

```rust
#[non_exhaustive]
pub enum PromptError {
    TemplateNotFound(String),
    RenderError(String),
    InvalidTemplate(String),
    IoError(#[from] std::io::Error),
}
```

| Variant | Description |
|---------|-------------|
| `TemplateNotFound` | The requested template was not found by name |
| `RenderError` | An error occurred while rendering a template |
| `InvalidTemplate` | The template content is invalid (e.g., syntax error) |
| `IoError` | An I/O error occurred while reading template files |
