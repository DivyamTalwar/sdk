use super::{CommandAction, CommandCatalog, CommandSpec};
use serde::{Deserialize, Serialize};

/// One completion replaces the supplied composer prefix, including its slash.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandCompletion {
    /// Display text; direct tools carry the `[tool]` suffix.
    pub label: String,
    /// Catalog-owned help text.
    pub description: String,
    /// Exact composer replacement; tools and theme append a space.
    pub replacement: String,
}

impl CommandCatalog {
    /// Composer completions, including aliases and configured theme names.
    /// Non-slash input and arguments other than theme names have no completions.
    pub fn complete_input(&self, input: &str) -> Vec<CommandCompletion> {
        let Some(command) = input.strip_prefix('/') else {
            return Vec::new();
        };
        if let Some(end) = command.find(char::is_whitespace) {
            let Some(spec) = self.lookup(&command[..end]) else {
                return Vec::new();
            };
            if !matches!(spec.action, CommandAction::Theme) {
                return Vec::new();
            }
            let prefix = command[end..].trim_start().to_lowercase();
            return self
                .themes
                .iter()
                .filter(|name| name.to_lowercase().starts_with(&prefix))
                .map(|name| CommandCompletion {
                    label: name.clone(),
                    description: "Switch theme".into(),
                    replacement: format!("/{} {name}", spec.name),
                })
                .collect();
        }
        self.complete(command)
            .map(|spec| {
                let tool = matches!(spec.action, CommandAction::Tool { .. });
                let space = tool || matches!(spec.action, CommandAction::Theme);
                CommandCompletion {
                    label: format!("/{}{}", spec.name, if tool { " [tool]" } else { "" }),
                    description: spec.description.clone(),
                    replacement: format!("/{}{}", spec.name, if space { " " } else { "" }),
                }
            })
            .collect()
    }

    /// A palette searches the same names, aliases and help used by dispatch.
    pub fn palette(&self, filter: &str) -> CommandPalette<'_> {
        let mut palette = CommandPalette {
            catalog: self,
            entries: Vec::new(),
            selected: 0,
        };
        palette.filter(filter);
        palette
    }
}

/// Borrowed renderer-neutral palette state. Selection wraps over its matches.
#[derive(Debug)]
pub struct CommandPalette<'a> {
    catalog: &'a CommandCatalog,
    entries: Vec<&'a CommandSpec>,
    selected: usize,
}

impl<'a> CommandPalette<'a> {
    /// Reset selection and match a substring of names, aliases or descriptions.
    pub fn filter(&mut self, filter: &str) {
        let filter = filter.to_lowercase();
        self.entries = self
            .catalog
            .complete("")
            .filter(|spec| {
                spec.name.to_lowercase().contains(&filter)
                    || spec
                        .aliases
                        .iter()
                        .any(|alias| alias.to_lowercase().contains(&filter))
                    || spec.description.to_lowercase().contains(&filter)
            })
            .collect();
        self.selected = 0;
    }

    /// Ordered matches; the selected item borrows this same catalog data.
    pub fn entries(&self) -> &[&'a CommandSpec] {
        &self.entries
    }

    /// Current selection, absent for an empty result set.
    pub fn selected(&self) -> Option<&'a CommandSpec> {
        self.entries.get(self.selected).copied()
    }

    /// Move forward, wrapping to the first result.
    pub fn next(&mut self) {
        self.selected = if self.selected + 1 >= self.entries.len() {
            0
        } else {
            self.selected + 1
        };
    }

    /// Move backward, wrapping to the last result.
    pub fn previous(&mut self) {
        self.selected = if self.selected == 0 {
            self.entries.len().saturating_sub(1)
        } else {
            self.selected - 1
        };
    }
}
