//! Install native manifests; this module starts no runtime or service.
use super::*;

pub(crate) fn install_host(host: &str, project: bool) -> Result<(), String> {
    match host {
        "codex" if !project => install_codex_plugin(),
        "claude-code" => install_claude_hooks(&current_executable()?, project).map(|_| ()),
        "pi" => install_pi_at(
            &pi_settings_path(project)?,
            &default_data_directory().join("pi-extension"),
            &current_executable()?,
        ),
        _ => Err(format!("unsupported native hook installation: {host}")),
    }
}

pub(crate) fn uninstall_host(host: &str) -> Result<(), String> {
    if host == "pi" {
        for project in [false, true] {
            let settings = pi_settings_path(project)?;
            if read_ownership(&ownership_path(&settings, "pi-extension"))?.is_some() {
                remove_owned_json(&settings, "pi-extension", "Pi settings")?;
            }
        }
        return Ok(());
    }
    if host == "codex" {
        let manifest_root = plugin_root()?;
        let ownership_path = codex_ownership_path();
        let ownership = read_codex_ownership(&ownership_path)?;
        let marketplaces = codex_json(&["plugin", "marketplace", "list", "--json"])?;
        let configured = marketplaces
            .get("marketplaces")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|entry| entry.get("name").and_then(Value::as_str) == Some("acyclic"));
        if let Some(configured) = configured {
            let configured_root = configured
                .get("root")
                .and_then(Value::as_str)
                .ok_or_else(|| "configured Acyclic marketplace has no root".to_owned())?;
            let owned = Path::new(configured_root)
                .canonicalize()
                .ok()
                .zip(manifest_root.canonicalize().ok())
                .is_some_and(|(configured, package)| configured == package);
            if !owned {
                return Err(format!(
                    "refusing to remove the Acyclic marketplace owned by {configured_root}"
                ));
            }
        }
        let plugin_was_installed = installed_codex_plugin()?.is_some();
        let Some(ownership) = ownership else {
            if configured.is_some() || plugin_was_installed {
                return Err(
                    "refusing to remove Codex integration without an Acyclic ownership record"
                        .to_owned(),
                );
            }
            return Ok(());
        };
        let owned_root = ownership
            .marketplace_root
            .canonicalize()
            .ok()
            .zip(manifest_root.canonicalize().ok())
            .is_some_and(|(owned, package)| owned == package);
        if ownership.version != 1 || !owned_root {
            return Err(
                "Codex integration ownership does not match this Acyclic package".to_owned(),
            );
        }
        if plugin_was_installed && !ownership.plugin_was_installed {
            run_codex(&["plugin", "remove", "acyclic@acyclic", "--json"])?;
        }
        if ownership.added_marketplace && configured.is_some() {
            run_codex(&["plugin", "marketplace", "remove", "acyclic"])?;
        }
        // Restore configuration last. Every preceding step is idempotent, so
        // an interrupted uninstall can resume from the durable ownership record.
        restore_codex_config(&ownership)?;
        fs::remove_file(&ownership_path).map_err(display)?;
        println!("removed Acyclic-owned Codex integration and preserved prior state");
        return Ok(());
    }
    if host == "claude-code" {
        let user = home_directory()?.join(".claude/settings.json");
        let project = env::current_dir()
            .map_err(display)?
            .join(".claude/settings.json");
        let same_settings = same_existing_path(&user, &project);
        remove_claude_hooks(false)?;
        if !same_settings {
            remove_claude_hooks(true)?;
        }
        println!("removed Acyclic-owned Claude Code lifecycle hooks");
        return Ok(());
    }
    Err(format!("unsupported native hook installation: {host}"))
}

pub(crate) fn same_existing_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    left.canonicalize()
        .ok()
        .zip(right.canonicalize().ok())
        .is_some_and(|(left, right)| left == right)
}

