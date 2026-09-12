//! Launch-at-login (autostart) handling.
//!
//! All platforms apply the setting through tauri-plugin-autostart, except
//! macOS 13+ where the app registers itself as a login item via
//! `SMAppService`. The plugin's launch agent plist carries no app
//! association, so the System Settings Login Items pane attributes it to the
//! code-signing certificate's developer name instead of the app (#337).
//! `SMAppService` login items are attributed to the app bundle itself and
//! appear under "Open at Login" with the app's name and icon.

use std::sync::{Mutex, MutexGuard};

use tauri::{AppHandle, Manager};
use tauri_plugin_autostart::ManagerExt;

/// Serializes login-item application so a status read is never separated from
/// the register/unregister it decides on. Two unserialized applications can
/// interleave their check and their act and leave the OS registered against
/// the older of the two requests.
static APPLYING: Mutex<()> = Mutex::new(());

fn lock_applying() -> MutexGuard<'static, ()> {
    APPLYING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Apply an explicit autostart preference using the best mechanism for the
/// current platform.
///
/// **Blocks.** On macOS 13+ the `SMAppService` status query is a synchronous
/// XPC round-trip to the background-task management service, measured at
/// ~1.75 s on a cold launch. Never call this from a path a window paint waits
/// on; use [`reconcile_autostart`] from a worker thread instead.
///
/// Errors are logged rather than returned: the preference is re-applied on
/// every launch, so a transient failure self-heals and must not block
/// startup. This mirrors the pre-existing behavior of ignoring
/// enable()/disable() results.
pub fn apply_autostart(app: &AppHandle, enabled: bool) {
    let _serialized = lock_applying();
    apply_locked(app, enabled);
}

/// Bring the OS login item in line with the persisted `autostart_enabled`
/// preference.
///
/// The preference is read *inside* the serialization lock, so this never
/// applies a value that went stale while it waited: the persisted setting is
/// the single source of truth, and a concurrent [`apply_autostart`] from a
/// settings change cannot be overwritten by an older captured value.
///
/// **Blocks** for as long as [`apply_autostart`] does.
pub fn reconcile_autostart(app: &AppHandle) {
    let _serialized = lock_applying();
    apply_locked(app, crate::settings::get_settings(app).autostart_enabled);
}

/// Whether this install owns the machine's login item at all.
///
/// A portable copy does not. `SMAppService::mainAppService()` and the plugin's
/// launch-agent plist are both keyed on the *bundle*, never on the data
/// directory — so a portable copy applies its own `autostart_enabled` to the
/// installed app's login item and deletes the installed app's launch agent.
/// Measured: a portable copy of Sona logged "Unregistered login item via
/// SMAppService" on its first launch while the installed app's persisted
/// preference was `true`, leaving the setting and the system permanently
/// disagreeing with nothing to say so.
///
/// Portable mode already refuses host credential storage for the same reason
/// ([`crate::secrets::SecretManager::native_for_service`]); a login item is
/// host state of the same class.
///
/// A core the native shell spawned does not either. `mainAppService()` is the
/// bundle of the calling process, and inside the shell that is the helper
/// `SonaCore.app`: registering it lists a second "Sona" under Login Items and
/// starts a bare core, webview and all, at the next login. Measured
/// 2026-09-12: the dev shell's helper sat in Login Items as "sona" at
/// `Sona.app/Contents/Helpers/SonaCore.app`. The shell registers its own
/// bundle from the same persisted preference.
fn owns_host_login_item(portable: bool, native_shell: bool) -> bool {
    !portable && !native_shell
}
/// Whether macOS must keep the legacy plugin owner for this OS release.
///
/// `SMAppService` replaces the plugin's LaunchAgent on macOS 13 and later.
/// Leaving the plugin registered there keeps an alternate writer for
/// `~/Library/LaunchAgents/sona.plist`, so an upgraded app could recreate the
/// lowercase legacy entry after this module removes it. Earlier supported
/// macOS releases have no `SMAppService`, and keep the plugin path.
const fn macos_plugin_autostart_required(sm_app_service_available: bool) -> bool {
    !sm_app_service_available
}

/// Whether this process needs the plugin's managed autostart backend.
///
/// Non-macOS platforms continue to use the plugin. On macOS, only hosts that
/// lack `SMAppService` register it, which leaves a single owner on newer
/// systems while preserving the supported older-macOS behavior.
pub(crate) fn should_install_autostart_plugin() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos_plugin_autostart_required(macos::login_item_api_available())
    }

    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// The platform work itself. Callers hold [`APPLYING`].
