//! Local desktop and freedesktop icon theme facts, captured at the Linux composition seam.
use super::app_directories::DesktopResourceDirectories;
use super::linux_session_bus::{SessionBus, SessionBusError};
use spaceterm_ui::{DesktopWindowControls, DesktopWindowStyle};
use std::{
    collections::HashSet,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) fn capture(
    bus: Option<&SessionBus>,
    resources: &DesktopResourceDirectories,
) -> DesktopWindowControls {
    let (theme, icons) = bus
        .and_then(|bus| {
            bus.query(|connection| {
                let proxy = zbus::blocking::Proxy::new(
                    connection,
                    "org.freedesktop.portal.Desktop",
                    "/org/freedesktop/portal/desktop",
                    "org.freedesktop.portal.Settings",
                )
                .map_err(SessionBusError::from)?;
                let read = |key: &str| {
                    proxy
                        .call::<_, _, zbus::zvariant::OwnedValue>(
                            "Read",
                            &("org.gnome.desktop.interface", key),
                        )
                        .ok()
                        .and_then(|value| String::try_from(value).ok())
                        .unwrap_or_default()
                };
                Ok((read("gtk-theme"), read("icon-theme")))
            })
            .ok()
        })
        .unwrap_or_default();
    let style = DesktopWindowStyle::select(
        &std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        &theme,
    );
    let icons = if style == DesktopWindowStyle::Breeze {
        resources
            .kde_globals_file()
            .and_then(|file| kde_icon_theme(&file))
            .unwrap_or_else(|| "breeze".into())
    } else {
        icons
    };
    let icons = if icons.is_empty() {
        match style {
            DesktopWindowStyle::Adwaita => "Adwaita",
            DesktopWindowStyle::Breeze => "breeze",
        }
    } else {
        &icons
    };
    let roots = resources.icon_theme_roots();
    DesktopWindowControls {
        style,
        icons: [
            "window-close-symbolic",
            "window-minimize-symbolic",
            "window-maximize-symbolic",
            "window-restore-symbolic",
        ]
        .map(|name| {
            resolve_icon(&roots, icons, name, 16, &mut HashSet::new())
                .and_then(|path| read_bounded(&path, 256 * 1024).map(Arc::from))
        }),
    }
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && Path::new(name).components().count() == 1
        && !matches!(name, "." | "..")
        && !Path::new(name).is_absolute()
}

/// Reads a desktop-owned resource of at most `limit` bytes. Startup captures these resources
/// synchronously, so a FIFO or device in their place must neither stall the open waiting for a
/// writer nor stall the read waiting for more bytes: the open is nonblocking and only a regular
/// file, checked on the opened handle, is read.
fn read_bounded(path: &Path, limit: usize) -> Option<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= limit).then_some(bytes)
}

fn resolve_icon(
    roots: &[PathBuf],
    theme: &str,
    name: &str,
    size: i32,
    visited: &mut HashSet<String>,
) -> Option<PathBuf> {
    if !safe_name(theme) || !safe_name(name) || visited.len() >= 32 || !visited.insert(theme.into())
    {
        return None;
    }
    let index = roots
        .iter()
        .find_map(|root| {
            read_bounded(&root.join(theme).join("index.theme"), 256 * 1024)
                .and_then(|bytes| String::from_utf8(bytes).ok())
        })
        .unwrap_or_default();
    let mut section = "";
    let mut directories = Vec::new();
    let mut inherited = Vec::new();
    let mut properties =
        std::collections::HashMap::<&str, std::collections::HashMap<&str, &str>>::new();
    for line in index.lines().map(str::trim) {
        if line.starts_with('[') && line.ends_with(']') {
            section = &line[1..line.len() - 1];
        } else if let Some((key, value)) = line.split_once('=') {
            let (key, value) = (key.trim(), value.trim());
            if section == "Icon Theme" && matches!(key, "Directories" | "ScaledDirectories") {
                directories.extend(value.split(',').map(str::trim));
            } else if section == "Icon Theme" && key == "Inherits" {
                inherited.extend(value.split(',').map(str::trim));
            } else {
                properties.entry(section).or_default().insert(key, value);
            }
        }
    }
    let mut candidates = Vec::new();
    for directory in directories {
        if Path::new(directory).is_absolute()
            || Path::new(directory)
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            continue;
        }
        let Some(values) = properties.get(directory) else {
            continue;
        };
        let number = |key, fallback| {
            values
                .get(key)
                .and_then(|value| value.parse::<i32>().ok())
                .map(i64::from)
                .unwrap_or(fallback)
        };
        let nominal = number("Size", 16).max(1);
        let scale = number("Scale", 1).max(1);
        let (minimum, maximum) = match values.get("Type").copied().unwrap_or("Threshold") {
            "Scalable" => (number("MinSize", nominal), number("MaxSize", nominal)),
            "Fixed" => (nominal, nominal),
            _ => (
                nominal - number("Threshold", 2).max(0),
                nominal + number("Threshold", 2).max(0),
            ),
        };
        let distance = (minimum * scale - i64::from(size))
            .max(i64::from(size) - maximum * scale)
            .max(0);
        for root in roots {
            let path = root.join(theme).join(directory).join(format!("{name}.svg"));
            if path.is_file() {
                candidates.push((distance, path));
            }
        }
    }
    candidates.sort_by_key(|(distance, _)| *distance);
    if let Some((_, path)) = candidates.into_iter().next() {
        return Some(path);
    }
    inherited.push("hicolor");
    for parent in inherited {
        if let Some(path) = resolve_icon(roots, parent, name, size, visited) {
            return Some(path);
        }
    }
    roots
        .iter()
        .map(|root| root.join(format!("{name}.svg")))
        .chain(std::iter::once(
            PathBuf::from("/usr/share/pixmaps").join(format!("{name}.svg")),
        ))
        .find(|path| path.is_file())
}

