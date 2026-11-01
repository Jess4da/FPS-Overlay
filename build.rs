// Embeds assets/icon.ico as resource id 1 (exe icon + tray icon) via the Windows SDK's rc.exe.
// ponytail: no rc crate; if the SDK isn't found the build still succeeds with the stock icon.
use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // Heavy providers stay unloaded until the Steam layout polls them, so the
        // FPS-only layout never maps pdh.dll/dxgi.dll/version.dll (spec: lazy resources).
        for dll in ["pdh.dll", "dxgi.dll", "version.dll"] {
            println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
        }
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
        // Common Controls v6 = modern-looking settings window (visual styles).
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bins=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' \
             version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
    }
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe not found; building without embedded icon");
        return;
    };
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let icon = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/icon.ico");
    let rc_file = out.join("app.rc");
    fs::write(
        &rc_file,
        format!(
            "1 ICON \"{}\"\n",
            icon.display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();
    let res = out.join("app.res");
    let ok = Command::new(rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg(&rc_file)
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        println!("cargo:rustc-link-arg-bins={}", res.display());
    } else {
        println!("cargo:warning=rc.exe failed; building without embedded icon");
    }
}

fn find_rc() -> Option<PathBuf> {
    let base = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut vers: Vec<_> = fs::read_dir(&base)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    vers.sort();
    vers.into_iter()
        .rev()
        .map(|v| v.join(r"x64\rc.exe"))
        .find(|p| p.exists())
}