fn apply_locked(app: &AppHandle, enabled: bool) {
    let portable = crate::portable::is_portable();
    let native_shell = app.state::<crate::cli::CliArgs>().native_socket.is_some();
    if !owns_host_login_item(portable, native_shell) {
        log::info!(
            "Leaving the login item and launch agent alone (autostart_enabled={enabled}, portable={portable}, native_shell={native_shell})"
        );
        return;
    }

    #[cfg(target_os = "macos")]
    if macos::login_item_api_available() {
        macos::remove_plugin_launch_agent(app);
        macos::set_login_item(enabled);
        return;
    }

    if !should_install_autostart_plugin() {
        // `autolaunch()` is `state::<AutoLaunchManager>()`, which panics when
        // the plugin was never registered. One predicate decides registration
        // and this reads it, rather than deriving the same answer a second time
        // from the OS check it happens to bottom out in today.
        log::warn!(
            "Autostart plugin is not registered on this host, so autostart_enabled={} cannot be applied",
            enabled
        );
        return;
    }

    let manager = app.autolaunch();
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    if let Err(e) = result {
        log::warn!(
            "Failed to apply autostart setting (enabled={}): {}",
            enabled,
            e
        );
    }
}

/// Remove artifacts the legacy fork could leave behind. The legacy macOS
/// SMAppService item belongs to another bundle and cannot be unregistered by
/// Sona; the completion prompt tells the user to remove that app instead.
pub fn remove_legacy_autostart_artifacts(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    macos::remove_legacy_fork_launch_agents(app);
    #[cfg(target_os = "windows")]
    windows::remove_legacy_run_values();
    #[cfg(target_os = "linux")]
    linux::remove_legacy_desktop_entries();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::{Path, PathBuf};

    use objc2::runtime::AnyClass;
    use objc2_service_management::{SMAppService, SMAppServiceStatus};
    use tauri::{AppHandle, Manager};

    // `tauri-plugin-autostart` 2.5.1 uses `app.package_info().name` as this
    // name. The shipped package name was lowercase, and it is an artifact we
    // must keep removing even if a future package rename changes that value.
    const LEGACY_PLUGIN_LAUNCH_AGENT_NAME: &str = "sona";

    /// `SMAppService` requires macOS 13. The ServiceManagement framework is
    /// linked unconditionally (it has existed since 10.6), so looking up the
    /// class doubles as the OS version check: present exactly when the API is
    /// usable.
    pub fn login_item_api_available() -> bool {
        AnyClass::get(c"SMAppService").is_some()
    }

    /// Register or unregister the app as a login item, skipping the call when
    /// the service is already in the requested state (unregistering a
    /// never-registered service returns an error on every launch otherwise).
    pub fn set_login_item(enabled: bool) {
        // SAFETY: apply_autostart calls this only after the SMAppService runtime-availability gate succeeds.
        let service = unsafe { SMAppService::mainAppService() };
        // SAFETY: service is the valid main-app SMAppService returned by the documented class method above.
        let status = unsafe { service.status() };

        if enabled {
            if status == SMAppServiceStatus::Enabled {
                return;
            }
            // SAFETY: the available main-app service supports this registration selector.
            match unsafe { service.registerAndReturnError() } {
                Ok(()) => log::info!("Registered login item via SMAppService"),
                // Fails in dev (no signed app bundle) and when the user has
                // switched the item off in System Settings, which apps are
                // not allowed to override.
                Err(e) => log::warn!("Failed to register login item: {}", e),
            }
        } else {
            if status == SMAppServiceStatus::NotRegistered || status == SMAppServiceStatus::NotFound
            {
                return;
            }
            // SAFETY: the available main-app service supports this unregistration selector.
            match unsafe { service.unregisterAndReturnError() } {
                Ok(()) => log::info!("Unregistered login item via SMAppService"),
                Err(e) => log::warn!("Failed to unregister login item: {}", e),
            }
        }
    }

    /// Remove the launch agent plist that tauri-plugin-autostart (via the
    /// auto-launch crate) wrote on older versions, so login doesn't start the
    /// app twice after migrating to `SMAppService`. Runs on every launch;
    /// missing file is the normal case.
    pub fn remove_plugin_launch_agent(app: &AppHandle) {
        let Ok(home) = app.path().home_dir() else {
            return;
        };
        remove_launch_agent_file(&legacy_plugin_launch_agent_path(&home));

        let package_name = &app.package_info().name;
        if package_name != LEGACY_PLUGIN_LAUNCH_AGENT_NAME {
            remove_launch_agent_file(&plugin_launch_agent_path(&home, package_name));
        }
    }

    /// Path of the plist the auto-launch crate writes:
    /// `~/Library/LaunchAgents/{app name}.plist`.
    fn plugin_launch_agent_path(home: &Path, app_name: &str) -> PathBuf {
        home.join("Library")
            .join("LaunchAgents")
            .join(format!("{}.plist", app_name))
    }

    fn legacy_plugin_launch_agent_path(home: &Path) -> PathBuf {
        plugin_launch_agent_path(home, LEGACY_PLUGIN_LAUNCH_AGENT_NAME)
    }

    fn remove_launch_agent_file(path: &Path) {
        match std::fs::remove_file(path) {
            Ok(()) => log::info!("Removed legacy autostart launch agent {:?}", path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("Failed to remove legacy launch agent {:?}: {}", path, e),
        }
    }

    pub fn remove_legacy_fork_launch_agents(app: &AppHandle) {
        let Ok(home) = app.path().home_dir() else {
            return;
        };
        for name in ["Handy", "Handy Personal"] {
            remove_launch_agent_file(&plugin_launch_agent_path(&home, name));
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Validates the assumption `login_item_api_available` rests on: the
        /// ServiceManagement framework is linked into the binary, so the
        /// class lookup finds `SMAppService` whenever the host is macOS 13+
        /// (which anything able to build this crate is).
        #[test]
        fn sm_app_service_class_resolves() {
            assert!(login_item_api_available());
        }

        #[test]
        fn launch_agent_path_matches_auto_launch_crate() {
            let path = plugin_launch_agent_path(Path::new("/Users/someone"), "Sona");
            assert_eq!(
                path,
                Path::new("/Users/someone/Library/LaunchAgents/Sona.plist")
            );
        }

        #[test]
        fn legacy_plugin_owner_is_the_lowercase_launch_agent() {
            assert_eq!(
                legacy_plugin_launch_agent_path(Path::new("/Users/someone")),
                Path::new("/Users/someone/Library/LaunchAgents/sona.plist")
            );
        }

        #[test]
        fn removes_existing_launch_agent() {
            let dir = tempfile::tempdir().unwrap();
            let plist = dir.path().join("Sona.plist");
            std::fs::write(&plist, "<plist/>").unwrap();

            remove_launch_agent_file(&plist);
            assert!(!plist.exists());
        }

        /// `remove_plugin_launch_agent` runs on every launch and the plist is
        /// normally already gone, so the absent path is the hot case: the call
        /// has to leave the directory as it found it. The neighbour is what
        /// makes "untouched" observable — `remove_legacy_fork_launch_agents`
        /// feeds the fork agents through this same function, so a call that
        /// reached past the path it was handed would delete a real one.
        #[test]
        fn missing_launch_agent_is_a_no_op() {
            let dir = tempfile::tempdir().unwrap();
            let neighbour = dir.path().join("Handy.plist");
            std::fs::write(&neighbour, "<plist/>").unwrap();
            let missing = dir.path().join("Sona.plist");

            remove_launch_agent_file(&missing);

            assert!(!missing.exists(), "the absent plist was created");
            assert!(neighbour.exists(), "a plist it was not handed was removed");
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_WRITE};
    use winreg::RegKey;

    pub fn remove_legacy_run_values() {
        let current_user = RegKey::predef(HKEY_CURRENT_USER);
        let Ok(run) = current_user.open_subkey_with_flags(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Run",
            KEY_WRITE,
        ) else {
            return;
        };
        for value in ["Handy", "Handy Personal"] {
            let _ = run.delete_value(value);
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::env;
    use std::fs;
    use std::path::PathBuf;

    pub fn remove_legacy_desktop_entries() {
        let Some(config) = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        else {
            return;
        };
        for file in ["Handy Personal.desktop", "Handy.desktop", "handy.desktop"] {
            let _ = fs::remove_file(config.join("autostart").join(file));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the guard is the *portable* and *native shell* cases,
    /// and the installed case is what makes "refused" observable rather than
    /// "always refuses": a guard that returned `false` unconditionally would
    /// disable launch-at-login for every user, which no test that only checked
    /// the refusing branches could tell apart from correct behaviour.
    ///
    /// The requested preference is deliberately not an input. Portable refuses
    /// `enabled: true` as well — registering would mint a login item pointing
    /// at the portable bundle's own path, which is host state written by a copy
    /// that may live on removable media. A native-shell core refuses it too:
    /// its own bundle is the helper, never the app the user opens.
    #[test]
    fn only_an_installed_standalone_copy_owns_the_host_login_item() {
        assert!(
            !owns_host_login_item(true, false),
            "a portable copy applied its own preference to the installed app's login item"
        );
        assert!(
            !owns_host_login_item(false, true),
            "a native-shell core registered its helper bundle as the login item"
        );
        assert!(
            owns_host_login_item(false, false),
            "an installed copy refused to manage its own login item"
        );
    }

    #[test]
    fn macos_uses_one_owner_per_os_generation() {
        assert!(
            !macos_plugin_autostart_required(true),
            "SMAppService hosts must not retain a plugin launch-agent writer"
        );
        assert!(
            macos_plugin_autostart_required(false),
            "older macOS releases still need the plugin launch-agent backend"
        );
    }
}
