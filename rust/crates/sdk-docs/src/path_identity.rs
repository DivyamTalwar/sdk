// Compare path components explicitly so Windows Rustdoc paths can be replayed
// on any host. Device namespaces other than verbatim drive/UNC remain distinct.
pub fn components(path: &str, windows: bool) -> Option<Vec<String>> {
    let mut path = if windows {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    };
    if windows {
        if let Some(rest) = path.strip_prefix("//?/") {
            if rest
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("UNC/"))
            {
                path = format!("//{}", &rest[4..]);
            } else if rest.as_bytes().get(1) == Some(&b':') {
                path = rest.to_owned();
            }
        }
    }
    let root = if windows && path.starts_with("//") {
        "unc"
    } else if windows && path.as_bytes().get(1) == Some(&b':') {
        if path.as_bytes().get(2) == Some(&b'/') {
            "drive-root"
        } else {
            "drive-relative"
        }
    } else if path.starts_with('/') {
        "root"
    } else {
        "relative"
    };
    let mut parts = vec![root.to_owned()];
    for part in path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
    {
        if part == ".." {
            let protected = match root {
                "unc" => 3,
                "drive-root" => 2,
                _ => 1,
            };
            if parts.len() <= protected {
                return None;
            }
            parts.pop();
            continue;
        }
        if windows
            && part.contains(':')
            && !(parts.len() == 1
                && part.len() == 2
                && part.as_bytes()[0].is_ascii_alphabetic()
                && matches!(root, "drive-root" | "drive-relative"))
        {
            return None;
        }
        parts.push(part.to_owned());
    }
    Some(parts)
}

pub fn stable_filename(filename: &str, root: &str) -> String {
    let windows = [filename, root].iter().any(|value| {
        value.contains('\\') || value.as_bytes().get(1) == Some(&b':') || value.starts_with("//")
    });
    let Some(parts) = components(filename, windows) else {
        return filename.replace('\\', "/");
    };
    if matches!(parts[0].as_str(), "relative" | "drive-relative") {
        return filename.replace('\\', "/");
    }
    if let Some(rebased) = rebase(filename, root, "/<root>", windows) {
        let rebased = rebased.replace('\\', "/");
        return format!(
            "<root>/{}",
            rebased
                .strip_prefix("/<root>")
                .unwrap()
                .trim_start_matches('/')
        );
    }
    format!(
        "<external>/{}",
        parts
            .last()
            .map(String::as_str)
            .unwrap_or("generated-source")
    )
}

pub fn rebase(path: &str, old: &str, new: &str, windows: bool) -> Option<String> {
    let parts = components(path, windows)?;
    let prefix = components(old, windows)?;
    let destination = components(new, windows)?;
    if [prefix[0].as_str(), destination[0].as_str()]
        .iter()
        .any(|root| matches!(*root, "relative" | "drive-relative"))
    {
        return None;
    }
    if parts.len() < prefix.len()
        || !parts.iter().zip(&prefix).all(|(a, b)| {
            if windows {
                a.eq_ignore_ascii_case(b)
            } else {
                a == b
            }
        })
    {
        return None;
    }
    let mut result = std::path::PathBuf::from(new);
    for component in &parts[prefix.len()..] {
        result.push(component);
    }
    Some(result.to_str()?.to_owned())
}
