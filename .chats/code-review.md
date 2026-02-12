# codex code review skill

帮我构建一个使用OpenAI Codex CLI做代码审查的skill，放在.cursor/skills/codex-code-review 下面，要求：

- 确保使用codex headless前授予所有必要的权限。
- 总是做必要的网络搜索以避免误报，例如涉及依赖版本、最佳实践等。
- 支持审查git staged changes、指定的文件、整个目录、git diffs。
- 当用户请求代码审查、分析diffs/PRs，需要安全审计、性能/架构分析，或者需要自动化代码质量回馈时使用。
- 需要考虑架构和设计的最佳实践，要有清晰的接口设计，考虑一定程度的可扩展性，符合KISS ，RY, YAGNI, SOLID, etc.