pub(crate) fn install_claude_hooks(executable: &Path, project: bool) -> Result<PathBuf, String> {
    let path = if project {
        env::current_dir()
            .map_err(display)?
            .join(".claude/settings.json")
    } else {
        home_directory()?.join(".claude/settings.json")
    };
    install_owned_json_normalized(
        &path,
        "claude-hooks",
        "Claude Code hooks",
        normalize_claude_hook_document,
        |prior| {
            let mut document = prior
                .cloned()
                .unwrap_or_else(|| json!({}))
                .as_object()
                .cloned()
                .ok_or_else(|| format!("{} must contain a JSON object", path.display()))?;
            let hooks = object_entry(&mut document, "hooks")?;
            let command = executable
                .to_str()
                .ok_or_else(|| "Claude hook executable path is not UTF-8".to_owned())?;
            for (event, generated) in acyclic_harness::external_executor::claude_hooks(command) {
                let entries = hooks
                    .entry(event.to_owned())
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .ok_or_else(|| format!("Claude Code hooks.{event} must be an array"))?;
                entries.extend(generated);
            }
            Ok(Value::Object(document))
        },
    )?;
    Ok(path)
}

pub(crate) fn normalize_claude_hook_document(
    prior: Option<&Value>,
) -> Result<Option<Value>, String> {
    let Some(prior) = prior else {
        return Ok(None);
    };
    let mut document = prior
        .as_object()
        .cloned()
        .ok_or_else(|| "Claude Code settings must contain a JSON object".to_owned())?;
    let Some(hooks) = document.get_mut("hooks") else {
        return Ok(Some(Value::Object(document)));
    };
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| "Claude Code settings hooks must contain a JSON object".to_owned())?;
    for event in [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "SubagentStart",
        "SubagentStop",
        "SessionEnd",
    ] {
        let Some(entries) = hooks.get_mut(event) else {
            continue;
        };
        let entries = entries
            .as_array_mut()
            .ok_or_else(|| format!("Claude Code hooks.{event} must be an array"))?;
        entries.retain(|entry| !is_acyclic_claude_hook_group(event, entry));
        if entries.is_empty() {
            hooks.remove(event);
        }
    }
    if hooks.is_empty() {
        document.remove("hooks");
    }
    Ok(Some(Value::Object(document)))
}

pub(crate) fn is_acyclic_claude_hook_group(event: &str, group: &Value) -> bool {
    let Some(group) = group.as_object() else {
        return false;
    };
    let expected_fields = if matches!(event, "PreToolUse" | "PostToolUse" | "PostToolUseFailure") {
        2
    } else {
        1
    };
    if group.len() != expected_fields
        || expected_fields == 2 && group.get("matcher").and_then(Value::as_str) != Some(".*")
    {
        return false;
    }
    let Some(hook) = group
        .get("hooks")
        .and_then(Value::as_array)
        .filter(|hooks| hooks.len() == 1)
        .and_then(|hooks| hooks.first())
        .and_then(Value::as_object)
    else {
        return false;
    };
    if hook.len() != 5
        || hook.get("type").and_then(Value::as_str) != Some("command")
        || hook.get("args") != Some(&json!(["__hook", "claude-code", event]))
        || hook.get("timeout").and_then(Value::as_u64)
            != Some(
                acyclic_harness::external_executor::hooks(
                    acyclic_harness::external_executor::ExternalHost::ClaudeCode,
                )
                .find(|spec| spec.event == event)
                .map_or(120, |spec| spec.timeout_seconds()),
            )
        || hook.get("statusMessage").and_then(Value::as_str)
            != Some("Acyclic is routing the workspace")
    {
        return false;
    }
    hook.get("command")
        .and_then(Value::as_str)
        .map(Path::new)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.eq_ignore_ascii_case("acyclic") || name.eq_ignore_ascii_case("acyclic.exe")
        })
}

pub(crate) fn remove_claude_hooks(project: bool) -> Result<(), String> {
    let path = if project {
        env::current_dir()
            .map_err(display)?
            .join(".claude/settings.json")
    } else {
        home_directory()?.join(".claude/settings.json")
    };
    remove_owned_json(&path, "claude-hooks", "Claude Code hooks")
}
