/// Stamp the commit this binary came from, so "which Sona is this" has an
/// answer. The version cannot say: an installed 1.1.0 bundle and a working
/// tree eight commits ahead both call themselves 1.1.0, which is how a stale
/// install gets exercised as if it were current. `unknown` when git is not
/// there to ask, as in a source tarball.
///
/// A bare sha claims more than a build script can know, and on 2026-09-03 it
/// collected: a release bundle finalised at 05:08 stamped `2c94651`, whose
/// eleven successor commits were created at 05:10 from the same working
/// tree, so the shipped code was newer than the sha it printed. A whole
/// batch of agents reasoned from that string for an hour. A stamp that is
/// sometimes right is worse than `unknown`, because it is trusted.
///
/// So the sha is now qualified by the tree it was taken from, and the two
/// halves below only work together. `-dirty` alone was tried and removed
/// once, correctly: watching only `HEAD` and `refs` re-runs this script just
/// after a commit, when the tree is clean, so the flag would have observed
/// "clean" forever and lied with confidence. Watching the sources as well is
/// what makes the observation contemporaneous with the code being compiled —
/// a build after an edit to a watched input re-samples and says `-dirty` until
/// it is committed. It costs one build-script run per such edit.
///
/// Watched, and therefore covered: the Rust sources, Swift bridges, tray
/// translations and Tauri context. The dirty check is scoped to the same
/// inputs. Native app presentation has its own build identity.
///
/// What it still cannot see: an edit made *during* a build. The sample is
/// taken once, before compilation, and the 05:08 bundle compiled for nine
/// minutes. No build script can close that; `-dirty` narrows the window from
/// "every build since the last commit" to "this build's own compile".
///
/// Only `sona --version` reads it. The update check keeps comparing bare
/// `CARGO_PKG_VERSION`, which is the number releases are ordered by.
fn emit_build_identity() {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    // `--git-path` resolves through a worktree's `.git` file, where a literal
    // `../.git/HEAD` does not exist and the stamp would freeze at whatever
    // the first build saw.
    for name in ["HEAD", "refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", name]) {
            if std::path::Path::new(&path).exists() {
                println!("cargo:rerun-if-changed={path}");
            }
        }
    }
    for sources in ["src", "swift", "tray-locales", "tauri.conf.json"] {
        println!("cargo:rerun-if-changed={sources}");
    }

    let commit = git(&["rev-parse", "--short=7", "HEAD"]).filter(|sha| !sha.is_empty());
    // Untracked source files count too: a mod-declared file is compiled
    // whether or not it has been added to git.
    let dirty = git(&[
        "status",
        "--porcelain",
        "--",
        ":(top)src-tauri/src",
        ":(top)src-tauri/swift",
        ":(top)src-tauri/tray-locales",
        ":(top)src-tauri/tauri.conf.json",
    ])
    .is_some_and(|status| !status.is_empty());
    let identity = match commit {
        Some(sha) if dirty => format!("{sha}-dirty"),
        Some(sha) => sha,
        None => "unknown".to_string(),
    };
    println!("cargo:rustc-env=SONA_BUILD_COMMIT={identity}");
}