fn kde_icon_theme(kde_globals: &Path) -> Option<String> {
    let contents = String::from_utf8(read_bounded(kde_globals, 256 * 1024)?).ok()?;
    let mut icons = false;
    for line in contents.lines().map(str::trim) {
        if line.starts_with('[') {
            icons = line == "[Icons]";
        }
        if icons && let Some(theme) = line.strip_prefix("Theme=") {
            return Some(theme.trim().into());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linux_native_icon_theme_uses_size_inheritance_and_cycle_protection() {
        let temporary = std::env::temp_dir().join(format!(
            "spaceterm-native-icons-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&temporary).unwrap();
        let root = temporary.as_path();
        for (theme, index) in [
            (
                "child",
                "[Icon Theme]\nDirectories=32/ui,16/ui\nInherits=parent\n[32/ui]\nSize=32\nType=Fixed\n[16/ui]\nSize=16\nType=Fixed\n",
            ),
            (
                "parent",
                "[Icon Theme]\nDirectories=symbolic/ui\nInherits=child\n[symbolic/ui]\nSize=16\nType=Scalable\nMinSize=8\nMaxSize=256\n",
            ),
        ] {
            std::fs::create_dir_all(root.join(theme)).unwrap();
            std::fs::write(root.join(theme).join("index.theme"), index).unwrap();
        }
        for directory in ["child/32/ui", "child/16/ui", "parent/symbolic/ui"] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        for directory in ["child/32/ui", "child/16/ui"] {
            std::fs::write(
                root.join(directory).join("window-close-symbolic.svg"),
                "<svg/>",
            )
            .unwrap();
        }
        std::fs::write(
            root.join("parent/symbolic/ui/window-restore-symbolic.svg"),
            "<svg/>",
        )
        .unwrap();
        let roots = [root.to_path_buf()];
        assert_eq!(
            resolve_icon(
                &roots,
                "child",
                "window-close-symbolic",
                16,
                &mut HashSet::new()
            ),
            Some(root.join("child/16/ui/window-close-symbolic.svg"))
        );
        assert_eq!(
            resolve_icon(
                &roots,
                "child",
                "window-restore-symbolic",
                16,
                &mut HashSet::new()
            ),
            Some(root.join("parent/symbolic/ui/window-restore-symbolic.svg"))
        );
        assert!(resolve_icon(&roots, "child", "missing", 16, &mut HashSet::new()).is_none());
        assert!(resolve_icon(&roots, "../parent", "missing", 16, &mut HashSet::new()).is_none());
        std::fs::create_dir_all(root.join("hicolor/16/ui")).unwrap();
        std::fs::write(
            root.join("hicolor/index.theme"),
            "[Icon Theme]\nDirectories=16/ui\n[16/ui]\nSize=16\nType=Fixed\n",
        )
        .unwrap();
        let fallback = root.join("hicolor/16/ui/window-close-symbolic.svg");
        std::fs::write(&fallback, "<svg/>").unwrap();
        assert_eq!(
            resolve_icon(
                &roots,
                "uninstalled",
                "window-close-symbolic",
                16,
                &mut HashSet::new()
            ),
            Some(fallback.clone())
        );
        assert!(read_bounded(&fallback, 4).is_none());
        std::fs::remove_dir_all(temporary).unwrap();
    }

    #[test]
    fn linux_desktop_resource_read_rejects_a_fifo_without_waiting_for_a_writer() {
        let path = std::env::temp_dir().join(format!(
            "spaceterm-desktop-resource-fifo-{}",
            std::process::id()
        ));
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: name is a live NUL-terminated path. This creates only this test's private FIFO.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (sender, receiver) = std::sync::mpsc::channel();
        let resource = path.clone();
        std::thread::spawn(move || {
            let _ = sender.send(read_bounded(&resource, 256 * 1024));
        });
        let result = receiver.recv_timeout(std::time::Duration::from_secs(2));
        std::fs::remove_file(path).unwrap();
        assert_eq!(
            result,
            Ok(None),
            "a FIFO in place of a desktop resource is rejected promptly"
        );
    }
}