/// Keep the core's package identity tied to its crate version. Tauri package
/// metadata falls back to Cargo while the context declares no version.
/// Declaring a different version there would split the core's CLI and update
/// comparisons from its package metadata, so stop the build instead.
fn assert_one_version_source() {
    // `tauri_build::build()` already registers this file with cargo.
    const CONFIG: &str = "tauri.conf.json";
    let Ok(config) = std::fs::read_to_string(CONFIG) else {
        return;
    };
    let Ok(config) = serde_json::from_str::<serde_json::Value>(&config) else {
        return;
    };
    let Some(declared) = config.get("version").and_then(serde_json::Value::as_str) else {
        return;
    };
    // PANIC: cargo always sets CARGO_PKG_VERSION for a build script.
    let crate_version = std::env::var("CARGO_PKG_VERSION").expect("crate version unavailable");
    assert!(
        declared == crate_version,
        "{CONFIG} declares version {declared} but the crate is {crate_version}: the \
         core's package metadata and CLI version would disagree. Remove the key \
         or make them agree."
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    emit_build_identity();
    assert_one_version_source();

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    build_apple_intelligence_bridge();

    #[cfg(target_os = "macos")]
    build_swift_capture_bridges();
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");

    generate_tray_translations()?;

    tauri_build::build();
    Ok(())
}

/// One Rust-owned locale file's flat map of tray menu strings.
fn read_locale_tray(
    path: &std::path::Path,
) -> Result<(String, std::collections::BTreeMap<String, String>), Box<dyn std::error::Error>> {
    // PANIC: checked-in locale filenames are UTF-8 BCP-47 codes.
    let lang = path.file_stem().unwrap().to_str().unwrap().to_string();
    println!("cargo:rerun-if-changed={}", path.display());
    let content = std::fs::read_to_string(path)?;
    let tray = serde_json::from_str(&content)?;
    Ok((lang, tray))
}

/// Generate tray menu translations from tray-locales/*.json.
/// The English locale defines the struct fields.
fn generate_tray_translations() -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    // cargo always sets OUT_DIR for a build script, and there is nowhere to
    // write the generated module without it.
    // PANIC: a build that cannot find its own output directory must stop.
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let locales_dir = Path::new("tray-locales");

    println!("cargo:rerun-if-changed=tray-locales");

    // Collect all locale translations
    let mut translations: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    let locale_entries = fs::read_dir(locales_dir)?;
    for entry in locale_entries {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let (lang, tray) = read_locale_tray(&path)?;
        translations.insert(lang, tray);
    }

    // English defines the schema, and without it there are no fields at all —
    // every downstream `TrayStrings` reference would fail to compile.
    // PANIC: a missing reference locale must stop the build.
    let english = translations.get("en").unwrap();
    assert!(
        !english.is_empty(),
        "English tray locale must contain menu copy"
    );
    for (lang, tray) in &translations {
        assert!(
            tray.keys().eq(english.keys()),
            "tray locale {lang} must have the same menu keys as English"
        );
        assert!(
            tray.values().all(|value| !value.trim().is_empty()),
            "tray locale {lang} must not contain blank menu copy"
        );
    }
    let fields: Vec<_> = english
        .keys()
        .map(|k| (camel_to_snake(k), k.clone()))
        .collect();

    // Generate code
    let mut out = String::from("// Auto-generated from tray-locales/*.json - do not edit\n\n");

    // Struct
    out.push_str("#[derive(Debug, Clone)]\npub struct TrayStrings {\n");
    for (rust_field, _) in &fields {
        out.push_str(&format!("    pub {rust_field}: String,\n"));
    }
    out.push_str("}\n\n");

    // Static map
    out.push_str(
        "pub static TRANSLATIONS: Lazy<HashMap<&'static str, TrayStrings>> = Lazy::new(|| {\n",
    );
    out.push_str("    let mut m = HashMap::new();\n");

    for (lang, tray) in &translations {
        out.push_str(&format!("    m.insert(\"{lang}\", TrayStrings {{\n"));
        for (rust_field, json_key) in &fields {
            let val = tray.get(json_key).map(String::as_str).unwrap_or("");
            out.push_str(&format!(
                "        {rust_field}: \"{}\".to_string(),\n",
                escape_string(val)
            ));
        }
        out.push_str("    });\n");
    }

    out.push_str("    m\n});\n");

    fs::write(Path::new(&out_dir).join("tray_translations.rs"), out)?;

    println!(
        "cargo:warning=Generated tray translations: {} languages, {} fields",
        translations.len(),
        fields.len()
    );
    Ok(())
}

fn camel_to_snake(s: &str) -> String {
    s.chars()
        .enumerate()
        .fold(String::new(), |mut acc, (i, c)| {
            if c.is_uppercase() && i > 0 {
                acc.push('_');
            }
            /* `char::to_lowercase` yields one or more chars, and the tail
             * matters: the old `.next().unwrap()` dropped every char past the
             * first, silently truncating a key whose lowercase expands. */
            acc.extend(c.to_lowercase());
            acc
        })
}

fn escape_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[cfg(target_os = "macos")]
fn build_swift_capture_bridges() {
    build_capture_bridge("swift/meeting_capture.swift", "meeting_capture");
    build_capture_bridge("swift/screen_recorder.swift", "screen_recorder");
    build_capture_bridge("swift/chat_voice.swift", "chat_voice");
    build_capture_bridge("swift/meeting_snapshot.swift", "meeting_snapshot");
    build_capture_bridge("swift/call_chat.swift", "call_chat");
    build_capture_bridge("swift/call_roster.swift", "call_roster");
    for framework in [
        "ScreenCaptureKit",
        "AVFoundation",
        "AppKit",
        "ApplicationServices",
        "CoreMedia",
        "CoreAudio",
        "AudioToolbox",
        "CoreGraphics",
        "CoreImage",
        "CoreVideo",
        "Foundation",
        "CoreFoundation",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}

#[cfg(target_os = "macos")]
fn build_capture_bridge(source: &str, library_name: &str) {
    if let Err(error) = try_build_capture_bridge(source, library_name) {
        panic!("{error}");
    }
}

#[cfg(target_os = "macos")]
fn try_build_capture_bridge(source: &str, library_name: &str) -> Result<(), String> {
    use std::env;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    println!("cargo:rerun-if-changed={source}");
    if !Path::new(source).is_file() {
        return Err(format!("Swift capture source is missing: {source}"));
    }

    let out_dir = env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| "OUT_DIR not set".to_string())?;
    let object_path = out_dir.join(format!("{library_name}.o"));
    let archive_path = out_dir.join(format!("lib{library_name}.a"));
    let sdk_path = match env::var_os("SDKROOT") {
        Some(path) => PathBuf::from(path),
        None => xcrun_path(&["--sdk", "macosx", "--show-sdk-path"], "macOS SDK")?,
    };
    let swiftc_path = match env::var_os("SWIFTC") {
        Some(path) => PathBuf::from(path),
        None => xcrun_path(&["--find", "swiftc"], "swiftc")?,
    };
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH")
        .map_err(|error| format!("target architecture unavailable: {error}"))?;
    let target = format!("{target_arch}-apple-macosx14.0");

    // swiftc and libtool run unconditionally, and this script re-runs whenever a watched source
    // changes, so an untouched bridge would be recompiled on every cargo invocation. Compare
    // timestamps: an archive newer than its source means there is nothing to do.
    let archive_is_current = std::fs::metadata(&archive_path)
        .and_then(|archive| archive.modified())
        .ok()
        .zip(
            std::fs::metadata(source)
                .and_then(|swift| swift.modified())
                .ok(),
        )
        .is_some_and(|(archive, swift)| archive >= swift);
    if !archive_is_current {
        let status = Command::new(&swiftc_path)
            .args(["-parse-as-library", "-target", &target, "-sdk"])
            .arg(&sdk_path)
            .args(["-O", "-c", source, "-o"])
            .arg(&object_path)
            .status()
            .map_err(|error| format!("Failed to invoke swiftc for capture bridge: {error}"))?;
        if !status.success() {
            return Err(format!("swiftc failed to compile capture bridge: {source}"));
        }

        let status = Command::new("libtool")
            .args(["-static", "-o"])
            .arg(&archive_path)
            .arg(&object_path)
            .status()
            .map_err(|error| format!("Failed to archive capture bridge: {error}"))?;
        if !status.success() {
            return Err(format!("libtool failed for capture bridge: {source}"));
        }
    }

    let toolchain_swift_lib = swiftc_path
        .parent()
        .and_then(|path| path.parent())
        .map(|root| root.join("lib/swift/macosx"))
        .ok_or_else(|| {
            format!(
                "Unable to determine Swift toolchain lib directory from {}",
                swiftc_path.display()
            )
        })?;
    let sdk_swift_lib = sdk_path.join("usr/lib/swift");
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static={library_name}");
    println!(
        "cargo:rustc-link-search=native={}",
        toolchain_swift_lib.display()
    );
    println!("cargo:rustc-link-search=native={}", sdk_swift_lib.display());

    Ok(())
}

#[cfg(target_os = "macos")]
fn xcrun_path(args: &[&str], name: &str) -> Result<std::path::PathBuf, String> {
    use std::process::Command;

    let output = Command::new("xcrun")
        .args(args)
        .output()
        .map_err(|error| format!("Failed to locate {name}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "Failed to locate {name}: xcrun exited with {}",
            output.status
        ));
    }

    let path = std::str::from_utf8(&output.stdout)
        .map_err(|error| format!("{name} path is not valid UTF-8: {error}"))?
        .trim();
    if path.is_empty() {
        return Err(format!("xcrun returned an empty {name} path"));
    }

    Ok(path.into())
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn build_apple_intelligence_bridge() {
    use std::env;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const REAL_SWIFT_FILE: &str = "swift/apple_intelligence.swift";
    const STUB_SWIFT_FILE: &str = "swift/apple_intelligence_stub.swift";
    const BRIDGE_HEADER: &str = "swift/apple_intelligence_bridge.h";

    println!("cargo:rerun-if-changed={REAL_SWIFT_FILE}");
    println!("cargo:rerun-if-changed={STUB_SWIFT_FILE}");
    println!("cargo:rerun-if-changed={BRIDGE_HEADER}");

    // The compiled bridge has nowhere to go without cargo's output directory.
    // PANIC: cargo always sets OUT_DIR for a build script.
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let object_path = out_dir.join("apple_intelligence.o");
    let static_lib_path = out_dir.join("libapple_intelligence.a");

    // SDKROOT/SWIFTC env-var overrides let non-Xcode toolchains (e.g. nixpkgs
    // with apple-sdk_* + standalone swift) bypass xcrun, which is Xcode-only.
    // PANIC: no macOS SDK means no Swift bridge, so the build cannot continue.
    let sdk_path = env::var("SDKROOT").unwrap_or_else(|_| {
        String::from_utf8(
            Command::new("xcrun")
                .args(["--sdk", "macosx", "--show-sdk-path"])
                .output()
                .expect("Failed to locate macOS SDK")
                .stdout,
        )
        .expect("SDK path is not valid UTF-8")
        .trim()
        .to_string()
    });

    // Check if the SDK supports FoundationModels (required for Apple Intelligence)
    let framework_path =
        Path::new(&sdk_path).join("System/Library/Frameworks/FoundationModels.framework");
    // SONA_FORCE_AI_STUB=1 is an explicit escape hatch: force the stub even when
    // the active toolchain could build the real path (e.g. to skip the Swift
    // compile, or if the auto-detection below misfires). The common CLT-only case
    // is detected automatically just below, so this flag is rarely needed.
    let force_stub = env::var("SONA_FORCE_AI_STUB").as_deref() == Ok("1");

    // Auto-detect a Command-Line-Tools-only toolchain. The CLT SDK contains
    // FoundationModels.framework, so the `framework_path.exists()` check alone
    // wrongly selects the real Swift path, which then fails to compile because
    // the CLT `swiftc` has no FoundationModelsMacros plugin (full Xcode only).
    // Detecting this lets a plain `cargo build` / `tauri dev` succeed without the
    // manual flag. Skipped when SWIFTC is overridden: that signals a custom
    // toolchain (e.g. the nixpkgs standalone-swift path supported above) whose
    // capabilities can't be inferred from `xcode-select`.
    let command_line_tools_only = env::var("SWIFTC").is_err() && is_command_line_tools_only();
    if command_line_tools_only && !force_stub {
        println!(
            "cargo:warning=Command Line Tools-only toolchain detected; Apple Intelligence \
             (FoundationModels) needs full Xcode. Falling back to stubs. Install Xcode and run \
             `sudo xcode-select -s /Applications/Xcode.app`, or set SONA_FORCE_AI_STUB=1 to \
             silence this message."
        );
    }

    let has_foundation_models = framework_path.exists() && !force_stub && !command_line_tools_only;

    let source_file = if has_foundation_models {
        println!("cargo:warning=Building with Apple Intelligence support.");
        REAL_SWIFT_FILE
    } else {
        // The SDK genuinely lacking FoundationModels is only one reason we build
        // stubs — CLT-only detection and SONA_FORCE_AI_STUB (each warned about
        // above) also land here, and for those the framework does exist. Only
        // claim it's "not found" when that's actually true.
        if framework_path.exists() {
            println!("cargo:warning=Building Apple Intelligence with stubs.");
        } else {
            println!("cargo:warning=Apple Intelligence SDK not found. Building with stubs.");
        }
        STUB_SWIFT_FILE
    };

    if !Path::new(source_file).exists() {
        panic!("Source file {} is missing!", source_file);
    }

    // See SDKROOT note above — same env-override pattern for non-Xcode toolchains.
    // PANIC: no Swift compiler means no bridge to link against.
    let swiftc_path = env::var("SWIFTC").unwrap_or_else(|_| {
        String::from_utf8(
            Command::new("xcrun")
                .args(["--find", "swiftc"])
                .output()
                .expect("Failed to locate swiftc")
                .stdout,
        )
        .expect("swiftc path is not valid UTF-8")
        .trim()
        .to_string()
    });

    // swiftc lives at <toolchain>/usr/bin/swiftc, so it always has two parents.
    // PANIC: a layout without them is not a toolchain this bridge can link to.
    let toolchain_swift_lib = Path::new(&swiftc_path)
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join("lib/swift/macosx"))
        .expect("Unable to determine Swift toolchain lib directory");
    let sdk_swift_lib = Path::new(&sdk_path).join("usr/lib/swift");

    // Use macOS 11.0 as deployment target for compatibility
    // The @available(macOS 26.0, *) checks in Swift handle runtime availability
    // Weak linking for FoundationModels is handled via cargo:rustc-link-arg below
    // A swiftc that cannot run leaves the crate with no bridge to link.
    // PANIC: stop here rather than fail obscurely at link time.
    let status = Command::new(&swiftc_path)
        .args([
            // Without this flag swiftc treats single-file input as script
            // mode and emits its own `_main` symbol into the .o, which can
            // win the link against Rust's main under some linkers (e.g.
            // open-source ld64 used in nixpkgs' Darwin stdenv), producing a
            // binary whose main() is a 5-instruction no-op that returns 0.
            // `-parse-as-library` keeps the compilation in library mode so
            // no `_main` is emitted. See:
            //   https://forums.swift.org/t/main-in-a-single-swift-file/63079
            "-parse-as-library",
            "-target",
            "arm64-apple-macosx11.0",
            "-sdk",
            &sdk_path,
            "-O",
            "-import-objc-header",
            BRIDGE_HEADER,
            "-c",
            source_file,
            "-o",
            object_path
                .to_str()
                .expect("Failed to convert object path to string"),
        ])
        .status()
        .expect("Failed to invoke swiftc for Apple Intelligence bridge");

    if !status.success() {
        panic!("swiftc failed to compile {source_file}");
    }

    // libtool is part of the same toolchain, and without the archive there is
    // nothing for `rustc-link-lib=static` to find.
    // PANIC: stop here rather than fail obscurely at link time.
    let status = Command::new("libtool")
        .args([
            "-static",
            "-o",
            static_lib_path
                .to_str()
                .expect("Failed to convert static lib path to string"),
            object_path
                .to_str()
                .expect("Failed to convert object path to string"),
        ])
        .status()
        .expect("Failed to create static library for Apple Intelligence bridge");

    if !status.success() {
        panic!("libtool failed for Apple Intelligence bridge");
    }

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=apple_intelligence");
    println!(
        "cargo:rustc-link-search=native={}",
        toolchain_swift_lib.display()
    );
    println!("cargo:rustc-link-search=native={}", sdk_swift_lib.display());
    println!("cargo:rustc-link-lib=framework=Foundation");

    if has_foundation_models {
        // Use weak linking so the app can launch on systems without FoundationModels
        println!("cargo:rustc-link-arg=-weak_framework");
        println!("cargo:rustc-link-arg=FoundationModels");
    }
}

/// Returns true when the active developer directory is the standalone Command
/// Line Tools rather than a full Xcode install.
///
/// `xcode-select -p` prints the active developer dir; the CLT install resolves
/// to a path ending in `CommandLineTools` (e.g. `/Library/Developer/CommandLineTools`),
/// whereas full Xcode resolves under `Xcode.app`. A CLT-only toolchain ships
/// FoundationModels.framework in its SDK but a `swiftc` without the
/// FoundationModelsMacros plugin, so the Apple Intelligence Swift path cannot
/// compile (issue #1448). On any error we conservatively return false so the
/// existing SDK-presence check decides.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn is_command_line_tools_only() -> bool {
    use std::process::Command;

    Command::new("xcode-select")
        .arg("-p")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|path| path.trim().ends_with("CommandLineTools"))
        .unwrap_or(false)
}
